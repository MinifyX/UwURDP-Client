//! A UwULock Server on 127.0.0.1, for the tests: the parts of the contract a
//! suite app uses (`docs/uwu-api.md` of UwULock-Server, §3, §5, §6), with
//! one account, and the rules for records taken from [`MemoryServer`], which
//! are UwUSync's and so the suite vault's.
//!
//! It can misbehave on purpose: run out an access token, end the session,
//! forget the extras key's user wrap as an official client's rotation does,
//! tell a pull to start over, give the space a new key (between a device's
//! look at the space and its push, too), keep a record back, refuse a push,
//! make up the headers of what it hands out.

use crate::engine::Transport;
use crate::lock::api::WireRecord;
use crate::MemoryServer;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use uuid::Uuid;
use uwulock_core::crypto::{self, EncString, Kdf, PrivateKey, SymmetricKey};
use uwurdp_proto::{SyncCursor, MAX_BATCH};

pub const EMAIL: &str = "nyu@example.com";
pub const PASSWORD: &str = "correct horse battery staple";
/// The code every two-step method accepts.
pub const CODE: &str = "123456";
const ITERATIONS: u32 = 5_000;

/// One RSA key for every test: making one takes a moment.
fn private_key_der() -> &'static [u8] {
    static KEY: OnceLock<Vec<u8>> = OnceLock::new();
    KEY.get_or_init(|| PrivateKey::generate().unwrap().to_der().unwrap().to_vec())
}

/// What a script tells the realtime side of the fake to do.
pub enum Live {
    /// Send this text frame.
    Send(String),
    /// Read the next text frame and hand it back.
    Expect(mpsc::Sender<String>),
    /// Close with this code.
    Close(u16),
}

pub struct Account {
    pub hash: String,
    pub protected_user_key: String,
    pub protected_private_key: String,
    pub two_factor: bool,
    /// Every device has a session of its own.
    pub access: std::collections::HashSet<String>,
    pub refresh: std::collections::HashSet<String>,
    pub issued: u64,
    pub extras: Option<(Option<String>, String)>,
    pub lost: bool,
    pub spaces: HashMap<String, (Uuid, String)>,
    /// Scopes and client ids the token requests asked for.
    pub logins: Vec<(String, String)>,
    pub reset_next_pull: bool,
    pub requests: Vec<String>,
    pub live: Option<mpsc::Receiver<Live>>,
    /// The script for the connection after the current one.
    pub live_next: Option<mpsc::Receiver<Live>>,
    /// The `spaceId` of every push, as it came.
    pub pushed_space_ids: Vec<Value>,
    /// The next push finds the space given a new key, as if that happened
    /// right after the device looked.
    pub rekey_at_next_push: bool,
    /// The next push is refused with 409 and this code.
    pub refuse_next_push: Option<&'static str>,
    /// Every pull says each record is newer than it is.
    pub forge_pulls: bool,
    /// The admin switched the suite vault off (feature switch `suite`).
    pub suite_off: bool,
    /// `/uwu/v1/info` has `switches`, as from UwULock Server 0.6.0-beta.2.
    pub switches: bool,
}

pub struct Fake {
    pub url: String,
    pub account: Arc<Mutex<Account>>,
    pub records: Arc<MemoryServer>,
    server: Arc<tiny_http::Server>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

impl Fake {
    pub fn start() -> Self {
        let kdf = Kdf::Pbkdf2 {
            iterations: ITERATIONS,
        };
        let master = crypto::master_key(PASSWORD, EMAIL, kdf).unwrap();
        let hash = crypto::master_password_hash(&master, PASSWORD);
        let user_key = SymmetricKey::generate();
        let protected_user_key =
            EncString::encrypt(&user_key.to_bytes(), &SymmetricKey::stretch(&master)).to_string();
        let protected_private_key = EncString::encrypt(private_key_der(), &user_key).to_string();
        let account = Arc::new(Mutex::new(Account {
            hash,
            protected_user_key,
            protected_private_key,
            two_factor: false,
            access: Default::default(),
            refresh: Default::default(),
            issued: 0,
            extras: None,
            lost: false,
            spaces: HashMap::new(),
            logins: Vec::new(),
            reset_next_pull: false,
            requests: Vec::new(),
            live: None,
            live_next: None,
            pushed_space_ids: Vec::new(),
            rekey_at_next_push: false,
            refuse_next_push: None,
            forge_pulls: false,
            suite_off: false,
            switches: true,
        }));
        let records = Arc::new(MemoryServer::new());
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        {
            let server = Arc::clone(&server);
            let account = Arc::clone(&account);
            let records = Arc::clone(&records);
            std::thread::spawn(move || {
                for request in server.incoming_requests() {
                    handle(request, &account, &records);
                }
            });
        }
        Self {
            url: format!("http://127.0.0.1:{port}"),
            account,
            records,
            server,
        }
    }

    pub fn with_two_factor(self) -> Self {
        self.account.lock().two_factor = true;
        self
    }

    /// The next request with the current access token is refused, as if it
    /// had run out.
    pub fn expire_access(&self) {
        self.account.lock().access.clear();
    }

    /// Signed out elsewhere: no token works any more, refresh included.
    pub fn end_session(&self) {
        let mut account = self.account.lock();
        account.access.clear();
        account.refresh.clear();
    }

    /// The admin switches the suite vault off (`true`) or on again, as
    /// UwULock Server does it: logins `invalid_client`, refreshes
    /// `invalid_grant`, the spaces 404 `feature_off`, the realtime channel
    /// closed with 4403 — and nothing forgotten.
    pub fn switch_suite(&self, off: bool) {
        self.account.lock().suite_off = off;
    }

    /// What an official client's rotation leaves behind.
    pub fn drop_user_wrap(&self) {
        if let Some((user, _)) = self.account.lock().extras.as_mut() {
            *user = None;
        }
    }

    pub fn space(&self) -> Option<(Uuid, String)> {
        self.account.lock().spaces.get("rdp").cloned()
    }

    pub fn requests(&self) -> Vec<String> {
        self.account.lock().requests.clone()
    }

    /// Hand the realtime side a script.
    pub fn live(&self) -> mpsc::Sender<Live> {
        let (tx, rx) = mpsc::channel();
        self.account.lock().live = Some(rx);
        tx
    }

    /// A script for the next connection, while the current one still runs.
    pub fn live_later(&self) -> mpsc::Sender<Live> {
        let (tx, rx) = mpsc::channel();
        self.account.lock().live_next = Some(rx);
        tx
    }
}

fn respond(request: tiny_http::Request, status: u16, body: Value) {
    let header: tiny_http::Header = "Content-Type: application/json".parse().unwrap();
    let _ = request.respond(
        tiny_http::Response::from_string(body.to_string())
            .with_status_code(status)
            .with_header(header),
    );
}

fn error(request: tiny_http::Request, status: u16, code: &str, message: &str) {
    respond(
        request,
        status,
        json!({ "object": "error", "message": message, "code": code, "validationErrors": null }),
    );
}

fn form(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn header(request: &tiny_http::Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.to_string().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

fn grant(account: &mut Account, remember: bool) -> Value {
    account.issued += 1;
    let access = format!("access-{}", account.issued);
    let refresh = format!("refresh-{}", account.issued);
    account.access.insert(access.clone());
    account.refresh.insert(refresh.clone());
    let mut body = json!({
        "access_token": access,
        "expires_in": 3600,
        "token_type": "Bearer",
        "refresh_token": refresh,
        "scope": "uwu.suite offline_access",
        "Key": account.protected_user_key,
        "PrivateKey": account.protected_private_key,
    });
    if remember {
        body["TwoFactorToken"] = json!("remembered");
    }
    body
}

fn handle(mut request: tiny_http::Request, account: &Mutex<Account>, records: &MemoryServer) {
    let method = request.method().as_str().to_string();
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let path = path.to_string();
    let query: HashMap<String, String> = form(query);
    let mut body = String::new();
    // An upgrade's "body" is the socket itself: never read it here.
    if method != "GET" {
        let _ = request.as_reader().read_to_string(&mut body);
    }
    account.lock().requests.push(format!("{method} {path}"));

    match (method.as_str(), path.as_str()) {
        ("POST", "/identity/accounts/prelogin") => respond(
            request,
            200,
            json!({ "kdf": 0, "kdfIterations": ITERATIONS }),
        ),
        ("POST", "/identity/connect/token") => token(request, &form(&body), account),
        ("POST", "/api/two-factor/send-email-login") => respond(request, 200, Value::Null),
        ("GET", "/uwu/v1/info") => {
            let account = account.lock();
            let mut body = json!({
                "object": "info", "name": "UwULock Server", "version": "0.6.0",
                "features": if account.suite_off { json!(["vault"]) } else { json!(["vault", "suite"]) },
            });
            if account.switches {
                body["switches"] = json!({ "suite": !account.suite_off, "families": true });
            }
            drop(account);
            respond(request, 200, body)
        }
        ("GET", "/uwu/v1/realtime") if account.lock().suite_off => realtime_closed(request, 4403),
        ("GET", "/uwu/v1/realtime") => realtime(request, account),
        _ if path.starts_with("/uwu/v1/suite/") && account.lock().suite_off => error(
            request,
            404,
            "feature_off",
            "This is switched off on this server.",
        ),
        _ if path.starts_with("/uwu/v1/") => {
            let bearer = header(&request, "Authorization");
            let authorised = {
                let account = account.lock();
                bearer
                    .as_deref()
                    .and_then(|b| b.strip_prefix("Bearer "))
                    .is_some_and(|token| account.access.contains(token))
            };
            if !authorised {
                return error(request, 401, "unauthorized", "Not logged in.");
            }
            api(request, &method, &path, &query, &body, account, records)
        }
        _ => error(request, 404, "not_found", "Not found."),
    }
}

fn token(request: tiny_http::Request, form: &HashMap<String, String>, account: &Mutex<Account>) {
    let mut account = account.lock();
    let field = |name: &str| form.get(name).cloned().unwrap_or_default();
    match field("grant_type").as_str() {
        "refresh_token" => {
            // Switched off: refused, but not used up.
            if account.suite_off {
                return respond(request, 400, json!({ "error": "invalid_grant" }));
            }
            // A refresh token works once.
            if field("client_id") != "uwurdp" || !account.refresh.remove(&field("refresh_token")) {
                return respond(request, 400, json!({ "error": "invalid_grant" }));
            }
            let body = grant(&mut account, false);
            respond(request, 200, body)
        }
        "password" => {
            account.logins.push((field("scope"), field("client_id")));
            if account.suite_off
                || field("scope") != "uwu.suite offline_access"
                || field("client_id") != "uwurdp"
            {
                return respond(request, 400, json!({ "error": "invalid_client" }));
            }
            if field("username") != EMAIL || field("password") != account.hash {
                return respond(
                    request,
                    400,
                    json!({
                        "error": "invalid_grant",
                        "error_description": "invalid_username_or_password",
                        "ErrorModel": { "Message": "Username or password is incorrect. Try again.", "Object": "error" }
                    }),
                );
            }
            let mut remember = false;
            if account.two_factor {
                let provider = field("twoFactorProvider");
                let code = field("twoFactorToken");
                let ok = match provider.as_str() {
                    "5" => code == "remembered",
                    "0" | "1" => code == CODE,
                    _ => false,
                };
                // A wrong code is refused like a wrong password, in words of
                // its own, as UwULock (and Bitwarden) do; a remembered device
                // that is not remembered any more is asked for a code again.
                if !ok && !code.is_empty() && provider != "5" {
                    let message = "The code from the authenticator app is wrong. Try again.";
                    return respond(
                        request,
                        400,
                        json!({
                            "error": "",
                            "message": message,
                            "ErrorModel": { "Message": message, "Object": "error" },
                            "object": "error",
                        }),
                    );
                }
                if !ok {
                    let body = json!({
                        "error": "invalid_grant",
                        "error_description": "Two factor required.",
                        "TwoFactorProviders": ["0", "1"],
                        "TwoFactorProviders2": { "0": null, "1": { "Email": "n***@example.com" } },
                    });
                    return respond(request, 400, body);
                }
                remember = field("twoFactorRemember") == "1";
            }
            let body = grant(&mut account, remember);
            respond(request, 200, body)
        }
        _ => respond(request, 400, json!({ "error": "unsupported_grant_type" })),
    }
}

fn keys_body(account: &Account) -> Value {
    json!({
        "object": "uwuKeys",
        "extrasKey": account.extras.as_ref().map(|(user, private)| json!({
            "userKeyWrapped": user,
            "privateKeyWrapped": private,
            "revisionDate": "2026-09-28T12:00:00.000000Z",
        })),
        "lost": account.lost,
    })
}

fn space_body(name: &str, id: Uuid, key: &str) -> Value {
    json!({
        "object": "suiteSpace", "space": name, "id": id, "key": key,
        "records": 0, "bytes": 0,
        "creationDate": "2026-09-28T12:00:00.000000Z",
        "revisionDate": "2026-09-28T12:00:00.000000Z",
    })
}

fn api(
    request: tiny_http::Request,
    method: &str,
    path: &str,
    query: &HashMap<String, String>,
    body: &str,
    account: &Mutex<Account>,
    records: &MemoryServer,
) {
    let json: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let mut account = account.lock();
    match (method, path) {
        ("GET", "/uwu/v1/keys") => respond(request, 200, keys_body(&account)),
        ("POST", "/uwu/v1/keys") => {
            if account.extras.is_some() {
                return error(request, 409, "exists", "There is one already.");
            }
            account.extras = Some((
                json["userKeyWrapped"].as_str().map(str::to_string),
                json["privateKeyWrapped"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            ));
            respond(request, 200, keys_body(&account))
        }
        // Every key here is made with its private wrap.
        ("PUT", "/uwu/v1/keys/private-wrap") => {
            error(request, 409, "exists", "It is wrapped already.")
        }
        ("PUT", "/uwu/v1/keys/user-wrap") => match account.extras.as_mut() {
            Some((user @ None, _)) => {
                *user = json["userKeyWrapped"].as_str().map(str::to_string);
                respond(request, 200, keys_body(&account))
            }
            _ => error(request, 409, "exists", "It is wrapped already."),
        },
        ("GET", "/uwu/v1/suite/spaces") => {
            let data: Vec<Value> = account
                .spaces
                .iter()
                .map(|(name, (id, key))| space_body(name, *id, key))
                .collect();
            respond(
                request,
                200,
                json!({ "object": "list", "data": data, "continuationToken": null }),
            )
        }
        ("PUT", "/uwu/v1/suite/spaces/rdp") => {
            if account.spaces.contains_key("rdp") {
                return error(request, 409, "exists", "The space exists.");
            }
            let id: Uuid = serde_json::from_value(json["id"].clone()).unwrap();
            let key = json["key"].as_str().unwrap().to_string();
            account.spaces.insert("rdp".into(), (id, key.clone()));
            respond(request, 200, space_body("rdp", id, &key))
        }
        ("GET", "/uwu/v1/suite/spaces/rdp/records") => {
            if std::mem::take(&mut account.reset_next_pull) {
                let cursor = query["since"].parse::<u64>().unwrap();
                return respond(
                    request,
                    200,
                    json!({ "object": "suitePull", "reset": true, "records": [], "cursor": cursor, "hasMore": false }),
                );
            }
            let since = query["since"].parse().unwrap();
            let limit = query["limit"].parse::<usize>().unwrap().min(MAX_BATCH);
            let page = records.pull(SyncCursor(since), limit).unwrap();
            let mut wire: Vec<WireRecord> = page
                .envelopes
                .iter()
                .map(WireRecord::from_envelope)
                .collect();
            if account.forge_pulls {
                for record in &mut wire {
                    record.updated_at.wall_ms += 3_600_000;
                }
            }
            respond(
                request,
                200,
                json!({ "object": "suitePull", "reset": false, "records": wire, "cursor": page.cursor.0, "hasMore": page.has_more }),
            )
        }
        ("POST", "/uwu/v1/suite/spaces/rdp/records") => {
            if json["schema"] != 2 {
                return error(request, 400, "schema", "Unknown schema.");
            }
            account.pushed_space_ids.push(json["spaceId"].clone());
            if let Some(code) = account.refuse_next_push.take() {
                return error(request, 409, code, "Refused.");
            }
            if std::mem::take(&mut account.rekey_at_next_push) {
                account.spaces.get_mut("rdp").unwrap().0 = Uuid::new_v4();
            }
            let space = account.spaces["rdp"].0;
            // Optional, for clients from before it (SV-L10 of UwULock).
            if !json["spaceId"].is_null() && json["spaceId"] != json!(space) {
                return error(
                    request,
                    409,
                    crate::lock::api::SPACE_CHANGED,
                    "The records are sealed for another space.",
                );
            }
            let wire: Vec<WireRecord> = serde_json::from_value(json["records"].clone()).unwrap();
            let envelopes = wire
                .into_iter()
                .map(|record| record.into_envelope(space).unwrap())
                .collect();
            let answer = records.push(envelopes).unwrap();
            let conflicts: Vec<WireRecord> = answer
                .conflicts
                .iter()
                .map(WireRecord::from_envelope)
                .collect();
            respond(
                request,
                200,
                json!({ "object": "suitePush", "accepted": answer.accepted, "conflicts": conflicts, "cursor": answer.cursor.0 }),
            )
        }
        _ => error(request, 404, "not_found", "Not found."),
    }
}

/// The realtime channel: the handshake by hand, then whatever the test's
/// script says.
/// Takes the WebSocket and closes it at once with `code`.
fn realtime_closed(request: tiny_http::Request, code: u16) {
    let (sender, script) = mpsc::channel();
    sender.send(Live::Close(code)).unwrap();
    drop(sender);
    serve_live(request, Some(script));
}

fn realtime(request: tiny_http::Request, account: &Mutex<Account>) {
    let script = {
        let mut account = account.lock();
        let script = account.live.take();
        account.live = account.live_next.take();
        script
    };
    serve_live(request, script);
}

fn serve_live(request: tiny_http::Request, script: Option<mpsc::Receiver<Live>>) {
    let key = header(&request, "Sec-WebSocket-Key").unwrap_or_default();
    let protocol = header(&request, "Sec-WebSocket-Protocol").unwrap_or_default();
    let Some(script) = script else {
        return error(request, 404, "not_found", "Not found.");
    };
    if !protocol.split(',').any(|p| p.trim() == "uwu.realtime.v1") {
        return error(request, 400, "invalid", "No subprotocol.");
    }
    let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
    let response = tiny_http::Response::empty(101)
        .with_header(
            format!("Sec-WebSocket-Accept: {accept}")
                .parse::<tiny_http::Header>()
                .unwrap(),
        )
        .with_header(
            "Sec-WebSocket-Protocol: uwu.realtime.v1"
                .parse::<tiny_http::Header>()
                .unwrap(),
        );
    let stream = request.upgrade("websocket", response);
    std::thread::spawn(move || {
        let mut socket = tungstenite::WebSocket::from_raw_socket(
            stream,
            tungstenite::protocol::Role::Server,
            None,
        );
        for step in script {
            match step {
                Live::Send(text) => {
                    if socket.send(tungstenite::Message::text(text)).is_err() {
                        return;
                    }
                }
                Live::Expect(reply) => loop {
                    match socket.read() {
                        Ok(tungstenite::Message::Text(text)) => {
                            let _ = reply.send(text.as_str().to_string());
                            break;
                        }
                        Ok(_) => continue,
                        Err(_) => return,
                    }
                },
                Live::Close(code) => {
                    let _ = socket.close(Some(tungstenite::protocol::CloseFrame {
                        code: code.into(),
                        reason: "".into(),
                    }));
                    let _ = socket.flush();
                    // Until the client answers the close.
                    while socket.read().is_ok() {}
                    return;
                }
            }
        }
    });
}
