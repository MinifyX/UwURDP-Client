//! The same code against a real UwULock Server — and, for the move, a real
//! UwUSync Server — instead of [`super::fake`]: what the fake cannot promise
//! is that it says what the server says. Opt-in, and not in CI:
//! `scripts/lock-live.sh` starts both servers on free ports of this machine,
//! makes the accounts these tests use, and runs them.
//!
//! What they read from the environment:
//!
//! - `UWULOCK_TEST_SERVER`: the server's address (`https://localhost:<port>`);
//! - `UWULOCK_TEST_CA`: the test CA its certificate is from (`crate::pin`
//!   trusts it in test builds only);
//! - `UWULOCK_TEST_PASSWORD`: the master password of every account, which is
//!   `<test>@example.com` for the test's own name;
//! - `UWUSYNC_TEST_SETUP`: a setup code of the UwUSync Server, for the move.
//!
//! Every test has an account of its own, so they run side by side.

use super::*;
use crate::engine::{Transport, TransportError};
use crate::{sync_once, SyncError};
use serde_json::{json, Value};
use std::sync::{mpsc, Arc, Barrier};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use uwulock_core::crypto::{self, SymmetricKey};
use uwulock_core::extras::SpaceKey;
use uwurdp_proto::{EntityKind, Envelope, Hlc, SyncCursor};
use uwurdp_store::{HostDraft, Joining, LockEnrolment, PasswordChange, SecretText, Space, Store};
use uwurdp_vault::{KdfParams, Sealed, UnlockedVault};
use zeroize::Zeroizing;

const NEEDS: &str = "needs a running UwULock Server: UWULOCK_TEST_SERVER=… (scripts/lock-live.sh)";
/// The other suite app, whose space this one must not see.
const OTHER_SPACE: &str = "ssh";
const OTHER_CLIENT: &str = "uwussh";

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set: {NEEDS}"))
}

fn server() -> String {
    var("UWULOCK_TEST_SERVER")
}

fn password() -> String {
    var("UWULOCK_TEST_PASSWORD")
}

fn email(account: &str) -> String {
    format!("{account}@example.com")
}

fn device() -> LockDevice {
    LockDevice::this_system(Uuid::new_v4())
}

fn signed_in(account: &str) -> SignedIn {
    signed_in_as(account, device())
}

fn signed_in_as(account: &str, device: LockDevice) -> SignedIn {
    let (server, email, password) = (server(), email(account), password());
    let request = SignIn {
        server_url: &server,
        email: &email,
        password: &password,
        two_factor: None,
        remember_token: None,
        known: Known::default(),
        accept_space: None,
    };
    match sign_in(&request, device) {
        Ok(SignInOutcome::SignedIn(signed)) => *signed,
        Ok(SignInOutcome::TwoFactor { .. }) => panic!("{account} has two-step login"),
        Ok(SignInOutcome::SpaceChanged { .. }) => panic!("{account}: a device knowing nothing"),
        Err(error) => panic!("{account} did not sign in: {error}"),
    }
}

fn copy(space: &Space) -> Space {
    Space {
        id: space.id,
        key: space.key.clone(),
    }
}

/// A device that signed in and took the space as its vault.
fn joined(account: &str, store: &Store) -> Lock {
    let signed = signed_in(account);
    join(store, &signed, Joining::SignIn);
    signed.lock
}

fn join(store: &Store, signed: &SignedIn, how: Joining) {
    store
        .join_space(
            copy(&signed.space),
            password().as_bytes(),
            KdfParams::INSECURE_FOR_TESTS,
            how,
            &LockEnrolment {
                server_url: &signed.server_url,
                email: &signed.email,
                protected_refresh_token: None,
                protected_remember_token: None,
                kdf: &signed.kdf_to_keep(),
            },
        )
        .unwrap();
}

fn draft(name: &str, address: &str) -> HostDraft {
    HostDraft {
        id: None,
        name: name.into(),
        address: address.into(),
        port: 3389,
        username: "uwu".into(),
        domain: String::new(),
        group_path: None,
        workspace: None,
        password: PasswordChange::Keep,
        rdp: uwurdp_store::RdpSettings::default(),
        comment: String::new(),
        gateway_username: String::new(),
        gateway_domain: String::new(),
        gateway_password: PasswordChange::Keep,
    }
}

fn names(store: &Store) -> Vec<String> {
    let mut names: Vec<String> = store
        .list_hosts()
        .unwrap()
        .into_iter()
        .map(|h| h.name)
        .collect();
    names.sort();
    names
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Requests the app never makes, to look at the wire and to do what only
/// the web vault does (remove a device, turn on two-step login).
struct Raw {
    base: String,
    client: reqwest::blocking::Client,
}

impl Raw {
    fn new() -> Self {
        Self {
            base: server(),
            client: reqwest::blocking::Client::builder()
                .use_preconfigured_tls(crate::pin::roots_config())
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        }
    }

    fn call(&self, method: &str, path: &str, token: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = self
            .client
            .request(method.parse().unwrap(), format!("{}{path}", self.base))
            .bearer_auth(token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        answer(request.send().unwrap())
    }

    fn token(&self, form: &[(&str, &str)]) -> (u16, Value) {
        answer(
            self.client
                .post(format!("{}/identity/connect/token", self.base))
                .form(form)
                .send()
                .unwrap(),
        )
    }
}

fn answer(response: reqwest::blocking::Response) -> (u16, Value) {
    let status = response.status().as_u16();
    let text = response.text().unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

/// The master password hash, as every login sends it.
fn password_hash(account: &str) -> String {
    let lock = Lock::connect(&server(), device()).unwrap();
    let kdf = lock.prelogin(&email(account)).unwrap();
    let key = crypto::master_key(&password(), &email(account), kdf).unwrap();
    crypto::master_password_hash(&key, &password())
}

/// A token of the web vault: scope `api`, everything a suite app's may not.
fn web_token(raw: &Raw, account: &str) -> String {
    let (email, hash, id) = (email(account), password_hash(account), Uuid::new_v4());
    let (status, body) = raw.token(&[
        ("grant_type", "password"),
        ("username", &email),
        ("password", &hash),
        ("scope", "api offline_access"),
        ("client_id", "web"),
        ("deviceType", "9"),
        ("deviceIdentifier", &id.to_string()),
        ("deviceName", "firefox"),
    ]);
    assert_eq!(status, 200, "{body}");
    body["access_token"].as_str().unwrap().to_string()
}

/// The account's extras key, as a sign-in opens it (and then drops it).
fn extras_key(account: &str) -> SymmetricKey {
    let lock = Lock::connect(&server(), device()).unwrap();
    let email = email(account);
    let kdf = lock.prelogin(&email).unwrap();
    let master_key = crypto::master_key(&password(), &email, kdf).unwrap();
    let hash = crypto::master_password_hash(&master_key, &password());
    let api::TokenOutcome::Granted(answer) = lock.token(&email, &hash, None).unwrap() else {
        panic!("{account} did not log in")
    };
    let protected: crypto::EncString = answer.key.unwrap().parse().unwrap();
    let user_key = crypto::decrypt_user_key(&master_key, &protected).unwrap();
    let private: crypto::EncString = answer.private_key.unwrap().parse().unwrap();
    let private = crypto::PrivateKey::from_der(&private.decrypt(&user_key).unwrap()).unwrap();
    account::extras_key(&lock, &user_key, Some(&private)).unwrap()
}

fn claims(token: &str) -> Value {
    use base64::Engine as _;
    let payload = token.split('.').nth(1).unwrap();
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn access(lock: &Lock) -> String {
    lock.access_token().unwrap().0.to_string()
}

/// A record of `kind`, sealed for the space as the store would.
fn sealed(space: &Space, kind: EntityKind, payload: &[u8]) -> Envelope {
    let vault = UnlockedVault::from_key(space.id, space.key.clone());
    let id = Uuid::new_v4();
    let clock = Hlc::new(unix_now() * 1000, 0, 7);
    let Sealed { nonce, blob } = vault.seal_synced(id, kind, clock, false, payload).unwrap();
    Envelope {
        id,
        vault_id: space.id,
        kind,
        updated_at: clock,
        base_seq: 0,
        deleted: false,
        nonce,
        blob,
        seq: None,
    }
}

fn records_path(space: &str) -> String {
    format!("/uwu/v1/suite/spaces/{space}/records")
}

// ── Logging in ──────────────────────────────────────────────────────────────

#[test]
#[ignore = "needs a running UwULock Server: UWULOCK_TEST_SERVER=… (scripts/lock-live.sh)"]
fn login_as_a_suite_app_and_what_its_token_may_do() {
    let raw = Raw::new();
    let (server, email) = (server(), email("login"));

    // A wrong password, in the server's words.
    let wrong = SignIn {
        server_url: &server,
        email: &email,
        password: "not it",
        two_factor: None,
        remember_token: None,
        known: Known::default(),
        accept_space: None,
    };
    match sign_in(&wrong, device()) {
        Err(LockError::WrongPassword(message)) => {
            assert!(
                !message.is_empty() && !message.starts_with("HTTP"),
                "{message}"
            );
        }
        other => panic!("{:?}", other.err()),
    }

    let first = signed_in("login");
    assert!(first.made_space);
    let token = claims(&access(&first.lock));
    assert_eq!(token["client_id"], CLIENT_ID);
    assert_eq!(token["scope"], json!(["uwu.suite", "offline_access"]));

    let second = signed_in("login");
    assert!(!second.made_space);
    assert_eq!(second.space.id, first.space.id);
    assert_eq!(*second.space.key, *first.space.key);

    // The client id and the scope belong together.
    let hash = password_hash("login");
    let id = Uuid::new_v4().to_string();
    let form = |client: &'static str, scope: &'static str| {
        vec![
            ("grant_type", "password"),
            ("username", email.as_str()),
            ("password", hash.as_str()),
            ("scope", scope),
            ("client_id", client),
            ("deviceType", "8"),
            ("deviceIdentifier", id.as_str()),
            ("deviceName", DEVICE_NAME),
        ]
    };
    let (status, body) = raw.token(&form("desktop", "uwu.suite offline_access"));
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("invalid_client"))
    );
    let (status, body) = raw.token(&form(CLIENT_ID, "api offline_access"));
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("invalid_scope"))
    );
    let (status, body) = raw.token(&form(CLIENT_ID, "uwu.suite offline_access"));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["scope"], "uwu.suite offline_access");

    // Its own space, and nothing else.
    let suite = access(&first.lock);
    for (method, path) in [
        ("GET", "/api/sync".to_string()),
        ("GET", "/api/accounts/profile".to_string()),
        ("GET", "/uwu/v1/sync?include=vault".to_string()),
        ("GET", format!("{}?since=0", records_path(OTHER_SPACE))),
        ("DELETE", format!("/uwu/v1/suite/spaces/{SPACE}")),
        ("GET", "/uwu/v1/devices".to_string()),
    ] {
        let (status, body) = raw.call(method, &path, &suite, Some(json!({})));
        assert_eq!(status, 403, "{method} {path}: {body}");
        if path.starts_with("/uwu") {
            assert_eq!(body["code"], "scope", "{method} {path}: {body}");
        }
    }
    // Without `include`, a suite token's sync is `suite`.
    for path in ["/uwu/v1/sync?include=suite", "/uwu/v1/sync"] {
        let (status, body) = raw.call("GET", path, &suite, None);
        assert_eq!(status, 200, "{body}");
        let spaces = body["suite"]["spaces"].as_array().unwrap();
        assert!(spaces.iter().all(|s| s["space"] == SPACE), "{body}");
        assert!(body["vault"].is_null() && body["uwu"].is_null(), "{body}");
    }
    for path in ["/uwu/v1/info", "/uwu/v1/keys", "/uwu/v1/suite/spaces"] {
        let (status, body) = raw.call("GET", path, &suite, None);
        assert_eq!(status, 200, "{path}: {body}");
    }

    // The web vault lists the app's devices as such.
    let web = web_token(&raw, "login");
    let (status, devices) = raw.call("GET", "/uwu/v1/devices", &web, None);
    assert_eq!(status, 200, "{devices}");
    let list = devices
        .as_array()
        .or_else(|| devices["data"].as_array())
        .unwrap();
    let this = list
        .iter()
        .find(|d| d["id"] == first.lock.device().identifier.to_string())
        .unwrap_or_else(|| panic!("{devices}"));
    assert_eq!(this["app"], CLIENT_ID);
    assert_eq!(this["name"], DEVICE_NAME);
}

#[test]
#[ignore = "needs a running UwULock Server: UWULOCK_TEST_SERVER=… (scripts/lock-live.sh)"]
fn two_step_login_with_an_authenticator_and_a_remembered_device() {
    let raw = Raw::new();
    let web = web_token(&raw, "twostep");
    let hash = password_hash("twostep");
    let (status, got) = raw.call(
        "POST",
        "/api/two-factor/get-authenticator",
        &web,
        Some(json!({ "masterPasswordHash": hash })),
    );
    assert_eq!(status, 200, "{got}");
    let key = got["key"].as_str().unwrap().to_string();
    let totp = uwulock_core::totp::Totp::parse(&key).unwrap();
    let (code, _) = totp.now();
    let (status, body) = raw.call(
        "PUT",
        "/api/two-factor/authenticator",
        &web,
        Some(json!({ "key": key, "token": code.as_str(), "masterPasswordHash": hash })),
    );
    assert_eq!(status, 200, "{body}");

    let (server, email, password) = (server(), email("twostep"), password());
    // "Remember this device" is for this one: the same identifier throughout.
    let this = device();
    let request = |two_factor: Option<TwoFactorAnswer>, remember_token: Option<&str>| {
        let request = SignIn {
            server_url: &server,
            email: &email,
            password: &password,
            two_factor,
            remember_token,
            known: Known::default(),
            accept_space: None,
        };
        sign_in(&request, this.clone()).unwrap()
    };
    let SignInOutcome::TwoFactor { methods, message } = request(None, None) else {
        panic!("a second step")
    };
    assert_eq!(message, None);
    assert_eq!(methods.len(), 1);
    assert_eq!(
        (methods[0].kind, methods[0].supported),
        ("authenticator", true)
    );

    let wrong = TwoFactorAnswer {
        provider: 0,
        code: "000000".into(),
        remember: false,
    };
    // Refused like a wrong password, in the server's words (Bitwarden's
    // way); the app stays at the code.
    let request_wrong = SignIn {
        server_url: &server,
        email: &email,
        password: &password,
        two_factor: Some(wrong),
        remember_token: None,
        known: Known::default(),
        accept_space: None,
    };
    match sign_in(&request_wrong, this.clone()) {
        Err(LockError::WrongPassword(message)) => assert!(message.contains("code"), "{message}"),
        other => panic!("{:?}", other.err()),
    }

    // The step used to turn it on is spent: the next one.
    let (next, _) = totp.code_at(unix_now() + 30);
    let right = TwoFactorAnswer {
        provider: 0,
        code: next.to_string(),
        remember: true,
    };
    let SignInOutcome::SignedIn(signed) = request(Some(right), None) else {
        panic!("in with the code")
    };
    let remembered = signed.remember_token.expect("a remember token");
    assert!(matches!(
        request(None, Some(&remembered)),
        SignInOutcome::SignedIn(_)
    ));
}

// ── Keys and the space ──────────────────────────────────────────────────────

#[test]
#[ignore = "needs a running UwULock Server: UWULOCK_TEST_SERVER=… (scripts/lock-live.sh)"]
fn devices_signing_in_at_once_end_up_with_one_extras_key_and_one_space() {
    let start = Arc::new(Barrier::new(4));
    let spaces: Vec<Space> = (0..4)
        .map(|_| {
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                signed_in("race").space
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    for space in &spaces[1..] {
        assert_eq!(space.id, spaces[0].id);
        assert_eq!(*space.key, *spaces[0].key);
    }

    // What the losers of such a race are told.
    let signed = signed_in("race");
    let raw = Raw::new();
    let token = access(&signed.lock);
    let (status, keys) = raw.call("GET", "/uwu/v1/keys", &token, None);
    assert_eq!(status, 200);
    let again = json!({
        "userKeyWrapped": keys["extrasKey"]["userKeyWrapped"],
        "privateKeyWrapped": keys["extrasKey"]["privateKeyWrapped"],
    });
    let (status, body) = raw.call("POST", "/uwu/v1/keys", &token, Some(again));
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("exists")),
        "{body}"
    );
    let other = SpaceKey::generate().wrap(&SymmetricKey::generate());
    let (status, body) = raw.call(
        "PUT",
        &format!("/uwu/v1/suite/spaces/{SPACE}"),
        &token,
        Some(json!({ "id": Uuid::new_v4(), "key": other })),
    );
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("exists")),
        "{body}"
    );
}

// ── Records ─────────────────────────────────────────────────────────────────

#[test]
#[ignore = "needs a running UwULock Server: UWULOCK_TEST_SERVER=… (scripts/lock-live.sh)"]
fn records_travel_in_the_contracts_shape_and_its_limits_hold() {
    let a = Store::open_in_memory().unwrap();
    let b = Store::open_in_memory().unwrap();
    let signed = signed_in("records");
    join(&a, &signed, Joining::SignIn);
    let (space, lock_a) = (copy(&signed.space), signed.lock);
    let lock_b = joined("records", &b);

    let mut host = draft("prox-1", "192.0.2.10");
    host.password = PasswordChange::Set {
        value: SecretText::from(Zeroizing::new("hunter2".to_string())),
    };
    a.save_host(host).unwrap();
    a.save_host(draft("nas", "192.0.2.20")).unwrap();
    let report = sync_once(&a, &lock_a).unwrap();
    assert!(report.complete && report.pushed >= 4, "{report:?}");
    sync_once(&b, &lock_b).unwrap();
    assert_eq!(names(&b), vec!["nas", "prox-1"]);
    let quiet = sync_once(&b, &lock_b).unwrap();
    assert_eq!((quiet.pulled, quiet.pushed), (0, 0));

    // On the wire: camelCase, base64, snake_case kinds.
    let raw = Raw::new();
    let token = access(&lock_a);
    let other = sealed(&space, EntityKind::PortForward, b"{\"local\":8080}");
    let pushed = lock_a.push(vec![other.clone()]).unwrap();
    assert_eq!(pushed.accepted.len(), 1);
    assert!(pushed.conflicts.is_empty());
    let (status, page) = raw.call(
        "GET",
        &format!("{}?since=0&limit=500", records_path(SPACE)),
        &token,
        None,
    );
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["object"], "suitePull");
    assert!(page["cursor"].is_u64(), "{page}");
    let records = page["records"].as_array().unwrap();
    let moved = records
        .iter()
        .find(|r| r["id"] == other.id.to_string())
        .unwrap();
    assert_eq!(moved["kind"], "port_forward");
    assert_eq!(moved["updatedAt"]["wallMs"], other.updated_at.wall_ms);
    assert_eq!(moved["seq"], pushed.accepted[0].seq);
    assert!(moved["baseSeq"].is_u64() && moved["nonce"].is_string() && moved["blob"].is_string());
    assert!(records.iter().any(|r| r["kind"] == "host"));
    for record in records {
        let blob = record["blob"].as_str().unwrap();
        assert!(!blob.contains("prox"));
    }

    // A stale base: the server's copy comes back as a conflict.
    let stale = lock_a.push(vec![other.clone()]).unwrap();
    assert!(stale.accepted.is_empty());
    assert_eq!(stale.conflicts.len(), 1);
    assert_eq!(stale.conflicts[0].seq, Some(pushed.accepted[0].seq));
    assert_eq!(stale.conflicts[0].updated_at, other.updated_at);

    // The limits.
    let record = |envelope: &Envelope| {
        serde_json::to_value(api::WireRecord::from_envelope(envelope)).unwrap()
    };
    let fresh = sealed(&space, EntityKind::Snippet, b"{}");
    let push = |body: Value| raw.call("POST", &records_path(SPACE), &token, Some(body));
    let (status, body) = push(json!({ "schema": 1, "records": [record(&fresh)] }));
    assert_eq!(
        (status, body["code"].as_str()),
        (400, Some("schema")),
        "{body}"
    );
    let mut unknown = record(&fresh);
    unknown["kind"] = "hologram".into();
    let (status, body) = push(json!({ "schema": 2, "records": [unknown] }));
    assert_eq!(
        (status, body["code"].as_str()),
        (400, Some("invalid")),
        "{body}"
    );
    let mut short = record(&fresh);
    short["nonce"] = "AAAA".into();
    let (status, body) = push(json!({ "schema": 2, "records": [short] }));
    assert_eq!(status, 400, "{body}");
    let big = sealed(&space, EntityKind::Snippet, &vec![b'x'; 256 * 1024]);
    let (status, body) = push(json!({ "schema": 2, "records": [record(&big)] }));
    assert!(matches!(status, 400 | 413), "{status} {body}");
    let many: Vec<Value> = (0..501)
        .map(|_| record(&sealed(&space, EntityKind::Snippet, b"{}")))
        .collect();
    let (status, body) = push(json!({ "schema": 2, "records": many }));
    assert!(matches!(status, 400 | 413), "{status} {body}");
    // Records sealed for another space than the account's (SV-L10): a 409
    // that is not about a record's id.
    let (status, body) = push(json!({
        "schema": 2,
        "spaceId": Uuid::new_v4(),
        "records": [record(&fresh)],
    }));
    assert_eq!(status, 409, "{body}");
    assert_ne!(body["code"].as_str(), Some(api::EXISTS), "{body}");
    // The other app's space, with this app's token.
    let (status, body) = raw.call(
        "POST",
        &records_path(OTHER_SPACE),
        &token,
        Some(json!({ "schema": 2, "records": [record(&fresh)] })),
    );
    assert_eq!(
        (status, body["code"].as_str()),
        (403, Some("scope")),
        "{body}"
    );
    // An id that is another space's.
    let web = web_token(&raw, "records");
    let theirs = SpaceKey::generate().wrap(&SymmetricKey::generate());
    let (status, body) = raw.call(
        "PUT",
        &format!("/uwu/v1/suite/spaces/{OTHER_SPACE}"),
        &web,
        Some(json!({ "id": Uuid::new_v4(), "key": theirs })),
    );
    assert_eq!(status, 200, "{body}");
    let (status, body) = raw.call(
        "POST",
        &records_path(OTHER_SPACE),
        &web,
        Some(json!({ "schema": 2, "records": [record(&other)] })),
    );
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("exists")),
        "{body}"
    );

    // The quota (the script sets 200 records an account): nothing of a
    // push over it is written.
    let (_, before) = raw.call("GET", "/uwu/v1/suite/spaces", &token, None);
    let held = before["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["records"].as_u64().unwrap())
        .sum::<u64>();
    let room = 200 - held as usize;
    let fill: Vec<Envelope> = (0..room)
        .map(|_| sealed(&space, EntityKind::Snippet, b"{}"))
        .collect();
    assert_eq!(lock_a.push(fill).unwrap().accepted.len(), room);
    let over = lock_a
        .push(vec![sealed(&space, EntityKind::Snippet, b"{}")])
        .unwrap_err();
    assert!(over.to_string().contains("quota"), "{over}");
}

#[test]
#[ignore = "needs a running UwULock Server: UWULOCK_TEST_SERVER=… (scripts/lock-live.sh)"]
fn a_rekey_elsewhere_sends_this_device_back_to_the_password() {
    let a = Store::open_in_memory().unwrap();
    let b = Store::open_in_memory().unwrap();
    let late = Store::open_in_memory().unwrap();
    let signed = signed_in("rekey");
    join(&a, &signed, Joining::SignIn);
    let (old, lock_a) = (copy(&signed.space), signed.lock);
    let lock_b = joined("rekey", &b);
    // Signed in, never synced: its cursor is 0.
    let lock_late = joined("rekey", &late);
    a.save_host(draft("prox-1", "192.0.2.10")).unwrap();
    sync_once(&a, &lock_a).unwrap();
    sync_once(&b, &lock_b).unwrap();
    assert_eq!(names(&b), vec!["prox-1"]);

    // Another device gives the space a new id and key (as after a lost
    // device), sealing everything again.
    let raw = Raw::new();
    let token = access(&lock_a);
    let (_, page) = raw.call(
        "GET",
        &format!("{}?since=0", records_path(SPACE)),
        &token,
        None,
    );
    let from = UnlockedVault::from_key(old.id, old.key.clone());
    let new = Space {
        id: Uuid::new_v4(),
        key: Zeroizing::new(*SpaceKey::generate().as_bytes()),
    };
    let to = UnlockedVault::from_key(new.id, new.key.clone());
    let records: Vec<Value> = page["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| {
            let wire: api::WireRecord = serde_json::from_value(record.clone()).unwrap();
            let seq = wire.seq.unwrap();
            let envelope = wire.into_envelope(old.id).unwrap();
            let plain = from
                .open_synced(
                    envelope.id,
                    envelope.kind,
                    envelope.updated_at,
                    envelope.deleted,
                    &Sealed {
                        nonce: envelope.nonce.clone(),
                        blob: envelope.blob.clone(),
                    },
                )
                .unwrap();
            // A manifest's id is made from the space's id: under the new
            // one it is nobody's, so it goes as a tombstone, and each device
            // writes a fresh one.
            let (updated_at, deleted, plain) = if envelope.kind == EntityKind::Manifest {
                (
                    Hlc::new(unix_now() * 1000, 0, 7),
                    true,
                    Zeroizing::new(Vec::new()),
                )
            } else {
                (envelope.updated_at, envelope.deleted, plain)
            };
            let Sealed { nonce, blob } = to
                .seal_synced(envelope.id, envelope.kind, updated_at, deleted, &plain)
                .unwrap();
            let again = Envelope {
                vault_id: new.id,
                updated_at,
                deleted,
                base_seq: seq,
                nonce,
                blob,
                seq: None,
                ..envelope
            };
            serde_json::to_value(api::WireRecord::from_envelope(&again)).unwrap()
        })
        .collect();
    let wrapped = SpaceKey::from_bytes(&new.key[..])
        .unwrap()
        .wrap(&extras_key("rekey"));
    let (status, body) = raw.call(
        "POST",
        &format!("/uwu/v1/suite/spaces/{SPACE}/rekey"),
        &token,
        Some(json!({ "id": new.id, "key": wrapped, "records": records })),
    );
    assert_eq!(status, 200, "{body}");

    // The other device's next pass: told to start over, and the space it
    // knows is gone — only the master password helps.
    let (status, page) = raw.call(
        "GET",
        &format!(
            "{}?since={}",
            records_path(SPACE),
            b.sync_state().unwrap().cursor
        ),
        &access(&lock_b),
        None,
    );
    assert_eq!(
        (status, page["reset"].as_bool()),
        (200, Some(true)),
        "{page}"
    );
    // With something to send, it asks before it writes there.
    b.save_host(draft("nas", "192.0.2.20")).unwrap();
    match sync_once(&b, &lock_b) {
        Err(SyncError::Transport(TransportError::SignIn(message))) => {
            assert!(message.contains("new key"), "{message}")
        }
        other => panic!("{other:?}"),
    }
    // Likewise for one that never synced before.
    match sync_once(&late, &lock_late) {
        Err(SyncError::Transport(TransportError::SignIn(message))) => {
            assert!(message.contains("new key"), "{message}")
        }
        other => panic!("{other:?}"),
    }
    assert!(names(&late).is_empty());

    // Signed in again: the new space is not taken without asking…
    let (server, email, password) = (server(), email("rekey"), password());
    let request = |accept_space| SignIn {
        server_url: &server,
        email: &email,
        password: &password,
        two_factor: None,
        remember_token: None,
        known: known(&b, &server, &email).unwrap(),
        accept_space,
    };
    match sign_in(&request(None), device()).unwrap() {
        SignInOutcome::SpaceChanged { was, now, .. } => {
            assert_eq!((was, now), (old.id, Some(new.id)))
        }
        _ => panic!("asked first"),
    }
    // …and once agreed: everything in it, and what waited here.
    let SignInOutcome::SignedIn(again) =
        sign_in(&request(Some(AcceptSpace::Listed(new.id))), device()).unwrap()
    else {
        panic!("in")
    };
    assert_eq!(again.space.id, new.id);
    join(&b, &again, Joining::SignIn);
    let report = sync_once(&b, &again.lock).unwrap();
    assert!(report.complete && report.apply.rejected == 0, "{report:?}");
    assert_eq!(names(&b), vec!["nas", "prox-1"]);
}

// ── Sessions ────────────────────────────────────────────────────────────────

#[test]
#[ignore = "needs a running UwULock Server: UWULOCK_TEST_SERVER=… (scripts/lock-live.sh)"]
fn a_refresh_keeps_the_client_id_and_a_removed_device_is_out() {
    let raw = Raw::new();
    let signed = signed_in("refresh");
    let refresh = signed.refresh_token.clone().expect("a refresh token");

    // Another app's client id, or none of the suite's, with this device's
    // refresh token: refused, and the session is still good.
    for client in [OTHER_CLIENT, "desktop"] {
        let (status, body) = raw.token(&[
            ("grant_type", "refresh_token"),
            ("client_id", client),
            ("refresh_token", refresh.as_str()),
        ]);
        assert_eq!(
            (status, body["error"].as_str()),
            (400, Some("invalid_grant")),
            "{client}: {body}"
        );
    }
    let (status, body) = raw.token(&[
        ("grant_type", "refresh_token"),
        ("client_id", CLIENT_ID),
        ("refresh_token", refresh.as_str()),
    ]);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["scope"], "uwu.suite offline_access");
    let token = claims(body["access_token"].as_str().unwrap());
    assert_eq!(token["client_id"], CLIENT_ID);
    assert_eq!(token["scope"], json!(["uwu.suite", "offline_access"]));

    // As the app does it: renewed, the new refresh token kept, and a restart
    // from the kept one works.
    let kept = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let lock = {
        let kept = Arc::clone(&kept);
        signed
            .lock
            .keeping(move |token| kept.lock().push(token.to_string()))
    };
    lock.renew().unwrap();
    let latest = kept.lock().last().cloned().expect("kept");
    let restarted = Lock::connect(&server(), lock.device().clone())
        .unwrap()
        .with_space(lock.space().unwrap())
        .with_refresh_token(Zeroizing::new(latest.clone()));
    restarted
        .pull(SyncCursor(0), 10)
        .expect("a session from the kept refresh token");

    // The web vault removes the device: its session is over.
    let web = web_token(&raw, "refresh");
    let id = lock.device().identifier.to_string();
    let (status, body) = raw.call("DELETE", &format!("/uwu/v1/devices/{id}"), &web, None);
    assert_eq!(status, 200, "{body}");
    let (status, body) = raw.token(&[
        ("grant_type", "refresh_token"),
        ("client_id", CLIENT_ID),
        ("refresh_token", &latest),
    ]);
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("invalid_grant")),
        "{body}"
    );
    assert!(matches!(
        restarted.pull(SyncCursor(0), 10),
        Err(TransportError::SignIn(_))
    ));
}

#[test]
#[ignore = "needs a running UwULock Server: UWULOCK_TEST_SERVER=… (scripts/lock-live.sh)"]
fn the_channel_hears_its_space_only_and_ends_when_the_device_is_removed() {
    let raw = Raw::new();
    let listening = signed_in("realtime");
    let id = listening.lock.device().identifier.to_string();
    let (seen, events) = mpsc::channel();
    let lock = listening.lock;
    let runner = std::thread::spawn(move || {
        live::run(&lock, &|| true, &mut |event| {
            let _ = seen.send(event);
        })
    });
    let wait = Duration::from_secs(10);
    assert_eq!(events.recv_timeout(wait).unwrap(), live::Event::Ready);

    // Another device of this app writes: heard.
    let writer = signed_in("realtime");
    let record = sealed(&writer.space, EntityKind::Snippet, b"{}");
    writer.lock.push(vec![record]).unwrap();
    assert_eq!(events.recv_timeout(wait).unwrap(), live::Event::Changed);

    // The other app's space and the vault: not this app's business.
    let web = web_token(&raw, "realtime");
    let key = SpaceKey::generate().wrap(&SymmetricKey::generate());
    let other_id = Uuid::new_v4();
    let (status, body) = raw.call(
        "PUT",
        &format!("/uwu/v1/suite/spaces/{OTHER_SPACE}"),
        &web,
        Some(json!({ "id": other_id, "key": key })),
    );
    assert_eq!(status, 200, "{body}");
    let theirs = Space {
        id: other_id,
        key: Zeroizing::new([7; 32]),
    };
    let record = serde_json::to_value(api::WireRecord::from_envelope(&sealed(
        &theirs,
        EntityKind::Host,
        b"{}",
    )))
    .unwrap();
    let (status, body) = raw.call(
        "POST",
        &records_path(OTHER_SPACE),
        &web,
        Some(json!({ "schema": 2, "records": [record] })),
    );
    assert_eq!(status, 200, "{body}");
    let (status, body) = raw.call(
        "POST",
        "/api/folders",
        &web,
        Some(json!({
            "name": crypto::EncString::encrypt(b"folder", &SymmetricKey::generate()).to_string()
        })),
    );
    assert_eq!(status, 200, "{body}");
    // Nothing comes for either. (Absence has no event to wait for; a second
    // is many times the server's own delay.)
    assert!(events.recv_timeout(Duration::from_secs(1)).is_err());

    // Removed in the web vault: the channel says so and ends.
    let (status, body) = raw.call("DELETE", &format!("/uwu/v1/devices/{id}"), &web, None);
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        runner.join().unwrap(),
        live::Ended::Logout("deviceRemoved".into())
    );
}

// ── The move ────────────────────────────────────────────────────────────────

#[test]
#[ignore = "needs a running UwULock Server: UWULOCK_TEST_SERVER=… (scripts/lock-live.sh)"]
fn the_move_from_a_real_uwusync_server() {
    let setup = crate::parse_setup(&var("UWUSYNC_TEST_SETUP")).expect("a setup code");
    // A device on UwUSync, with something in it.
    let store = Store::open_in_memory().unwrap();
    let (uwusync, paired) =
        crate::create_account(&store, &setup, b"uwusync password", "live test", |bytes| {
            Ok(bytes.to_vec())
        })
        .unwrap();
    let mut host = draft("prox-1", "192.0.2.10");
    host.password = PasswordChange::Set {
        value: SecretText::from(Zeroizing::new("hunter2".to_string())),
    };
    store.save_host(host).unwrap();
    store.save_host(draft("nas", "192.0.2.20")).unwrap();
    let gone = store.save_host(draft("old", "192.0.2.30")).unwrap();
    store.delete_host(gone.id).unwrap();
    let first = sync_once(&store, &uwusync).unwrap();
    assert!(first.complete && first.pushed >= 5, "{first:?}");

    let signed = signed_in("move");
    let report = copy_to_lock(&store, &uwusync, &signed.lock, &signed.space).unwrap();
    assert_eq!(report.read, report.copied, "{report:?}");
    assert_eq!(report.unreadable, 0);
    assert!(report.copied >= 5, "{report:?}");
    let again = copy_to_lock(&store, &uwusync, &signed.lock, &signed.space).unwrap();
    assert_eq!((again.copied, again.already_there), (0, report.copied));

    join(&store, &signed, Joining::Moved);
    let there = sync_once(&store, &signed.lock).unwrap();
    assert!(there.complete, "{there:?}");
    assert_eq!((there.conflicts, there.apply.applied), (0, 0), "{there:?}");

    // A device that only ever knew UwULock has it all.
    let other = Store::open_in_memory().unwrap();
    let lock = joined("move", &other);
    sync_once(&other, &lock).unwrap();
    assert_eq!(names(&other), vec!["nas", "prox-1"]);
    let prox = other
        .list_hosts()
        .unwrap()
        .into_iter()
        .find(|h| h.name == "prox-1")
        .unwrap();
    assert_eq!(&**other.reveal_host_password(prox.id).unwrap(), b"hunter2");

    // The last device on UwUSync cannot leave it (the app says so instead
    // of offering it).
    let devices = uwusync.devices().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].id, paired.device_id);
    assert!(uwusync.revoke(paired.device_id, None).is_err());
}
