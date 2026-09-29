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
//! 5. the space: taken if there is one, made with a fresh key if not.
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
use uwulock_core::crypto::{self, EncString, PrivateKey, SymmetricKey};
use uwulock_core::extras::{self, Resolved, SpaceKey};
use uwurdp_store::Space;
use zeroize::Zeroizing;

/// What the person typed.
pub struct SignIn<'a> {
    pub server_url: &'a str,
    pub email: &'a str,
    pub password: &'a str,
    /// The code of a second step, once the server asked for one.
    pub two_factor: Option<TwoFactorAnswer>,
    /// A "remember this device" token from an earlier two-step login.
    pub remember_token: Option<&'a str>,
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
    /// `message` says why when a code was given and did not count.
    TwoFactor {
        methods: Vec<TwoFactorMethod>,
        message: Option<String>,
    },
}

/// A device signed in, with the space it syncs.
pub struct SignedIn {
    /// Signed in, with the space set.
    pub lock: Lock,
    pub space: Space,
    /// This sign-in made the space: the account had no UwURDP data yet.
    pub made_space: bool,
    pub server_url: String,
    pub email: String,
    pub refresh_token: Option<Zeroizing<String>>,
    /// Asked for with the code, to skip the second step next time.
    pub remember_token: Option<Zeroizing<String>>,
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
    let kdf = lock.prelogin(email)?;
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
    let (space, made_space) = space(&lock, &extras_key)?;

    let server_url = lock.base().to_string();
    let refresh_token = lock.refresh_token();
    let lock = lock.with_space(space.id);
    Ok(SignInOutcome::SignedIn(Box::new(SignedIn {
        lock,
        space,
        made_space,
        server_url,
        email: email.to_string(),
        refresh_token,
        remember_token: answer.remember_token,
    })))
}

/// Ask for a two-step code by email. Needs the master password, as the
/// sign-in does: the server only mails a code to someone who knows it.
pub fn send_email_code(
    server_url: &str,
    email: &str,
    password: &str,
    device: LockDevice,
) -> Result<(), LockError> {
    let lock = Lock::connect(server_url, device)?;
    let kdf = lock.prelogin(email.trim())?;
    let master_key = crypto::master_key(password, email.trim(), kdf)?;
    let hash = Zeroizing::new(crypto::master_password_hash(&master_key, password));
    lock.send_email_code(email.trim(), &hash)?;
    Ok(())
}

/// The extras key: opened, opened and wrapped again, or made.
fn extras_key(
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
            Resolved::Open { key, rewrap } => {
                if let Some(rewrap) = rewrap {
                    // A failed wrap costs nothing but doing it again next time.
                    if let Err(error) = lock.put_user_wrap(&rewrap) {
                        tracing::info!(%error, "the extras key could not be wrapped again");
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

/// This app's space: the account's, or a new one.
fn space(lock: &Lock, extras_key: &SymmetricKey) -> Result<(Space, bool), LockError> {
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
            return Ok((open(existing.id, &existing.key)?, false));
        }
        let key = SpaceKey::generate();
        let id = Uuid::new_v4();
        match lock.create_space(id, &key.wrap(extras_key))? {
            Call::Done(_) => {
                return Ok((
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
