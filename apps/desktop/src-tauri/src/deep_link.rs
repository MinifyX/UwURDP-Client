//! `uwurdp://connect/<host-id>`: a link that opens the desktop of one host,
//! as a double click on it in the list would.
//!
//! Only the host's record id travels in the link — never an address, a login
//! or `.rdp` settings. A link anyone can put on a web page must not be able to
//! point UwURDP at a server of their choosing; an id can only name a host the
//! person already has. Anything that is not exactly this form is refused.
//!
//! One UwURDP runs at a time (`tauri-plugin-single-instance`): a link opened
//! while it runs is handed to the running one, which comes to the front. The
//! link waits here until the page takes it, so one that started UwURDP is not
//! lost while the page is still loading.

use crate::sync::{self, Sync};
use crate::AppState;
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;
use uwurdp_store::VaultStatus;

/// The scheme, as registered with the system (see `tauri.conf.json`).
pub(crate) const SCHEME: &str = "uwurdp";

/// What a link asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum DeepLink {
    /// Connect to the host with this record id.
    Connect { id: Uuid },
    /// A `uwurdp:` link of any other form. Refused, but the page says so.
    Invalid,
}

/// Reads a link. `None` for one that isn't `uwurdp:` at all (an argument the
/// system passed for something else).
///
/// Accepted is exactly `uwurdp://connect/<uuid>`, optionally with one trailing
/// slash: no user, port, query or fragment, the id in its usual hyphenated
/// form. Scheme and `connect` in any case; the id comes out lowercase.
pub(crate) fn parse(link: &str) -> Option<DeepLink> {
    let (scheme, rest) = link.split_once(':')?;
    if !scheme.eq_ignore_ascii_case(SCHEME) {
        return None;
    }
    let connect = rest
        .strip_prefix("//")
        .and_then(|rest| rest.split_at_checked(7))
        .filter(|(host, _)| host.eq_ignore_ascii_case("connect"))
        .and_then(|(_, path)| path.strip_prefix('/'))
        .map(|id| id.strip_suffix('/').unwrap_or(id))
        .and_then(parse_id);
    Some(match connect {
        Some(id) => DeepLink::Connect { id },
        None => DeepLink::Invalid,
    })
}

/// A UUID in its hyphenated form only: 8-4-4-4-12 hex digits. `Uuid::parse_str`
/// also takes braces, `urn:uuid:` and no hyphens, none of which a link of ours
/// carries.
fn parse_id(text: &str) -> Option<Uuid> {
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        });
    if !shaped {
        return None;
    }
    Uuid::try_parse(text).ok().filter(|id| !id.is_nil())
}

/// The link the page hasn't taken yet. A newer one replaces it: the person
/// clicked the last one last.
#[derive(Default)]
pub(crate) struct Pending(Mutex<Option<DeepLink>>);

/// Links that came in: the newest `uwurdp:` one waits for the page, which is
/// told, and the window comes to the front.
pub(crate) fn received<S: AsRef<str>>(app: &AppHandle, links: impl IntoIterator<Item = S>) {
    let Some(link) = links.into_iter().filter_map(|l| parse(l.as_ref())).last() else {
        return;
    };
    match link {
        DeepLink::Connect { id } => tracing::info!(%id, "link to a host"),
        DeepLink::Invalid => tracing::warn!("refused a uwurdp: link of an unknown form"),
    }
    *app.state::<Pending>().0.lock() = Some(link);
    show(app);
    let _ = app.emit("deep-link", ());
}

/// Brings the main window to the front, also from minimized or hidden.
pub(crate) fn show(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// The waiting link, once.
#[tauri::command]
pub(crate) fn take_deep_link(pending: State<'_, Pending>) -> Option<DeepLink> {
    pending.0.lock().take()
}

/// How the sync for a link went.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum LinkSync {
    /// A pass ran; the host list may have changed.
    Done,
    /// This device doesn't sync, or the vault is locked: nothing to ask.
    Off,
    Failed {
        message: String,
    },
}

/// A host the link names isn't here (yet): one sync pass now, and the page
/// looks again. Waits for a pass already running instead of failing on it.
#[tauri::command]
pub(crate) async fn sync_for_link(app: AppHandle) -> LinkSync {
    let app2 = app.clone();
    let run = tauri::async_runtime::spawn_blocking(move || {
        let store = Arc::clone(&app2.state::<AppState>().store);
        let sync = app2.state::<Sync>();
        let paired = store.sync_state().map(|s| s.paired()).unwrap_or(false);
        let on_lock = store
            .lock_state()
            .map(|s| s.active && s.signed_in)
            .unwrap_or(false);
        let unlocked = matches!(store.vault_status(), Ok(VaultStatus::Unlocked));
        if !(paired || on_lock) || !unlocked {
            return LinkSync::Off;
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            // A pass that is already running may have started before the
            // host arrived on the server: ours comes after it.
            while sync.running.load(Ordering::SeqCst) {
                if Instant::now() > deadline {
                    return LinkSync::Failed {
                        message: "the sync is busy".into(),
                    };
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            match sync::pass(&app2, &store, &sync) {
                Ok(_) => return LinkSync::Done,
                Err(failure) if sync.running.load(Ordering::SeqCst) => {
                    // The worker got in between; wait for it again.
                    tracing::debug!(?failure, "sync pass for a link raced the worker");
                }
                Err(failure) => {
                    return LinkSync::Failed {
                        message: sync::describe(&failure),
                    }
                }
            }
        }
    });
    run.await.unwrap_or_else(|e| LinkSync::Failed {
        message: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0f8b2c4e-6a1d-4c3b-9e7f-112233445566";

    fn connect(id: &str) -> Option<DeepLink> {
        Some(DeepLink::Connect {
            id: Uuid::parse_str(id).unwrap(),
        })
    }

    #[test]
    fn takes_a_connect_link() {
        assert_eq!(parse(&format!("uwurdp://connect/{ID}")), connect(ID));
        assert_eq!(parse(&format!("uwurdp://connect/{ID}/")), connect(ID));
        assert_eq!(parse(&format!("UwURDP://CONNECT/{ID}")), connect(ID));
        let upper = ID.to_uppercase();
        assert_eq!(parse(&format!("uwurdp://connect/{upper}")), connect(ID));
    }

    #[test]
    fn ignores_what_is_not_ours() {
        for other in [
            "",
            "--relaunch",
            "/home/nyu/hosts.rdp",
            "C:\\Users\\nyu\\host.rdp",
            "uwussh://connect/0f8b2c4e-6a1d-4c3b-9e7f-112233445566",
            "https://example.com/uwurdp://connect/0f8b2c4e-6a1d-4c3b-9e7f-112233445566",
        ] {
            assert_eq!(parse(other), None, "{other}");
        }
    }

    #[test]
    fn refuses_everything_else() {
        let nil = Uuid::nil().to_string();
        for bad in [
            "uwurdp:".to_string(),
            "uwurdp://".to_string(),
            "uwurdp://connect".to_string(),
            "uwurdp://connect/".to_string(),
            format!("uwurdp:connect/{ID}"),
            format!("uwurdp:/connect/{ID}"),
            format!("uwurdp://open/{ID}"),
            format!("uwurdp://connectx/{ID}"),
            format!("uwurdp://connect//{ID}"),
            format!("uwurdp://connect/{ID}//"),
            format!("uwurdp://connect/{ID}?address=203.0.113.7"),
            format!("uwurdp://connect/{ID}#x"),
            format!("uwurdp://connect/{ID}/settings"),
            format!("uwurdp://user@connect/{ID}"),
            format!("uwurdp://connect:3389/{ID}"),
            format!("uwurdp://connect/{{{ID}}}"),
            format!("uwurdp://connect/urn:uuid:{ID}"),
            format!("uwurdp://connect/{}", ID.replace('-', "")),
            format!("uwurdp://connect/{}", &ID[..35]),
            format!("uwurdp://connect/{}g", &ID[..35]),
            format!("uwurdp://connect/%30{}", &ID[1..]),
            format!("uwurdp://connect/{nil}"),
            "uwurdp://connect/full%20address:s:203.0.113.7".to_string(),
            "uwurdp://connect/ä0f8b2c4-6a1d-4c3b-9e7f-11223344556".to_string(),
        ] {
            assert_eq!(parse(&bad), Some(DeepLink::Invalid), "{bad}");
        }
    }

    #[test]
    fn the_page_sees_kind_and_id() {
        let link = parse(&format!("uwurdp://connect/{ID}")).unwrap();
        assert_eq!(
            serde_json::to_value(link).unwrap(),
            serde_json::json!({ "kind": "connect", "id": ID })
        );
        assert_eq!(
            serde_json::to_value(DeepLink::Invalid).unwrap(),
            serde_json::json!({ "kind": "invalid" })
        );
    }

    #[test]
    fn a_link_waits_until_taken_once() {
        let pending = Pending::default();
        *pending.0.lock() = parse(&format!("uwurdp://connect/{ID}"));
        assert_eq!(pending.0.lock().take(), connect(ID));
        assert_eq!(pending.0.lock().take(), None);
    }
}
