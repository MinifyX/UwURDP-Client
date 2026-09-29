//! Talking to a UwULock Server over HTTPS: the login of a suite app, the
//! extras key, the space, and the records.
//!
//! Blocking, like [`crate::http`], and for the same reason: it belongs to the
//! sync thread. The same rules too — https, or plain http to this machine only;
//! no redirects; answers read up to a limit. What is different is the session:
//! an access token for an hour and a refresh token that is replaced at every
//! refresh, so a new one is handed to whoever keeps it ([`Lock::keeping`])
//! before the old one is forgotten, and two threads never refresh at once.

use super::{CLIENT_ID, SPACE};
use crate::engine::{Transport, TransportError};
use parking_lot::Mutex;
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use uwulock_core::crypto::Kdf;
use uwulock_core::extras::{ExtrasKeyRequest, Keys, UserWrapRequest};
use uwulock_core::wire;
use uwurdp_proto::{
    Accepted, EntityKind, Envelope, Hlc, PullResponse, PushResponse, SyncCursor, MAX_BATCH,
    MAX_BATCH_BYTES, SCHEMA_VERSION,
};
use zeroize::Zeroizing;

const TIMEOUT: Duration = Duration::from_secs(30);
/// A page of records: a full batch in base64 inside JSON, with room to spare.
const MAX_PAGE_BYTES: u64 = (MAX_BATCH_BYTES as u64) * 2 + (1 << 20);
/// Everything else the server says is small.
const MAX_ANSWER_BYTES: u64 = 256 * 1024;
/// An access token this close to running out is refreshed first.
const REFRESH_AHEAD: Duration = Duration::from_secs(120);

/// This install, as the server lists it among the account's devices.
#[derive(Debug, Clone)]
pub struct LockDevice {
    /// Made once per install and kept.
    pub identifier: Uuid,
    pub name: String,
    /// Bitwarden's device type: 6 Windows, 7 macOS, 8 Linux desktop.
    pub kind: u8,
}

impl LockDevice {
    pub fn this_system(identifier: Uuid) -> Self {
        let kind = if cfg!(target_os = "windows") {
            6
        } else if cfg!(target_os = "macos") {
            7
        } else {
            8
        };
        Self {
            identifier,
            name: super::DEVICE_NAME.into(),
            kind,
        }
    }
}

/// The tokens of a session.
struct Tokens {
    access: Zeroizing<String>,
    refresh: Option<Zeroizing<String>>,
    renew_at: Instant,
    /// When the access token runs out, as the realtime channel counts.
    expires_unix: u64,
}

type Keeper = Box<dyn Fn(&str) + Send + Sync>;

/// One UwULock Server, as this device talks to it.
pub struct Lock {
    base: String,
    client: Client,
    device: LockDevice,
    space: Option<Uuid>,
    tokens: Mutex<Option<Tokens>>,
    /// Held while refreshing: a refresh token works once, and a second
    /// refresh with the same one would end the session.
    refreshing: Mutex<()>,
    keeper: Option<Keeper>,
    /// The last pull was told to start over.
    reset: AtomicBool,
}

impl std::fmt::Debug for Lock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lock")
            .field("base", &self.base)
            .field("space", &self.space)
            .finish_non_exhaustive()
    }
}

/// A server address from what someone typed: the web vault's address, with or
/// without `https://`, with or without a path of the web vault behind it.
pub fn normalize_server(input: &str) -> Result<String, TransportError> {
    let mut text = input.trim().to_string();
    if text.is_empty() {
        return Err(TransportError::Refused(
            "the server address is empty".into(),
        ));
    }
    if !text.contains("://") {
        text = format!("https://{text}");
    }
    let mut url = reqwest::Url::parse(&text)
        .map_err(|_| TransportError::Refused(format!("“{}” is no address", input.trim())))?;
    url.set_fragment(None);
    url.set_query(None);
    let mut path = url.path().trim_end_matches('/').to_string();
    for tail in ["/api", "/identity", "/vault", "/login", "/#"] {
        if let Some(stripped) = path.strip_suffix(tail) {
            path = stripped.to_string();
        }
    }
    url.set_path(&path);
    let base = url.as_str().trim_end_matches('/').to_string();
    if !crate::http::safe_address(&base) {
        return Err(TransportError::Refused(
            "a UwULock Server must be reached over https (localhost may use http)".into(),
        ));
    }
    Ok(base)
}

/// What a refused request said.
#[derive(Debug)]
pub(crate) struct Refusal {
    pub status: u16,
    /// UwULock's machine-readable reason (`exists`, `scope`, …).
    pub code: Option<String>,
    pub message: String,
}

impl Refusal {
    fn into_transport(self) -> TransportError {
        match self.status {
            401 => TransportError::SignIn(self.message),
            _ => TransportError::Refused(match self.code {
                Some(code) => format!("{} ({code})", self.message),
                None => self.message,
            }),
        }
    }
}

pub(crate) enum Call<T> {
    Done(T),
    Refused(Refusal),
}

/// The answer of the token endpoint, as far as a suite app needs it.
pub(crate) struct TokenAnswer {
    pub key: Option<String>,
    pub private_key: Option<String>,
    pub remember_token: Option<Zeroizing<String>>,
}

pub(crate) enum TokenOutcome {
    Granted(TokenAnswer),
    Refused(wire::TokenError, u16),
}

/// A space as `GET /uwu/v1/suite/spaces` lists it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpaceObject {
    pub space: String,
    pub id: Uuid,
    pub key: String,
}

#[derive(Deserialize)]
struct List<T> {
    data: Vec<T>,
}

/// An envelope as UwULock writes it: UwUSync's, in camelCase, the vault id
/// implied by the space.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireRecord {
    pub id: Uuid,
    /// A kind name; one this build does not know is skipped.
    pub kind: String,
    pub updated_at: WireClock,
    #[serde(default)]
    pub base_seq: u64,
    #[serde(default)]
    pub deleted: bool,
    pub nonce: String,
    pub blob: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireClock {
    pub wall_ms: u64,
    pub counter: u32,
    pub device: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SuitePull {
    #[serde(default)]
    reset: bool,
    #[serde(default)]
    records: Vec<WireRecord>,
    cursor: u64,
    #[serde(default)]
    has_more: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SuitePush {
    #[serde(default)]
    accepted: Vec<Accepted>,
    #[serde(default)]
    conflicts: Vec<WireRecord>,
    cursor: u64,
}

pub(crate) fn kind_name(kind: EntityKind) -> String {
    match serde_json::to_value(kind) {
        Ok(Value::String(name)) => name,
        _ => String::new(),
    }
}

pub(crate) fn kind_of(name: &str) -> Option<EntityKind> {
    serde_json::from_value(Value::String(name.to_string())).ok()
}

impl WireRecord {
    pub(crate) fn from_envelope(envelope: &Envelope) -> Self {
        Self {
            id: envelope.id,
            kind: kind_name(envelope.kind),
            updated_at: WireClock {
                wall_ms: envelope.updated_at.wall_ms,
                counter: envelope.updated_at.counter,
                device: envelope.updated_at.device,
            },
            base_seq: envelope.base_seq,
            deleted: envelope.deleted,
            nonce: encode(&envelope.nonce),
            blob: encode(&envelope.blob),
            seq: envelope.seq,
        }
    }

    /// The envelope, or `None` for a kind this build does not know or bytes
    /// that are not base64 — neither of which it could open anyway.
    pub(crate) fn into_envelope(self, space: Uuid) -> Option<Envelope> {
        Some(Envelope {
            id: self.id,
            vault_id: space,
            kind: kind_of(&self.kind)?,
            updated_at: Hlc::new(
                self.updated_at.wall_ms,
                self.updated_at.counter,
                self.updated_at.device,
            ),
            base_seq: self.base_seq,
            deleted: self.deleted,
            nonce: decode(&self.nonce)?,
            blob: decode(&self.blob)?,
            seq: self.seq,
        })
    }
}

impl Lock {
    /// Reach a server. Its certificate has to stand up to the usual checks —
    /// a UwULock Server has a real one, its own Let's Encrypt or a proxy's.
    pub fn connect(server: &str, device: LockDevice) -> Result<Self, TransportError> {
        let base = normalize_server(server)?;
        let mut builder = Client::builder();
        if base.starts_with("http://") {
            builder = builder.no_proxy();
        }
        let client = builder
            .use_preconfigured_tls(crate::pin::webpki_config())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TIMEOUT)
            .connect_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(1)
            .user_agent(concat!("UwURDP/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| TransportError::Unreachable(error.to_string()))?;
        Ok(Self {
            base,
            client,
            device,
            space: None,
            tokens: Mutex::new(None),
            refreshing: Mutex::new(()),
            keeper: None,
            reset: AtomicBool::new(false),
        })
    }

    /// The server's address, as it is kept.
    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn device(&self) -> &LockDevice {
        &self.device
    }

    /// The space whose records this syncs.
    pub fn with_space(mut self, space: Uuid) -> Self {
        self.space = Some(space);
        self
    }

    pub fn space(&self) -> Option<Uuid> {
        self.space
    }

    /// A session from a refresh token kept earlier: the first request
    /// refreshes it.
    pub fn with_refresh_token(self, refresh: Zeroizing<String>) -> Self {
        *self.tokens.lock() = Some(Tokens {
            access: Zeroizing::new(String::new()),
            refresh: Some(refresh),
            renew_at: Instant::now(),
            expires_unix: 0,
        });
        self
    }

    /// Who keeps a new refresh token. Called before the old one is forgotten.
    pub fn keeping(mut self, keeper: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.keeper = Some(Box::new(keeper));
        self
    }

    /// The refresh token of the session, to be kept.
    pub fn refresh_token(&self) -> Option<Zeroizing<String>> {
        self.tokens.lock().as_ref().and_then(|t| t.refresh.clone())
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn request(&self, method: reqwest::Method, path: &str) -> RequestBuilder {
        self.client
            .request(method, self.url(path))
            .header("Accept", "application/json")
            .header("Device-Type", self.device.kind.to_string())
    }

    // ── Logging in ─────────────────────────────────────────────────────────

    /// How the master key is derived for this email.
    pub(crate) fn prelogin(&self, email: &str) -> Result<Kdf, TransportError> {
        let body = serde_json::json!({ "email": uwulock_core::crypto::normalize_email(email) });
        let mut last = None;
        for path in ["/identity/accounts/prelogin", "/api/accounts/prelogin"] {
            let response = self
                .request(reqwest::Method::POST, path)
                .json(&body)
                .send()
                .map_err(unreachable)?;
            let status = response.status().as_u16();
            if status == 404 || status == 405 {
                last = Some(status);
                continue;
            }
            let body = read_limited(response, MAX_ANSWER_BYTES)?;
            if !(200..300).contains(&status) {
                return Err(refusal(status, &body).into_transport());
            }
            let prelogin: wire::Prelogin = parse_lowercase(&body)?;
            return kdf_from(&prelogin);
        }
        Err(TransportError::Refused(format!(
            "this is no UwULock Server (HTTP {})",
            last.unwrap_or(404)
        )))
    }

    /// The password grant, as a suite app: `scope=uwu.suite`, this app's
    /// `client_id`.
    pub(crate) fn token(
        &self,
        email: &str,
        password_hash: &str,
        two_factor: Option<(&str, u8, bool)>,
    ) -> Result<TokenOutcome, TransportError> {
        let email = uwulock_core::crypto::normalize_email(email);
        let mut form: Vec<(&str, Zeroizing<String>)> = vec![
            ("grant_type", "password".to_string().into()),
            ("username", email.clone().into()),
            ("password", password_hash.to_string().into()),
            ("scope", "uwu.suite offline_access".to_string().into()),
            ("client_id", CLIENT_ID.to_string().into()),
            ("deviceType", self.device.kind.to_string().into()),
            (
                "deviceIdentifier",
                self.device.identifier.to_string().into(),
            ),
            ("deviceName", self.device.name.clone().into()),
        ];
        if let Some((code, provider, remember)) = two_factor {
            form.push(("twoFactorToken", code.trim().replace(' ', "").into()));
            form.push(("twoFactorProvider", provider.to_string().into()));
            form.push(("twoFactorRemember", u8::from(remember).to_string().into()));
        }
        let pairs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, v.as_str())).collect();
        use base64::Engine as _;
        let response = self
            .request(reqwest::Method::POST, "/identity/connect/token")
            .header(
                "Auth-Email",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(email.as_bytes()),
            )
            .form(&pairs)
            .send()
            .map_err(unreachable)?;
        let status = response.status().as_u16();
        let body = Zeroizing::new(read_limited(response, MAX_ANSWER_BYTES)?);
        if (200..300).contains(&status) {
            let token: wire::Token = parse_lowercase(&body)?;
            let answer = TokenAnswer {
                key: token.key.clone(),
                private_key: token.private_key.clone(),
                remember_token: token.two_factor_token.clone().map(Zeroizing::new),
            };
            self.take(token);
            return Ok(TokenOutcome::Granted(answer));
        }
        let refusal: wire::TokenError = parse_lowercase(&body).unwrap_or_default();
        Ok(TokenOutcome::Refused(refusal, status))
    }

    /// Asks the server to email a two-step code.
    pub(crate) fn send_email_code(
        &self,
        email: &str,
        password_hash: &str,
    ) -> Result<(), TransportError> {
        let body = serde_json::json!({
            "email": uwulock_core::crypto::normalize_email(email),
            "masterPasswordHash": password_hash,
            "deviceIdentifier": self.device.identifier.to_string(),
        });
        let response = self
            .request(reqwest::Method::POST, "/api/two-factor/send-email-login")
            .json(&body)
            .send()
            .map_err(unreachable)?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let body = read_limited(response, MAX_ANSWER_BYTES)?;
        Err(refusal(status, &body).into_transport())
    }

    /// Keep the tokens of a grant.
    fn take(&self, token: wire::Token) {
        let lifetime = Duration::from_secs(token.expires_in.unwrap_or(3600).clamp(60, 86_400));
        let expires_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            + lifetime.as_secs();
        let mut tokens = self.tokens.lock();
        let refresh = token
            .refresh_token
            .map(Zeroizing::new)
            .or_else(|| tokens.as_ref().and_then(|t| t.refresh.clone()));
        if let (Some(keeper), Some(refresh)) = (&self.keeper, &refresh) {
            keeper(refresh);
        }
        *tokens = Some(Tokens {
            access: Zeroizing::new(token.access_token),
            refresh,
            renew_at: Instant::now() + lifetime.saturating_sub(REFRESH_AHEAD),
            expires_unix,
        });
    }

    /// A current access token, refreshed first when it is about to run out,
    /// and when it runs out: the Unix second.
    pub fn access_token(&self) -> Result<(Zeroizing<String>, u64), TransportError> {
        {
            let tokens = self.tokens.lock();
            match tokens.as_ref() {
                None => return Err(TransportError::SignIn("not signed in".into())),
                Some(t) if Instant::now() < t.renew_at && !t.access.is_empty() => {
                    return Ok((t.access.clone(), t.expires_unix));
                }
                Some(_) => {}
            }
        }
        self.refresh(None)?;
        let tokens = self.tokens.lock();
        let t = tokens
            .as_ref()
            .ok_or_else(|| TransportError::SignIn("not signed in".into()))?;
        Ok((t.access.clone(), t.expires_unix))
    }

    /// A new access token from the refresh token. `stale` is the access token
    /// a request was refused with: if another thread refreshed meanwhile,
    /// that one is used instead of refreshing twice.
    fn refresh(&self, stale: Option<&str>) -> Result<(), TransportError> {
        let _one_at_a_time = self.refreshing.lock();
        let refresh = {
            let tokens = self.tokens.lock();
            let Some(t) = tokens.as_ref() else {
                return Err(TransportError::SignIn("not signed in".into()));
            };
            let fresh = Instant::now() < t.renew_at && !t.access.is_empty();
            let changed = stale.is_some_and(|stale| stale != t.access.as_str());
            if fresh && (stale.is_none() || changed) {
                return Ok(());
            }
            t.refresh
                .clone()
                .ok_or_else(|| TransportError::SignIn("the session has no refresh token".into()))?
        };
        let form = [
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh.as_str()),
        ];
        let response = self
            .request(reqwest::Method::POST, "/identity/connect/token")
            .form(&form)
            .send()
            .map_err(unreachable)?;
        let status = response.status().as_u16();
        let body = Zeroizing::new(read_limited(response, MAX_ANSWER_BYTES)?);
        if (200..300).contains(&status) {
            let token: wire::Token = parse_lowercase(&body)?;
            self.take(token);
            return Ok(());
        }
        if matches!(status, 400 | 401) {
            // `invalid_grant`: run out, revoked, device removed, password
            // changed. Only the master password helps now.
            *self.tokens.lock() = None;
            return Err(TransportError::SignIn("the session has ended".into()));
        }
        Err(refusal(status, &body).into_transport())
    }

    /// A new access token now, whatever this device thinks of the current
    /// one: the realtime channel was told it is no good (4401).
    pub(crate) fn renew(&self) -> Result<(), TransportError> {
        let current = self
            .tokens
            .lock()
            .as_ref()
            .map(|t| t.access.clone())
            .ok_or_else(|| TransportError::SignIn("not signed in".into()))?;
        {
            // Stale by definition, so `refresh` does not take it as fresh.
            if let Some(t) = self.tokens.lock().as_mut() {
                t.renew_at = Instant::now();
            }
        }
        self.refresh(Some(current.as_str()))
    }

    /// Forget the session here. The server ends it by itself.
    pub fn forget_session(&self) {
        *self.tokens.lock() = None;
    }

    // ── Authorised requests ────────────────────────────────────────────────

    /// One request with the access token — and, when it comes back
    /// unauthorised, one more after refreshing.
    pub(crate) fn call<T: DeserializeOwned>(
        &self,
        build: impl Fn() -> RequestBuilder,
        limit: u64,
    ) -> Result<Call<T>, TransportError> {
        let (token, _) = self.access_token()?;
        let mut response = build()
            .bearer_auth(token.as_str())
            .send()
            .map_err(unreachable)?;
        if response.status().as_u16() == 401 {
            self.refresh(Some(token.as_str()))?;
            let (token, _) = self.access_token()?;
            response = build()
                .bearer_auth(token.as_str())
                .send()
                .map_err(unreachable)?;
        }
        let status = response.status().as_u16();
        let body = read_limited(response, limit)?;
        if (200..300).contains(&status) {
            let parsed = serde_json::from_slice(if body.is_empty() { b"null" } else { &body })
                .map_err(|error| {
                    TransportError::Refused(format!("the server's answer: {error}"))
                })?;
            return Ok(Call::Done(parsed));
        }
        Ok(Call::Refused(refusal(status, &body)))
    }

    fn call_ok<T: DeserializeOwned>(
        &self,
        build: impl Fn() -> RequestBuilder,
        limit: u64,
    ) -> Result<T, TransportError> {
        match self.call(build, limit)? {
            Call::Done(value) => Ok(value),
            Call::Refused(refusal) => Err(refusal.into_transport()),
        }
    }

    pub(crate) fn keys(&self) -> Result<Keys, TransportError> {
        self.call_ok(
            || self.request(reqwest::Method::GET, "/uwu/v1/keys"),
            MAX_ANSWER_BYTES,
        )
    }

    pub(crate) fn create_keys(
        &self,
        request: &ExtrasKeyRequest,
    ) -> Result<Call<Keys>, TransportError> {
        self.call(
            || {
                self.request(reqwest::Method::POST, "/uwu/v1/keys")
                    .json(request)
            },
            MAX_ANSWER_BYTES,
        )
    }

    pub(crate) fn put_user_wrap(&self, request: &UserWrapRequest) -> Result<(), TransportError> {
        let _: Value = self.call_ok(
            || {
                self.request(reqwest::Method::PUT, "/uwu/v1/keys/user-wrap")
                    .json(request)
            },
            MAX_ANSWER_BYTES,
        )?;
        Ok(())
    }

    pub(crate) fn spaces(&self) -> Result<Vec<SpaceObject>, TransportError> {
        let list: List<SpaceObject> = self.call_ok(
            || self.request(reqwest::Method::GET, "/uwu/v1/suite/spaces"),
            MAX_ANSWER_BYTES,
        )?;
        Ok(list.data)
    }

    pub(crate) fn create_space(
        &self,
        id: Uuid,
        key: &str,
    ) -> Result<Call<SpaceObject>, TransportError> {
        let body = serde_json::json!({ "id": id, "key": key });
        self.call(
            || {
                self.request(
                    reqwest::Method::PUT,
                    &format!("/uwu/v1/suite/spaces/{SPACE}"),
                )
                .json(&body)
            },
            MAX_ANSWER_BYTES,
        )
    }

    fn records_path() -> String {
        format!("/uwu/v1/suite/spaces/{SPACE}/records")
    }

    fn space_id(&self) -> Result<Uuid, TransportError> {
        self.space
            .ok_or_else(|| TransportError::Refused("no space to sync".into()))
    }
}

impl Transport for Lock {
    fn pull(&self, since: SyncCursor, limit: usize) -> Result<PullResponse, TransportError> {
        let space = self.space_id()?;
        let limit = limit.min(MAX_BATCH);
        let page: SuitePull = self.call_ok(
            || {
                self.request(reqwest::Method::GET, &Self::records_path())
                    .query(&[("since", since.0.to_string()), ("limit", limit.to_string())])
            },
            MAX_PAGE_BYTES,
        )?;
        if page.reset {
            // A space with a new key has a new id as well: what comes now is
            // sealed under a key this device does not have.
            let current = self
                .spaces()?
                .into_iter()
                .find(|object| object.space == SPACE);
            if current.is_none_or(|object| object.id != space) {
                return Err(TransportError::SignIn(
                    "the space was given a new key".into(),
                ));
            }
            self.reset.store(true, Ordering::SeqCst);
            return Ok(PullResponse {
                envelopes: Vec::new(),
                cursor: since,
                has_more: false,
            });
        }
        let envelopes = page
            .records
            .into_iter()
            .filter_map(|record| record.into_envelope(space))
            .collect();
        Ok(PullResponse {
            envelopes,
            cursor: SyncCursor(page.cursor),
            has_more: page.has_more,
        })
    }

    fn push(&self, envelopes: Vec<Envelope>) -> Result<PushResponse, TransportError> {
        let space = self.space_id()?;
        let records: Vec<WireRecord> = envelopes
            .iter()
            .map(|envelope| {
                let mut record = WireRecord::from_envelope(envelope);
                record.seq = None;
                record
            })
            .collect();
        let body = serde_json::json!({ "schema": SCHEMA_VERSION, "records": records });
        let answer: SuitePush = self.call_ok(
            || {
                self.request(reqwest::Method::POST, &Self::records_path())
                    .json(&body)
            },
            MAX_PAGE_BYTES,
        )?;
        Ok(PushResponse {
            accepted: answer.accepted,
            conflicts: answer
                .conflicts
                .into_iter()
                .filter_map(|record| record.into_envelope(space))
                .collect(),
            cursor: SyncCursor(answer.cursor),
        })
    }

    fn take_reset(&self) -> bool {
        self.reset.swap(false, Ordering::SeqCst)
    }
}

fn unreachable(error: reqwest::Error) -> TransportError {
    TransportError::Unreachable(error.to_string())
}

/// The body, or a refusal when it is longer than `limit`.
fn read_limited(response: Response, limit: u64) -> Result<Vec<u8>, TransportError> {
    use std::io::Read;
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(TransportError::Refused(
            "the server's answer is too large".into(),
        ));
    }
    let mut body = Vec::new();
    response
        .take(limit + 1)
        .read_to_end(&mut body)
        .map_err(|error| TransportError::Unreachable(error.to_string()))?;
    if body.len() as u64 > limit {
        return Err(TransportError::Refused(
            "the server's answer is too large".into(),
        ));
    }
    Ok(body)
}

/// Bitwarden's identity answers come in PascalCase or camelCase depending on
/// the server; `uwulock_core::wire` reads them in lower case.
fn parse_lowercase<T: DeserializeOwned>(body: &[u8]) -> Result<T, TransportError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|error| TransportError::Refused(format!("the server's answer: {error}")))?;
    serde_json::from_value(wire::lowercase_keys(value))
        .map_err(|error| TransportError::Refused(format!("the server's answer: {error}")))
}

/// What a refusal's body says: UwULock's `message` and `code`, Bitwarden's
/// `ErrorModel`, OAuth's `error_description`.
fn refusal(status: u16, body: &[u8]) -> Refusal {
    let value = serde_json::from_slice::<Value>(body)
        .map(wire::lowercase_keys)
        .unwrap_or(Value::Null);
    let text = |value: &Value| value.as_str().map(str::to_string).filter(|s| !s.is_empty());
    let message = text(&value["message"])
        .or_else(|| text(&value["errormodel"]["message"]))
        .or_else(|| text(&value["error_description"]))
        .or_else(|| text(&value["error"]))
        .unwrap_or_else(|| format!("HTTP {status}"));
    Refusal {
        status,
        code: text(&value["code"]),
        message,
    }
}

fn kdf_from(prelogin: &wire::Prelogin) -> Result<Kdf, TransportError> {
    let kdf = match prelogin.kdf.unwrap_or(0) {
        0 => Kdf::Pbkdf2 {
            iterations: prelogin.kdf_iterations.unwrap_or(600_000),
        },
        1 => Kdf::Argon2id {
            iterations: prelogin.kdf_iterations.unwrap_or(3),
            memory_mib: prelogin.kdf_memory.unwrap_or(64),
            parallelism: prelogin.kdf_parallelism.unwrap_or(4),
        },
        other => {
            return Err(TransportError::Refused(format!(
                "key derivation type {other} is not supported"
            )))
        }
    };
    // Floors against a server that would make the master password cheap to
    // guess from its hash, ceilings against one that would keep this device
    // deriving for hours.
    kdf.check()
        .and_then(|()| kdf.check_ceilings())
        .map_err(|error| TransportError::Refused(error.to_string()))?;
    Ok(kdf)
}

fn encode(bytes: &[u8]) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    STANDARD.encode(bytes)
}

/// Standard base64 as written here; the URL-safe alphabet and missing
/// padding are read too.
fn decode(text: &str) -> Option<Vec<u8>> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    use base64::Engine;
    let text = text.trim();
    [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD]
        .iter()
        .find_map(|engine| engine.decode(text).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_is_read_the_way_people_type_it() {
        for (typed, base) in [
            ("lock.example.com", "https://lock.example.com"),
            ("https://lock.example.com/", "https://lock.example.com"),
            (
                "https://lock.example.com/#/login",
                "https://lock.example.com",
            ),
            (
                "https://lock.example.com/identity",
                "https://lock.example.com",
            ),
            ("https://example.com/lock/api", "https://example.com/lock"),
            ("http://127.0.0.1:8080", "http://127.0.0.1:8080"),
        ] {
            assert_eq!(normalize_server(typed).unwrap(), base, "{typed}");
        }
        for typed in ["", "http://lock.example.com", "ftp://lock.example.com"] {
            assert!(normalize_server(typed).is_err(), "{typed}");
        }
    }

    #[test]
    fn a_record_crosses_the_wire_in_camel_case_and_back() {
        let envelope = Envelope {
            id: Uuid::new_v4(),
            vault_id: Uuid::new_v4(),
            kind: EntityKind::KnownHost,
            updated_at: Hlc::new(1_790_000_000_000, 2, 305_419_896),
            base_seq: 17,
            deleted: false,
            nonce: vec![1; 24],
            blob: vec![0xfb, 0xff, 0xbe],
            seq: None,
        };
        let json = serde_json::to_value(WireRecord::from_envelope(&envelope)).unwrap();
        assert_eq!(json["kind"], "known_host");
        assert_eq!(json["updatedAt"]["wallMs"], 1_790_000_000_000u64);
        assert_eq!(json["baseSeq"], 17);
        assert_eq!(json["blob"], "+/++");
        assert!(json.get("seq").is_none());

        let back: WireRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back.into_envelope(envelope.vault_id).unwrap(), envelope);

        let unknown = WireRecord {
            kind: "hologram".into(),
            ..WireRecord::from_envelope(&envelope)
        };
        assert!(unknown.into_envelope(envelope.vault_id).is_none());
        assert_eq!(decode("-_--").unwrap(), vec![0xfb, 0xff, 0xbe]);
    }

    #[test]
    fn a_server_asking_for_cheap_or_endless_key_derivation_is_refused() {
        let prelogin = |kdf: u32, iterations: u32| wire::Prelogin {
            kdf: Some(kdf),
            kdf_iterations: Some(iterations),
            kdf_memory: Some(64),
            kdf_parallelism: Some(4),
        };
        assert!(kdf_from(&prelogin(0, 600_000)).is_ok());
        assert!(kdf_from(&prelogin(0, 100)).is_err());
        assert!(kdf_from(&prelogin(0, 100_000_000)).is_err());
        assert!(kdf_from(&prelogin(1, 3)).is_ok());
        assert!(kdf_from(&prelogin(7, 3)).is_err());
    }
}
