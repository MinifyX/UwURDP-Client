//! Text clipboard in both directions (CLIPRDR), backed by `arboard`.
//!
//! IronRDP's [`CliprdrBackend`] callbacks run inside the session task, in the
//! middle of processing a PDU, so they must not touch the OS clipboard
//! themselves: that can block (X11 round trips, another app holding the
//! Windows clipboard open). Instead a small worker thread owns the local
//! clipboard, and the two sides talk through channels:
//!
//! - remote copy → the backend asks the server for `CF_UNICODETEXT` right
//!   away → the answer goes to the worker, which puts it on the local
//!   clipboard;
//! - local copy → the worker notices (session start, `clipboard_changed`
//!   from the page, or its once-a-second poll) and announces a format list →
//!   when the server asks for the data, the worker reads the text and hands
//!   the response back to the session.
//!
//! On Linux (X11) the thread also keeps the `arboard::Clipboard` alive for
//! the whole session, which is what keeps text we copied available to other
//! applications.
//!
//! Every failure here is logged and swallowed: a broken clipboard must never
//! take the session down with it.

use ironrdp_cliprdr::backend::{ClipboardMessage, CliprdrBackend};
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest,
    FileContentsResponse, FormatDataRequest, FormatDataResponse, LockDataId,
    OwnedFormatDataResponse,
};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;
use tracing::{debug, warn};

/// How often the worker looks for local clipboard changes.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// The local clipboard, as the worker needs it. Behind a trait so the logic
/// is testable without touching the real one.
pub(crate) trait LocalClipboard {
    fn get_text(&mut self) -> Option<String>;
    fn set_text(&mut self, text: String) -> Result<(), String>;
}

struct Arboard(arboard::Clipboard);

impl LocalClipboard for Arboard {
    fn get_text(&mut self) -> Option<String> {
        self.0.get_text().ok()
    }

    fn set_text(&mut self, text: String) -> Result<(), String> {
        self.0.set_text(text).map_err(|e| e.to_string())
    }
}

fn open_system_clipboard() -> Option<Box<dyn LocalClipboard>> {
    match arboard::Clipboard::new() {
        Ok(clipboard) => Some(Box::new(Arboard(clipboard))),
        Err(e) => {
            warn!(error = %e, "local clipboard unavailable; clipboard sharing is off");
            None
        }
    }
}

#[derive(Debug)]
pub(crate) enum WorkerMsg {
    /// The channel asks for our initial format list; it must be answered
    /// (even with an empty list) or the channel never finishes initializing.
    Initial,
    /// Re-announce the local text even if it did not change (the page
    /// regained focus: the user may have copied something the poll missed).
    Reannounce,
    /// The server wants our text.
    Serve,
    /// Text copied on the server, to be put on the local clipboard.
    Store(String),
}

/// Where the worker sends messages for the session loop.
pub(crate) type Outbox = Box<dyn Fn(ClipboardMessage) + Send>;

/// Handle to the clipboard worker. The thread ends once every handle (the
/// backend's and the session's) is dropped.
#[derive(Debug, Clone)]
pub(crate) struct ClipboardHandle {
    tx: mpsc::Sender<WorkerMsg>,
}

impl ClipboardHandle {
    pub(crate) fn send(&self, msg: WorkerMsg) {
        if self.tx.send(msg).is_err() {
            debug!("clipboard worker is gone");
        }
    }
}

/// Starts the worker on the system clipboard.
pub(crate) fn spawn_worker(outbox: Outbox) -> ClipboardHandle {
    spawn_worker_with(open_system_clipboard, outbox, POLL_INTERVAL)
}

pub(crate) fn spawn_worker_with(
    open: impl FnOnce() -> Option<Box<dyn LocalClipboard>> + Send + 'static,
    outbox: Outbox,
    poll: Duration,
) -> ClipboardHandle {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("uwurdp-clipboard".into())
        .spawn(move || run_worker(open(), rx, &outbox, poll));
    if let Err(e) = spawned {
        warn!(error = %e, "cannot start the clipboard thread");
    }
    ClipboardHandle { tx }
}

fn unicode_text_format() -> ClipboardFormat {
    ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)
}

fn run_worker(
    mut clipboard: Option<Box<dyn LocalClipboard>>,
    rx: mpsc::Receiver<WorkerMsg>,
    outbox: &Outbox,
    poll: Duration,
) {
    let read = |clipboard: &mut Option<Box<dyn LocalClipboard>>| {
        clipboard
            .as_mut()
            .and_then(|c| c.get_text())
            .filter(|t| !t.is_empty())
    };
    // What the local clipboard held last time we looked (or what we put
    // there ourselves), so we announce real changes only — and never echo
    // the server's own text back to it.
    let mut last_seen = None;
    let mut initialized = false;

    loop {
        match rx.recv_timeout(poll) {
            Ok(WorkerMsg::Initial) => {
                let text = read(&mut clipboard);
                let formats = if text.is_some() {
                    vec![unicode_text_format()]
                } else {
                    Vec::new()
                };
                outbox(ClipboardMessage::SendInitiateCopy(formats));
                last_seen = text;
                initialized = true;
            }
            Ok(WorkerMsg::Reannounce) => {
                let text = read(&mut clipboard);
                if initialized && text.is_some() {
                    outbox(ClipboardMessage::SendInitiateCopy(vec![
                        unicode_text_format(),
                    ]));
                }
                last_seen = text;
            }
            Ok(WorkerMsg::Serve) => {
                let response = match read(&mut clipboard) {
                    Some(text) => OwnedFormatDataResponse::new_unicode_string(&to_crlf(&text)),
                    None => OwnedFormatDataResponse::new_error(),
                };
                outbox(ClipboardMessage::SendFormatData(response));
            }
            Ok(WorkerMsg::Store(text)) => {
                let text = from_remote_line_endings(&text);
                if let Some(c) = clipboard.as_mut() {
                    match c.set_text(text.clone()) {
                        Ok(()) => last_seen = Some(text),
                        Err(e) => warn!(error = %e, "cannot set the local clipboard"),
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if !initialized {
                    continue;
                }
                let text = read(&mut clipboard);
                if text.is_some() && text != last_seen {
                    outbox(ClipboardMessage::SendInitiateCopy(vec![
                        unicode_text_format(),
                    ]));
                }
                last_seen = text;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    debug!("clipboard worker stopped");
}

/// Windows expects CRLF in `CF_UNICODETEXT`; plenty of Windows apps show a
/// bare LF as nothing at all.
fn to_crlf(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 16);
    let mut previous = '\0';
    for c in text.chars() {
        if c == '\n' && previous != '\r' {
            out.push('\r');
        }
        out.push(c);
        previous = c;
    }
    out
}

/// Back to the local convention: CRLF stays on Windows, becomes LF elsewhere.
fn from_remote_line_endings(text: &str) -> String {
    if cfg!(windows) {
        text.to_owned()
    } else {
        text.replace("\r\n", "\n")
    }
}

/// The CLIPRDR backend: forwards everything to the worker or the session.
pub(crate) struct TextClipboardBackend {
    worker: ClipboardHandle,
    /// Straight to the session loop, for answers that need no clipboard access.
    session: Outbox,
}

impl std::fmt::Debug for TextClipboardBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextClipboardBackend")
            .finish_non_exhaustive()
    }
}

impl TextClipboardBackend {
    pub(crate) fn new(worker: ClipboardHandle, session: Outbox) -> Self {
        Self { worker, session }
    }
}

impl ironrdp_core::AsAny for TextClipboardBackend {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl CliprdrBackend for TextClipboardBackend {
    fn temporary_directory(&self) -> &str {
        // Only used for file transfers, which we do not offer.
        ".cliprdr"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
    }

    fn on_ready(&mut self) {}

    fn on_request_format_list(&mut self) {
        self.worker.send(WorkerMsg::Initial);
    }

    fn on_process_negotiated_capabilities(&mut self, _: ClipboardGeneralCapabilityFlags) {}

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        // Fetch eagerly: text is small, and the local clipboard has no way
        // to render lazily across the network anyway.
        if available_formats
            .iter()
            .any(|f| f.id() == ClipboardFormatId::CF_UNICODETEXT)
        {
            (self.session)(ClipboardMessage::SendInitiatePaste(
                ClipboardFormatId::CF_UNICODETEXT,
            ));
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        if request.format == ClipboardFormatId::CF_UNICODETEXT {
            self.worker.send(WorkerMsg::Serve);
        } else {
            (self.session)(ClipboardMessage::SendFormatData(
                OwnedFormatDataResponse::new_error(),
            ));
        }
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            debug!("the server could not deliver its clipboard text");
            return;
        }
        match response.to_unicode_string() {
            Ok(text) => {
                let text = text.trim_end_matches('\0').to_owned();
                self.worker.send(WorkerMsg::Store(text));
            }
            Err(e) => warn!(error = %e, "undecodable clipboard text from the server"),
        }
    }

    fn on_file_contents_request(&mut self, _: FileContentsRequest) {}

    fn on_file_contents_response(&mut self, _: FileContentsResponse<'_>) {}

    fn on_lock(&mut self, _: LockDataId) {}

    fn on_unlock(&mut self, _: LockDataId) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;

    #[derive(Clone, Default)]
    struct FakeClipboard(Arc<Mutex<Option<String>>>);

    impl LocalClipboard for FakeClipboard {
        fn get_text(&mut self) -> Option<String> {
            self.0.lock().clone()
        }

        fn set_text(&mut self, text: String) -> Result<(), String> {
            *self.0.lock() = Some(text);
            Ok(())
        }
    }

    /// What the worker told the session, simplified for comparison.
    #[derive(Debug, PartialEq)]
    enum Sent {
        Announce(Vec<u32>),
        Data(Option<String>),
        Paste(u32),
        Other,
    }

    fn simplify(msg: ClipboardMessage) -> Sent {
        match msg {
            ClipboardMessage::SendInitiateCopy(formats) => {
                Sent::Announce(formats.iter().map(|f| f.id().value()).collect())
            }
            ClipboardMessage::SendFormatData(response) => Sent::Data(if response.is_error() {
                None
            } else {
                response
                    .to_unicode_string()
                    .ok()
                    .map(|s| s.trim_end_matches('\0').to_owned())
            }),
            ClipboardMessage::SendInitiatePaste(format) => Sent::Paste(format.value()),
            _ => Sent::Other,
        }
    }

    struct Rig {
        local: FakeClipboard,
        handle: ClipboardHandle,
        sent: std::sync::mpsc::Receiver<Sent>,
    }

    fn rig(initial: Option<&str>) -> Rig {
        let local = FakeClipboard::default();
        *local.0.lock() = initial.map(str::to_owned);
        let (tx, sent) = std::sync::mpsc::channel();
        let tx = Mutex::new(tx);
        let clipboard = local.clone();
        let handle = spawn_worker_with(
            move || Some(Box::new(clipboard) as Box<dyn LocalClipboard>),
            Box::new(move |msg| {
                let _ = tx.lock().send(simplify(msg));
            }),
            Duration::from_millis(20),
        );
        Rig {
            local,
            handle,
            sent,
        }
    }

    fn next(rig: &Rig) -> Sent {
        rig.sent
            .recv_timeout(Duration::from_secs(2))
            .expect("the worker said nothing")
    }

    fn quiet(rig: &Rig) -> bool {
        rig.sent.recv_timeout(Duration::from_millis(100)).is_err()
    }

    #[test]
    fn initial_announce_lists_text_only_when_there_is_some() {
        let with_text = rig(Some("hello"));
        with_text.handle.send(WorkerMsg::Initial);
        assert_eq!(next(&with_text), Sent::Announce(vec![13]));

        let empty = rig(None);
        empty.handle.send(WorkerMsg::Initial);
        // Must still answer, or the channel never becomes ready.
        assert_eq!(next(&empty), Sent::Announce(vec![]));
    }

    #[test]
    fn serving_sends_the_local_text_with_crlf() {
        let r = rig(Some("a\nb"));
        r.handle.send(WorkerMsg::Serve);
        assert_eq!(next(&r), Sent::Data(Some("a\r\nb".into())));
    }

    #[test]
    fn serving_an_empty_clipboard_is_an_error_response() {
        let r = rig(None);
        r.handle.send(WorkerMsg::Serve);
        assert_eq!(next(&r), Sent::Data(None));
    }

    #[test]
    fn a_local_change_is_announced_by_the_poll() {
        let r = rig(Some("one"));
        r.handle.send(WorkerMsg::Initial);
        assert_eq!(next(&r), Sent::Announce(vec![13]));
        assert!(quiet(&r), "unchanged text must not be re-announced");
        *r.local.0.lock() = Some("two".into());
        assert_eq!(next(&r), Sent::Announce(vec![13]));
    }

    #[test]
    fn remote_text_is_stored_and_not_echoed_back() {
        let r = rig(None);
        r.handle.send(WorkerMsg::Initial);
        assert_eq!(next(&r), Sent::Announce(vec![]));
        r.handle.send(WorkerMsg::Store("from server".into()));
        assert!(quiet(&r), "the poll must not announce what the server sent");
        assert_eq!(r.local.0.lock().as_deref(), Some("from server"));
    }

    #[test]
    fn reannounce_forces_an_announcement() {
        let r = rig(Some("same"));
        r.handle.send(WorkerMsg::Initial);
        assert_eq!(next(&r), Sent::Announce(vec![13]));
        r.handle.send(WorkerMsg::Reannounce);
        assert_eq!(next(&r), Sent::Announce(vec![13]));
    }

    #[test]
    fn nothing_is_announced_before_the_channel_asked() {
        let r = rig(Some("early"));
        r.handle.send(WorkerMsg::Reannounce);
        assert!(quiet(&r));
    }

    #[test]
    fn a_missing_clipboard_still_answers_the_channel() {
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Mutex::new(tx);
        let handle = spawn_worker_with(
            || None,
            Box::new(move |msg| {
                let _ = tx.lock().send(simplify(msg));
            }),
            Duration::from_millis(20),
        );
        handle.send(WorkerMsg::Initial);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).expect("answer"),
            Sent::Announce(vec![])
        );
        handle.send(WorkerMsg::Serve);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).expect("answer"),
            Sent::Data(None)
        );
    }

    #[test]
    fn backend_pastes_remote_text_and_refuses_other_formats() {
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Mutex::new(tx);
        let r = rig(None);
        let mut backend = TextClipboardBackend::new(
            r.handle.clone(),
            Box::new(move |msg| {
                let _ = tx.lock().send(simplify(msg));
            }),
        );
        backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_DIB)]);
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        backend.on_remote_copy(&[
            ClipboardFormat::new(ClipboardFormatId::CF_DIB),
            ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT),
        ]);
        assert_eq!(rx.recv().expect("paste"), Sent::Paste(13));
        backend.on_format_data_request(FormatDataRequest {
            format: ClipboardFormatId::CF_DIB,
        });
        assert_eq!(rx.recv().expect("refusal"), Sent::Data(None));
    }

    #[test]
    fn line_endings() {
        assert_eq!(to_crlf("a\nb\r\nc"), "a\r\nb\r\nc");
        if !cfg!(windows) {
            assert_eq!(from_remote_line_endings("a\r\nb"), "a\nb");
        }
    }
}
