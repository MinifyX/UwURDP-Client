//! Signing in to a UwULock account, from the email and master password to the
//! space key.
//!
//! The way down is Bitwarden's, then UwULock's own:
//!
//! 1. prelogin: how the master key is derived for this email;
//! 2. the master key, and its hash as the password of the token request —
//!    with `scope=uwu.suite` and this app's `client_id`, so the token opens
//!    this app's space and nothing else; two-step login as every Bitwarden
//!    client does it;
//! 3. the user key, opened with the master key; the account's private key,
//!    opened with the user key;
//! 4. the extras key (`GET /uwu/v1/keys`), opened with the user key — or with
//!    the private key after an official client rotated the user key, and then
//!    wrapped again for the new one — or made, by the first UwU app that
//!    needs it;
//! 5. the space: taken if there is one, made with a fresh key if not. A
//!    device that used another space on this account before asks first (see
//!    [`SignInOutcome::SpaceChanged`]), and never takes one it left.
//!
//! Only the last step's result stays on this device. The master key, the
//! user key, the private key and the extras key are dropped (and wiped) when
//! this returns.

use super::api::{Call, Lock, LockDevice, TokenOutcome};
use super::{LockError, SPACE};
use crate::engine::TransportError;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;
use uwulock_core::crypto::{self, EncString, Kdf, PrivateKey, SymmetricKey};
use uwulock_core::extras::{self, Resolved, SpaceKey};
use uwurdp_store::{Space, Store};
use zeroize::Zeroizing;

/// What the person typed.
pub struct SignIn<'a> {
    pub server_url: &'a str,
    pub email: &'a str,
    pub password: &'a str,
    /// The code of a second step, once the server asked for one.
    pub two_factor: Option<TwoFactorAnswer>,
    /// A "remember this device" token from an earlier two-step login — to
    /// this server and this account only, which issued it: any other server
    /// could log in to that account as this device with it.
    pub remember_token: Option<&'a str>,
    /// What this device learnt about the account at earlier sign-ins.
    pub known: Known,
    /// The person agreed to take this space instead of the known one, after
    /// [`SignInOutcome::SpaceChanged`].
    pub accept_space: Option<AcceptSpace>,
}

/// A space other than the one this device used, agreed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptSpace {
    /// The space the server listed, by its id.
    Listed(Uuid),
    /// None was listed: make a new one.
    New,
}

/// What this device learnt about an account at earlier sign-ins, from
/// [`known`].
#[derive(Debug, Clone, Default)]
pub struct Known {
    /// The key derivation the last sign-in used. A server asking for a
    /// weaker one now is refused before anything is derived or sent: the
    /// hash it would get is that much cheaper to guess the master password
    /// from — and that password opens the whole UwULock vault.
    pub kdf: Option<Kdf>,
    /// The space this device used on the account.
    pub space: Option<Uuid>,
    /// The spaces it used before and moved on from.
    pub left_spaces: Vec<Uuid>,
}

/// What this device learnt about the account of this server and email.
/// Nothing for an address that is none: the sign-in says so.
pub fn known(store: &Store, server_url: &str, email: &str) -> Result<Known, LockError> {
    let Ok(server_url) = super::normalize_server(server_url) else {
        return Ok(Known::default());
    };
    let account = store.lock_account(&server_url, email)?;
    let kdf = account
        .kdf
        .map(|text| serde_json::from_str(&text))
        .transpose()
        .map_err(|_| LockError::Crypto("the key derivation kept here is unreadable".into()))?;
    Ok(Known {
        kdf,
        space: account.space_id,
        left_spaces: account.left_spaces,
    })
}

/// The "remember this device" token this device holds for the account of
/// this server and email, opened with `unprotect`: what lets a sign-in skip
/// the second step. It is looked up by the server's normalized address, so
/// it only ever goes back to the server that issued it — never to another,
/// and none for an address that is none. Kept until the account is signed
/// out of; a session that ended or a locked vault leaves it as it is.
pub fn remember_token(
    store: &Store,
    server_url: &str,
    email: &str,
    unprotect: impl Fn(&[u8]) -> std::io::Result<Zeroizing<Vec<u8>>>,
) -> Result<Option<Zeroizing<String>>, LockError> {
    let Ok(server_url) = super::normalize_server(server_url) else {
        return Ok(None);
    };
    Ok(store.lock_remember_token(&server_url, email, unprotect)?)
}

/// A server that asks for a cheaper key derivation than this account used
/// last time is refused, as the UwULock app refuses it. A first sign-in
/// takes what the server says, within the floors and ceilings of the
/// prelogin.
fn refuse_weaker_kdf(kdf: Kdf, known: &Known) -> Result<(), LockError> {
    match known.kdf {
        Some(before) if kdf.is_weaker_than(&before) => Err(LockError::WeakerKdf(format!(
            "the server asks for {} where this account's last sign-in used {}",
            kdf_words(kdf),
            kdf_words(before)
        ))),
        _ => Ok(()),
    }
}

fn kdf_words(kdf: Kdf) -> String {
    match kdf {
        Kdf::Pbkdf2 { iterations } => format!("PBKDF2 with {iterations} iterations"),
        Kdf::Argon2id {
            iterations,
            memory_mib,
            parallelism,
        } => format!("Argon2id with {iterations} passes, {memory_mib} MiB and {parallelism} lanes"),
    }
}

/// A code for two-step login.
#[derive(Debug, Clone)]
pub struct TwoFactorAnswer {
    /// Bitwarden's provider number.
    pub provider: u8,
    pub code: String,
    /// Ask for a token that skips the second step on this device next time.
    pub remember: bool,
}

/// One way of two-step login the account has set up.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TwoFactorMethod {
    pub provider: u8,
    /// `authenticator`, `email`, `yubikey`, `duo`, `webauthn`, …
    pub kind: &'static str,
    /// Whether a code typed here can do it.
    pub supported: bool,
    /// For email codes: the masked address the code goes to.
    pub hint: Option<String>,
}

fn two_factor_kind(provider: u8) -> (&'static str, bool) {
    match provider {
        0 => ("authenticator", true),
        1 => ("email", true),
        2 | 6 => ("duo", false),
        3 => ("yubikey", true),
        4 => ("u2f", false),
        7 => ("webauthn", false),
        _ => ("other", false),
    }
}

pub enum SignInOutcome {
    SignedIn(Box<SignedIn>),
    /// The account has two-step login: ask for a code and sign in again.
    /// `message` says why when a code was given and did not count (a server
    /// that asks again). UwULock refuses a wrong code the way it refuses a
    /// wrong password instead: [`LockError::WrongPassword`] with its words.
    TwoFactor {
        methods: Vec<TwoFactorMethod>,
        message: Option<String>,
    },
    /// The account's space is not the one this device used there: another
    /// device gave it a new key (a new key means a new id), it was deleted
    /// (`now` is `None`), or the server is not honest. Nothing was taken or
    /// written; ask the person, and sign in again with
    /// [`SignIn::accept_space`] if they agree.
    SpaceChanged {
        was: Uuid,
        now: Option<Uuid>,
        /// Asked for with the code: keep it for this server and account, or
        /// the next try needs a code again.
        remember_token: Option<Zeroizing<String>>,
    },
}

/// A device signed in, with the space it syncs.
pub struct SignedIn {
    /// Signed in, with the space set.
    pub lock: Lock,
    pub space: Space,
    /// This sign-in made the space: the account had no UwURDP data yet.
    pub made_space: bool,
    /// Normalized, as [`super::normalize_server`] makes it.
    pub server_url: String,
    /// In lower case, as the server compares it.
    pub email: String,
    pub refresh_token: Option<Zeroizing<String>>,
    /// Asked for with the code, to skip the second step next time.
    pub remember_token: Option<Zeroizing<String>>,
    /// The key derivation this sign-in used.
    pub kdf: Kdf,
}

impl SignedIn {
    /// The key derivation, as [`uwurdp_store::LockEnrolment`] keeps it for
    /// the next sign-in to hold the server to.
    pub fn kdf_to_keep(&self) -> String {
        serde_json::to_string(&self.kdf).expect("a KDF serializes")
    }
}

/// Sign in and open (or make) this app's space.
pub fn sign_in(request: &SignIn<'_>, device: LockDevice) -> Result<SignInOutcome, LockError> {
    let lock = Lock::connect(request.server_url, device)?;
    let email = request.email.trim();
    if email.is_empty() || request.password.is_empty() {
        return Err(LockError::WrongPassword(
            "email and master password are both needed".into(),
        ));
    }
    // Before the key derivation: the server would refuse the login anyway.
    if lock.app_sync_switched_off() {
        return Err(TransportError::SwitchedOff.into());
    }
    let kdf = lock.prelogin(email)?;
    refuse_weaker_kdf(kdf, &request.known)?;
    let master_key = crypto::master_key(request.password, email, kdf)?;
    let hash = Zeroizing::new(crypto::master_password_hash(&master_key, request.password));

    let two_factor = match (&request.two_factor, request.remember_token) {
        (Some(answer), _) => Some((answer.code.as_str(), answer.provider, answer.remember)),
        // Provider 5: the token a device got for "remember me".
        (None, Some(token)) => Some((token, 5, false)),
        (None, None) => None,
    };
    let answer = match lock.token(email, &hash, two_factor)? {
        TokenOutcome::Granted(answer) => answer,
        TokenOutcome::Refused(refusal, status) => {
            if let Some(methods) = two_factor_methods(&refusal) {
                let message = request
                    .two_factor
                    .is_some()
                    .then(|| refusal_message(&refusal, "The two-step code was not accepted."));
                return Ok(SignInOutcome::TwoFactor { methods, message });
            }
            if matches!(status, 400 | 401) {
                // Switched off just now: suite logins are `invalid_client`.
                if lock.app_sync_switched_off() {
                    return Err(TransportError::SwitchedOff.into());
                }
                return Err(LockError::WrongPassword(refusal_message(
                    &refusal,
                    "Email or master password is wrong.",
                )));
            }
            return Err(TransportError::Refused(refusal_message(
                &refusal,
                &format!("HTTP {status}"),
            ))
            .into());
        }
    };

    let protected = answer
        .key
        .as_deref()
        .ok_or_else(|| LockError::Crypto("the server sent no user key".into()))?
        .parse::<EncString>()?;
    let user_key = crypto::decrypt_user_key(&master_key, &protected)?;
    drop(master_key);
    let private_key = match answer.private_key.as_deref() {
        Some(wrapped) => {
            let der = wrapped.parse::<EncString>()?.decrypt(&user_key)?;
            Some(PrivateKey::from_der(&der)?)
        }
        None => None,
    };

    let extras_key = extras_key(&lock, &user_key, private_key.as_ref())?;
    drop(user_key);
    let (space, made_space) = match space(&lock, &extras_key, &request.known, request.accept_space)?
    {
        Taken::Space(space, made) => (space, made),
        Taken::Changed { was, now } => {
            return Ok(SignInOutcome::SpaceChanged {
                was,
                now,
                remember_token: answer.remember_token,
            })
        }
    };

    let server_url = lock.base().to_string();
    let refresh_token = lock.refresh_token();
    let lock = lock.with_space(space.id);
    Ok(SignInOutcome::SignedIn(Box::new(SignedIn {
        lock,
        space,
        made_space,
        server_url,
        email: crypto::normalize_email(email),
        refresh_token,
        remember_token: answer.remember_token,
        kdf,
    })))
}

/// Ask for a two-step code by email. Needs the master password, as the
/// sign-in does: the server only mails a code to someone who knows it.
pub fn send_email_code(
    server_url: &str,
    email: &str,
    password: &str,
    known: &Known,
    device: LockDevice,
) -> Result<(), LockError> {
    let lock = Lock::connect(server_url, device)?;
    let kdf = lock.prelogin(email.trim())?;
    refuse_weaker_kdf(kdf, known)?;
    let master_key = crypto::master_key(password, email.trim(), kdf)?;
    let hash = Zeroizing::new(crypto::master_password_hash(&master_key, password));
    lock.send_email_code(email.trim(), &hash)?;
    Ok(())
}

/// The extras key: opened, opened and wrapped again, or made.
pub(super) fn extras_key(
    lock: &Lock,
    user_key: &SymmetricKey,
    private_key: Option<&PrivateKey>,
) -> Result<SymmetricKey, LockError> {
    for _ in 0..2 {
        let keys = lock.keys()?;
        if keys.extras_key.is_none() && private_key.is_none() {
            return Err(LockError::NoKeyPair);
        }
        match extras::resolve(&keys, user_key, private_key)? {
            Resolved::Open {
                key,
                rewrap,
                private_wrap,
            } => {
                // A failed wrap costs nothing but doing it again next time.
                if let Some(rewrap) = rewrap {
                    if let Err(error) = lock.put_user_wrap(&rewrap) {
                        tracing::info!(%error, "the extras key could not be wrapped again");
                    }
                }
                // A key made before the private wrap existed gets one; a server
                // without the endpoint, or one that has it already, is fine.
                if let Some(private_wrap) = private_wrap {
                    if let Err(error) = lock.put_private_wrap(&private_wrap) {
                        tracing::info!(%error, "the extras key could not be wrapped for the key pair");
                    }
                }
                return Ok(key);
            }
            Resolved::Create(new) => match lock.create_keys(&new.request)? {
                Call::Done(_) => return Ok(new.key),
                // Another app of this account made one just now: take that.
                Call::Refused(refusal) if refusal.code.as_deref() == Some("exists") => continue,
                Call::Refused(refusal) if refusal.code.as_deref() == Some("no_key_pair") => {
                    return Err(LockError::NoKeyPair)
                }
                Call::Refused(refusal) => {
                    return Err(TransportError::Refused(refusal.message).into())
                }
            },
            Resolved::Lost => return Err(LockError::KeysLost),
        }
    }
    Err(TransportError::Refused("the extras key keeps changing".into()).into())
}

/// What [`space`] came to.
enum Taken {
    /// The space, and whether this sign-in made it.
    Space(Space, bool),
    /// Not the space this device used on the account: ask first.
    Changed { was: Uuid, now: Option<Uuid> },
}

/// This app's space: the account's, or a new one — as long as it is the one
/// this device used on the account before, or the person agreed to another.
///
/// Only the server says which space is the account's, and a space's key is
/// only as good as the account's keys it came under. A device that knows a
/// space therefore doesn't follow the server to another one on its own: the
/// server could list a space it kept from before a rekey (whose key a lost
/// device still holds), and everything here would be pushed to it. One this
/// device moved on from is refused outright.
fn space(
    lock: &Lock,
    extras_key: &SymmetricKey,
    known: &Known,
    accept: Option<AcceptSpace>,
) -> Result<Taken, LockError> {
    let open = |id: Uuid, wrapped: &str| -> Result<Space, LockError> {
        let key = SpaceKey::unwrap(wrapped, extras_key)?;
        Ok(Space {
            id,
            key: Zeroizing::new(*key.as_bytes()),
        })
    };
    for _ in 0..2 {
        if let Some(existing) = lock
            .spaces()?
            .into_iter()
            .find(|object| object.space == SPACE)
        {
            if known.left_spaces.contains(&existing.id) {
                return Err(LockError::SpaceLeft(existing.id));
            }
            if let Some(was) = known.space {
                if was != existing.id && accept != Some(AcceptSpace::Listed(existing.id)) {
                    return Ok(Taken::Changed {
                        was,
                        now: Some(existing.id),
                    });
                }
            }
            return Ok(Taken::Space(open(existing.id, &existing.key)?, false));
        }
        if let Some(was) = known.space {
            if accept != Some(AcceptSpace::New) {
                return Ok(Taken::Changed { was, now: None });
            }
        }
        let key = SpaceKey::generate();
        let id = Uuid::new_v4();
        match lock.create_space(id, &key.wrap(extras_key))? {
            Call::Done(_) => {
                return Ok(Taken::Space(
                    Space {
                        id,
                        key: Zeroizing::new(*key.as_bytes()),
                    },
                    true,
                ))
            }
            // Another device made it first: take that one.
            Call::Refused(refusal) if refusal.code.as_deref() == Some("exists") => continue,
            Call::Refused(refusal) => return Err(TransportError::Refused(refusal.message).into()),
        }
    }
    Err(TransportError::Refused("the space keeps changing".into()).into())
}

fn two_factor_methods(refusal: &uwulock_core::wire::TokenError) -> Option<Vec<TwoFactorMethod>> {
    let providers = refusal.two_factor_providers2.as_ref();
    let listed = refusal.two_factor_providers.as_ref();
    if providers.is_none() && listed.is_none() {
        return None;
    }
    let mut methods: Vec<TwoFactorMethod> = providers
        .into_iter()
        .flatten()
        .filter_map(|(provider, details)| {
            let provider: u8 = provider.parse().ok()?;
            let (kind, supported) = two_factor_kind(provider);
            Some(TwoFactorMethod {
                provider,
                kind,
                supported,
                hint: details
                    .get("email")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect();
    if methods.is_empty() {
        methods = listed
            .into_iter()
            .flatten()
            .filter_map(|p| match p {
                Value::Number(n) => n.as_u64().and_then(|n| u8::try_from(n).ok()),
                Value::String(s) => s.parse().ok(),
                _ => None,
            })
            .map(|provider| {
                let (kind, supported) = two_factor_kind(provider);
                TwoFactorMethod {
                    provider,
                    kind,
                    supported,
                    hint: None,
                }
            })
            .collect();
    }
    // A remembered device is no method to pick.
    methods.retain(|m| m.provider != 5);
    methods.sort_by_key(|m| (!m.supported, m.provider));
    Some(methods)
}

fn refusal_message(refusal: &uwulock_core::wire::TokenError, fallback: &str) -> String {
    refusal
        .error_model
        .as_ref()
        .and_then(|m| m.message.clone())
        .or_else(|| refusal.message.clone())
        .or_else(|| {
            refusal
                .error_description
                .clone()
                .filter(|d| !d.eq_ignore_ascii_case("invalid_username_or_password"))
        })
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string())
}
