//! The worker's logic against a fake clipboard (synchronous, no threads),
//! the backend's request queue, and both ends of the channel talking to each
//! other in-process: our client with its worker thread against IronRDP's
//! CLIPRDR server.

use super::*;
use ironrdp_cliprdr::pdu::{ClipboardFileAttributes, FileContentsFlags};
use ironrdp_cliprdr::CliprdrServer;
use ironrdp_svc::{SvcMessage, SvcProcessor};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

// ── A fake local clipboard ──────────────────────────────────────────────────

#[derive(Default)]
struct State {
    text: Option<String>,
    html: Option<String>,
    image: Option<Rgba>,
    files: Option<Vec<PathBuf>>,
    /// Like Windows' sequence number, when `counter` is on.
    counter: bool,
    token: u64,
    /// How often anything was read.
    reads: usize,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<State>>);

impl Fake {
    fn with_counter() -> Self {
        let fake = Fake::default();
        fake.0.lock().counter = true;
        fake
    }

    /// What a user copying something does: replaces everything.
    fn copy(&self, f: impl FnOnce(&mut State)) {
        let mut state = self.0.lock();
        state.text = None;
        state.html = None;
        state.image = None;
        state.files = None;
        state.token += 1;
        f(&mut state);
    }

    fn copy_text(&self, text: &str) {
        self.copy(|s| s.text = Some(text.into()));
    }
}

impl LocalClipboard for Fake {
    fn change_token(&mut self) -> Option<u64> {
        let state = self.0.lock();
        state.counter.then_some(state.token)
    }
    fn files(&mut self) -> Option<Vec<PathBuf>> {
        let mut s = self.0.lock();
        s.reads += 1;
        s.files.clone()
    }
    fn text(&mut self) -> Option<String> {
        let mut s = self.0.lock();
        s.reads += 1;
        s.text.clone()
    }
    fn html(&mut self) -> Option<String> {
        self.0.lock().html.clone()
    }
    fn image(&mut self) -> Option<Rgba> {
        let mut s = self.0.lock();
        s.reads += 1;
        s.image.clone()
    }
    fn set_text(&mut self, text: &str, html: Option<&str>) -> Result<(), String> {
        let html = html.map(str::to_owned);
        self.copy(|s| {
            s.text = Some(text.into());
            s.html = html;
        });
        Ok(())
    }
    fn set_image(&mut self, image: Rgba) -> Result<(), String> {
        self.copy(|s| s.image = Some(image));
        Ok(())
    }
    fn set_files(&mut self, files: &[PathBuf]) -> Result<(), String> {
        let files = files.to_vec();
        self.copy(|s| s.files = Some(files));
        Ok(())
    }
}

/// What reached the session, simplified for comparison.
#[derive(Debug, PartialEq)]
enum Sent {
    Announce(Vec<u32>),
    Data(Option<Vec<u8>>),
    Paste(u32),
    FileCopy(Vec<String>),
    FileRequest(u32, FileContentsFlags, u64, u32),
    FileResponse(u32, Option<Vec<u8>>),
    Status(Status),
}

fn simplify(out: Outgoing) -> Sent {
    let message = match out {
        Outgoing::Status(status) => return Sent::Status(status),
        Outgoing::Cliprdr(message) => message,
    };
    match message {
        ClipboardMessage::SendInitiateCopy(formats) => {
            Sent::Announce(formats.iter().map(|f| f.id().value()).collect())
        }
        ClipboardMessage::SendFormatData(response) => {
            Sent::Data((!response.is_error()).then(|| response.data().to_vec()))
        }
        ClipboardMessage::SendInitiatePaste(format) => Sent::Paste(format.value()),
        ClipboardMessage::SendInitiateFileCopy(files) => {
            Sent::FileCopy(files.into_iter().map(|f| f.name).collect())
        }
        ClipboardMessage::SendFileContentsRequest(r) => {
            Sent::FileRequest(r.stream_id, r.flags, r.position, r.requested_size)
        }
        ClipboardMessage::SendFileContentsResponse(r) => {
            Sent::FileResponse(r.stream_id(), (!r.is_error()).then(|| r.data().to_vec()))
        }
        ClipboardMessage::Error(e) => panic!("unexpected error {e}"),
    }
}

type Log = Arc<Mutex<Vec<Sent>>>;

fn collect(log: &Log) -> Outbox {
    let log = log.clone();
    Box::new(move |out| log.lock().push(simplify(out)))
}

fn take(log: &Log) -> Vec<Sent> {
    std::mem::take(&mut *log.lock())
}

struct Rig {
    fake: Fake,
    worker: Worker,
    log: Log,
    dir: tempfile::TempDir,
}

/// A worker whose channel is up, with file transfer negotiated.
fn rig(fake: Fake) -> Rig {
    let log = Log::default();
    let dir = tempfile::tempdir().expect("tempdir");
    let mut worker = Worker::new(
        Some(Box::new(fake.clone())),
        collect(&log),
        dir.path().join("session"),
    );
    worker.handle(WorkerMsg::Capabilities(
        ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED,
    ));
    worker.handle(WorkerMsg::Initial);
    assert_eq!(take(&log), vec![Sent::Announce(vec![])]);
    worker.handle(WorkerMsg::Ready);
    Rig {
        fake,
        worker,
        log,
        dir,
    }
}

const TEXT: u32 = 13;
const DIB: u32 = 8;
const HTML: u32 = 0xC0F0;

// ── The worker ──────────────────────────────────────────────────────────────

#[test]
fn the_initial_list_is_empty_and_ready_announces_what_is_there() {
    let fake = Fake::default();
    fake.copy_text("hello");
    let mut r = rig(fake);
    assert_eq!(take(&r.log), vec![Sent::Announce(vec![TEXT])]);
    // Unchanged: not again.
    r.worker.check(false);
    r.worker.check(true);
    assert_eq!(take(&r.log), vec![]);
}

#[test]
fn a_channel_started_over_gets_what_is_there_right_away() {
    let fake = Fake::default();
    fake.copy_text("still here");
    let mut r = rig(fake);
    take(&r.log);
    r.worker.handle(WorkerMsg::Initial);
    assert_eq!(take(&r.log), vec![Sent::Announce(vec![TEXT])]);
    r.fake.copy(|_| {});
    r.worker.check(false);
    r.worker.handle(WorkerMsg::Initial);
    assert_eq!(
        take(&r.log),
        vec![Sent::Announce(vec![])],
        "empty, but answered"
    );
}

#[test]
fn nothing_is_announced_before_the_channel_is_ready() {
    let fake = Fake::default();
    fake.copy_text("early");
    let log = Log::default();
    let dir = tempfile::tempdir().expect("tempdir");
    let mut worker = Worker::new(Some(Box::new(fake)), collect(&log), dir.path().into());
    worker.handle(WorkerMsg::Check { deep: true });
    worker.handle(WorkerMsg::Initial);
    worker.check(false);
    assert_eq!(take(&log), vec![Sent::Announce(vec![])]);
}

#[test]
fn a_local_change_is_announced_once() {
    let mut r = rig(Fake::default());
    assert_eq!(take(&r.log), vec![], "an empty clipboard is no news");
    r.fake.copy_text("one");
    r.worker.check(false);
    r.worker.check(false);
    assert_eq!(take(&r.log), vec![Sent::Announce(vec![TEXT])]);
    r.fake.copy_text("two");
    r.worker.check(false);
    assert_eq!(take(&r.log), vec![Sent::Announce(vec![TEXT])]);
}

#[test]
fn with_a_change_counter_an_unchanged_clipboard_is_not_read() {
    let fake = Fake::with_counter();
    fake.copy_text("x");
    let mut r = rig(fake);
    assert_eq!(take(&r.log), vec![Sent::Announce(vec![TEXT])]);
    let reads = r.fake.0.lock().reads;
    for _ in 0..5 {
        r.worker.check(false);
    }
    assert_eq!(r.fake.0.lock().reads, reads);
    // The same text copied again is a new copy: the counter says so.
    r.fake.copy_text("x");
    r.worker.check(false);
    assert_eq!(take(&r.log), vec![]);
    r.fake.copy(|s| s.image = Some(pixel()));
    r.worker.check(false);
    assert_eq!(take(&r.log), vec![Sent::Announce(vec![DIB])]);
}

fn pixel() -> Rgba {
    Rgba {
        width: 1,
        height: 1,
        bytes: vec![1, 2, 3, 255],
    }
}

#[test]
fn serving_reads_the_clipboard_then_and_always_answers() {
    let fake = Fake::default();
    fake.copy_text("a\nb");
    let mut r = rig(fake);
    take(&r.log);
    r.worker
        .handle(WorkerMsg::Serve(ClipboardFormatId::CF_UNICODETEXT));
    assert_eq!(
        take(&r.log),
        vec![Sent::Data(Some(formats::encode_utf16_text("a\r\nb")))]
    );
    for format in [HTML, DIB, 49_999] {
        r.worker.handle(WorkerMsg::Serve(ClipboardFormatId(format)));
    }
    assert_eq!(
        take(&r.log),
        vec![Sent::Data(None), Sent::Data(None), Sent::Data(None)]
    );
}

#[test]
fn a_missing_clipboard_still_answers_the_channel() {
    let log = Log::default();
    let dir = tempfile::tempdir().expect("tempdir");
    let mut worker = Worker::new(None, collect(&log), dir.path().into());
    worker.handle(WorkerMsg::Initial);
    worker.handle(WorkerMsg::Ready);
    worker.handle(WorkerMsg::Serve(ClipboardFormatId::CF_UNICODETEXT));
    assert_eq!(take(&log), vec![Sent::Announce(vec![]), Sent::Data(None)]);
}

#[test]
fn html_goes_with_its_text() {
    let fake = Fake::default();
    fake.copy(|s| {
        s.text = Some("bold".into());
        s.html = Some("<b>bold</b>".into());
    });
    let mut r = rig(fake);
    assert_eq!(take(&r.log), vec![Sent::Announce(vec![TEXT, HTML])]);
    r.worker.handle(WorkerMsg::Serve(HTML_FORMAT_ID));
    assert_eq!(
        take(&r.log),
        vec![Sent::Data(Some(formats::wrap_cf_html("<b>bold</b>")))]
    );
}

#[test]
fn without_a_counter_pictures_are_looked_at_on_focus_only() {
    let mut r = rig(Fake::default());
    r.fake.copy_text("text first");
    r.worker.check(false);
    assert_eq!(take(&r.log), vec![Sent::Announce(vec![TEXT])]);
    // The text going away is a change: the picture is found right away.
    r.fake.copy(|s| s.image = Some(pixel()));
    r.worker.check(false);
    assert_eq!(take(&r.log), vec![Sent::Announce(vec![DIB])]);
    // Picture to picture: the poll can't see it, the focus can.
    r.fake.copy(|s| {
        s.image = Some(Rgba {
            width: 1,
            height: 1,
            bytes: vec![9, 9, 9, 255],
        })
    });
    r.worker.check(false);
    assert_eq!(take(&r.log), vec![]);
    r.worker.handle(WorkerMsg::Check { deep: true });
    assert_eq!(take(&r.log), vec![Sent::Announce(vec![DIB])]);
    r.worker.handle(WorkerMsg::Serve(ClipboardFormatId::CF_DIB));
    let Some(Sent::Data(Some(dib))) = take(&r.log).pop() else {
        panic!("no picture");
    };
    assert_eq!(
        formats::dib_to_rgba(&dib).expect("dib").bytes,
        vec![9, 9, 9, 255]
    );
}

#[test]
fn what_the_server_copied_is_stored_and_never_echoed() {
    for counter in [false, true] {
        let fake = if counter {
            Fake::with_counter()
        } else {
            Fake::default()
        };
        let mut r = rig(fake);
        r.worker.handle(WorkerMsg::Store(RemoteContent {
            text: Some("from\r\nserver".into()),
            html: Some("<i>x</i>".into()),
            image: None,
        }));
        let expected = if cfg!(windows) {
            "from\r\nserver"
        } else {
            "from\nserver"
        };
        assert_eq!(r.fake.0.lock().text.as_deref(), Some(expected));
        assert_eq!(r.fake.0.lock().html.as_deref(), Some("<i>x</i>"));
        r.worker.handle(WorkerMsg::Store(RemoteContent {
            image: Some(pixel()),
            ..RemoteContent::default()
        }));
        assert_eq!(r.fake.0.lock().image, Some(pixel()));
        r.worker.check(false);
        r.worker.handle(WorkerMsg::Check { deep: true });
        assert_eq!(take(&r.log), vec![], "counter: {counter}");
    }
}

#[test]
fn local_files_are_offered_and_served() {
    let r_dir = tempfile::tempdir().expect("tempdir");
    let file = r_dir.path().join("notes.txt");
    std::fs::write(&file, b"0123456789").expect("write");
    let mut r = rig(Fake::default());
    r.fake.copy(|s| s.files = Some(vec![file.clone()]));
    r.worker.check(false);
    assert_eq!(take(&r.log), vec![Sent::FileCopy(vec!["notes.txt".into()])]);

    r.worker.handle(WorkerMsg::FileRequest(FileContentsRequest {
        stream_id: 4,
        index: 0,
        flags: FileContentsFlags::RANGE,
        position: 2,
        requested_size: 3,
        data_id: None,
    }));
    assert_eq!(
        take(&r.log),
        vec![Sent::FileResponse(4, Some(b"234".to_vec()))]
    );

    // Text copied afterwards withdraws the files.
    r.fake.copy_text("now text");
    r.worker.check(false);
    r.worker.handle(WorkerMsg::FileRequest(FileContentsRequest {
        stream_id: 5,
        index: 0,
        flags: FileContentsFlags::SIZE,
        position: 0,
        requested_size: 8,
        data_id: None,
    }));
    assert_eq!(
        take(&r.log),
        vec![Sent::Announce(vec![TEXT]), Sent::FileResponse(5, None)]
    );
}

#[test]
fn files_for_a_server_without_file_transfer() {
    let log = Log::default();
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = Fake::default();
    let mut worker = Worker::new(
        Some(Box::new(fake.clone())),
        collect(&log),
        dir.path().into(),
    );
    worker.handle(WorkerMsg::Capabilities(
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES,
    ));
    worker.handle(WorkerMsg::Initial);
    worker.handle(WorkerMsg::Ready);
    take(&log);
    fake.copy(|s| s.files = Some(vec![dir.path().to_owned()]));
    worker.check(false);
    // The server keeps nothing stale, and is offered nothing it can't take.
    assert_eq!(take(&log), vec![Sent::Announce(vec![])]);
    worker.handle(WorkerMsg::OfferFiles(vec![dir.path().to_owned()]));
    assert!(matches!(
        take(&log).as_slice(),
        [Sent::Status(Status::Failed {
            error: Failure::Unsupported,
            ..
        })]
    ));
}

#[test]
fn dropped_files_are_offered_without_touching_the_local_clipboard() {
    let fake = Fake::default();
    fake.copy_text("keep me");
    let mut r = rig(fake);
    take(&r.log);
    let file = r.dir.path().join("dropped.bin");
    std::fs::write(&file, b"x").expect("write");
    r.worker.handle(WorkerMsg::OfferFiles(vec![file]));
    assert_eq!(
        take(&r.log),
        vec![
            Sent::FileCopy(vec!["dropped.bin".into()]),
            Sent::Status(Status::Sent { files: 1 }),
        ]
    );
    // Focus coming back must not replace them with the unchanged text.
    r.worker.handle(WorkerMsg::Check { deep: true });
    assert_eq!(take(&r.log), vec![]);
    assert_eq!(r.fake.0.lock().text.as_deref(), Some("keep me"));
}

fn descriptor(name: &str, size: u64) -> FileDescriptor {
    FileDescriptor::new(name)
        .with_attributes(ClipboardFileAttributes::ARCHIVE)
        .with_file_size(size)
}

/// Answers the worker's file requests from `content` until it says
/// something else; returns that.
fn serve_download(r: &mut Rig, content: &[u8]) -> Vec<Sent> {
    let mut rest = Vec::new();
    loop {
        let sent = take(&r.log);
        if sent.is_empty() {
            return rest;
        }
        for s in sent {
            match s {
                Sent::FileRequest(id, flags, position, size) => {
                    assert!(flags.contains(FileContentsFlags::RANGE));
                    let start = position as usize;
                    let end = (start + size as usize).min(content.len());
                    r.worker.handle(WorkerMsg::FileResponse {
                        stream_id: id,
                        data: Some(content[start..end].to_vec()),
                    });
                }
                other => rest.push(other),
            }
        }
    }
}

#[test]
fn small_remote_files_are_downloaded_and_put_on_the_clipboard() {
    let fake = Fake::with_counter();
    let mut r = rig(fake);
    r.worker.handle(WorkerMsg::RemoteCopy);
    r.worker.handle(WorkerMsg::RemoteFiles {
        files: vec![descriptor("report.txt", 5)],
        clip_data_id: Some(1),
    });
    let rest = serve_download(&mut r, b"hello");
    assert_eq!(
        rest,
        vec![
            Sent::Status(Status::Downloading {
                files: 1,
                done: 0,
                total: 0
            }),
            Sent::Status(Status::Ready { files: 1 }),
        ]
    );
    let files = r
        .fake
        .0
        .lock()
        .files
        .clone()
        .expect("files on the clipboard");
    assert_eq!(files.len(), 1);
    assert!(files[0].starts_with(r.dir.path().join("session")));
    assert_eq!(std::fs::read(&files[0]).expect("read"), b"hello");
    // Our own files are not offered back to the server.
    r.worker.check(false);
    r.worker.handle(WorkerMsg::Check { deep: true });
    assert_eq!(take(&r.log), vec![]);
    // They outlive the session, so pasting after disconnecting still works.
    r.worker.shutdown();
    assert!(files[0].exists());
}

#[test]
fn big_remote_files_wait_for_the_user() {
    let mut r = rig(Fake::default());
    let big = AUTO_DOWNLOAD_LIMIT + 1;
    r.worker.handle(WorkerMsg::RemoteFiles {
        files: vec![descriptor("huge.iso", big)],
        clip_data_id: None,
    });
    assert_eq!(
        take(&r.log),
        vec![Sent::Status(Status::Offered {
            files: 1,
            total: Some(big)
        })]
    );
    r.worker.handle(WorkerMsg::Download);
    let sent = take(&r.log);
    assert_eq!(
        sent.iter()
            .filter(|s| matches!(s, Sent::FileRequest(..)))
            .count(),
        4,
        "a window of requests: {sent:?}"
    );
    // The server copying something else cancels it, folder and all.
    r.worker.handle(WorkerMsg::RemoteCopy);
    assert!(!r.dir.path().join("session").join("1").exists());
    r.worker.handle(WorkerMsg::Download);
    assert_eq!(take(&r.log), vec![], "the offer is gone too");
}

#[test]
fn a_local_copy_during_a_download_wins() {
    let mut r = rig(Fake::default());
    r.worker.handle(WorkerMsg::RemoteFiles {
        files: vec![descriptor("a.txt", 3)],
        clip_data_id: None,
    });
    let first = take(&r.log);
    r.fake.copy_text("mine");
    r.worker.check(false);
    for s in first {
        if let Sent::FileRequest(id, ..) = s {
            r.worker.handle(WorkerMsg::FileResponse {
                stream_id: id,
                data: Some(b"abc".to_vec()),
            });
        }
    }
    let sent = take(&r.log);
    assert!(sent.contains(&Sent::Announce(vec![TEXT])));
    assert!(!sent
        .iter()
        .any(|s| matches!(s, Sent::Status(Status::Ready { .. }))));
    assert_eq!(r.fake.0.lock().text.as_deref(), Some("mine"));
}

#[test]
fn a_failed_download_is_reported() {
    let mut r = rig(Fake::default());
    r.worker.handle(WorkerMsg::RemoteFiles {
        files: vec![descriptor("a.txt", 3)],
        clip_data_id: None,
    });
    let id = take(&r.log)
        .into_iter()
        .find_map(|s| match s {
            Sent::FileRequest(id, ..) => Some(id),
            _ => None,
        })
        .expect("a request");
    r.worker.handle(WorkerMsg::FileResponse {
        stream_id: id,
        data: None,
    });
    assert!(matches!(
        take(&r.log).as_slice(),
        [Sent::Status(Status::Failed {
            error: Failure::Download,
            ..
        })]
    ));
    assert_eq!(r.fake.0.lock().files, None);
}

// ── The backend ─────────────────────────────────────────────────────────────

/// A backend whose worker is a plain channel we read ourselves.
fn backend() -> (ClipboardBackend, mpsc::Receiver<WorkerMsg>, Log) {
    let (tx, rx) = mpsc::channel();
    let log = Log::default();
    (
        ClipboardBackend::new(ClipboardHandle { tx }, collect(&log)),
        rx,
        log,
    )
}

fn named(id: u32, name: ClipboardFormatName) -> ClipboardFormat {
    ClipboardFormat::new(ClipboardFormatId(id)).with_name(name)
}

#[test]
fn the_backend_fetches_text_and_html_one_request_at_a_time() {
    let (mut backend, rx, log) = backend();
    backend.on_remote_copy(&[
        ClipboardFormat::new(ClipboardFormatId::CF_DIB),
        ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT),
        named(0xC100, ClipboardFormatName::HTML),
    ]);
    assert!(matches!(rx.try_recv(), Ok(WorkerMsg::RemoteCopy)));
    assert_eq!(take(&log), vec![Sent::Paste(TEXT)]);
    backend.on_format_data_response(FormatDataResponse::new_data(formats::encode_utf16_text(
        "hi",
    )));
    assert_eq!(take(&log), vec![Sent::Paste(0xC100)]);
    assert!(
        rx.try_recv().is_err(),
        "nothing stored before the HTML is in"
    );
    backend.on_format_data_response(FormatDataResponse::new_data(formats::wrap_cf_html(
        "<u>hi</u>",
    )));
    match rx.try_recv() {
        Ok(WorkerMsg::Store(content)) => {
            assert_eq!(content.text.as_deref(), Some("hi"));
            assert_eq!(content.html.as_deref(), Some("<u>hi</u>"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_backend_prefers_files_then_text_then_pictures() {
    let (mut backend, _rx, log) = backend();
    backend.on_remote_copy(&[
        ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT),
        named(0xC0FE, ClipboardFormatName::FILE_LIST),
    ]);
    assert_eq!(take(&log), vec![Sent::Paste(0xC0FE)]);
    backend.on_remote_file_list(&[descriptor("a", 1)], Some(2));

    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_DIBV5)]);
    assert_eq!(take(&log), vec![Sent::Paste(17)]);
    backend.on_format_data_response(FormatDataResponse::new_error());
    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_LOCALE)]);
    assert_eq!(take(&log), vec![], "nothing it could use");
}

#[test]
fn an_answer_to_an_older_copy_is_dropped() {
    let (mut backend, rx, log) = backend();
    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
    // The second request waits for the first answer.
    assert_eq!(take(&log), vec![Sent::Paste(TEXT)]);
    backend.on_format_data_response(FormatDataResponse::new_data(formats::encode_utf16_text(
        "old",
    )));
    assert_eq!(take(&log), vec![Sent::Paste(TEXT)]);
    backend.on_format_data_response(FormatDataResponse::new_data(formats::encode_utf16_text(
        "new",
    )));
    let stored: Vec<String> = rx
        .try_iter()
        .filter_map(|m| match m {
            WorkerMsg::Store(c) => c.text,
            _ => None,
        })
        .collect();
    assert_eq!(stored, vec!["new".to_owned()]);
}

#[test]
fn a_request_the_server_never_answers_does_not_block_the_next() {
    let (mut backend, _rx, log) = backend();
    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_DIB)]);
    assert_eq!(take(&log), vec![Sent::Paste(TEXT)]);
    backend.tick();
    assert_eq!(take(&log), vec![], "not yet");
    if let Some((_, _, at)) = backend.in_flight.as_mut() {
        *at = Instant::now()
            .checked_sub(REQUEST_TIMEOUT)
            .expect("an instant in the past");
    }
    backend.tick();
    assert_eq!(take(&log), vec![Sent::Paste(DIB)]);
}

// ── Both ends in-process ────────────────────────────────────────────────────

/// What the test server's backend saw.
#[derive(Debug, Default)]
struct ServerLog {
    copies: Vec<Vec<ClipboardFormat>>,
    data_requests: Vec<ClipboardFormatId>,
    data: Vec<Option<Vec<u8>>>,
    file_lists: Vec<Vec<FileDescriptor>>,
    file_requests: Vec<FileContentsRequest>,
    file_data: Vec<(u32, Option<Vec<u8>>)>,
    locks: Vec<u32>,
}

#[derive(Debug, Clone, Default)]
struct ServerBackend(Arc<Mutex<ServerLog>>);

impl ironrdp_core::AsAny for ServerBackend {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl CliprdrBackend for ServerBackend {
    fn temporary_directory(&self) -> &str {
        ""
    }
    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
            | ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
            | ClipboardGeneralCapabilityFlags::FILECLIP_NO_FILE_PATHS
            | ClipboardGeneralCapabilityFlags::CAN_LOCK_CLIPDATA
    }
    fn on_ready(&mut self) {}
    fn on_request_format_list(&mut self) {}
    fn on_process_negotiated_capabilities(&mut self, _: ClipboardGeneralCapabilityFlags) {}
    fn on_remote_copy(&mut self, formats: &[ClipboardFormat]) {
        self.0.lock().copies.push(formats.to_vec());
    }
    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        self.0.lock().data_requests.push(request.format);
    }
    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        self.0
            .lock()
            .data
            .push((!response.is_error()).then(|| response.data().to_vec()));
    }
    fn on_remote_file_list(&mut self, files: &[FileDescriptor], _: Option<u32>) {
        self.0.lock().file_lists.push(files.to_vec());
    }
    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        self.0.lock().file_requests.push(request);
    }
    fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
        self.0.lock().file_data.push((
            response.stream_id(),
            (!response.is_error()).then(|| response.data().to_vec()),
        ));
    }
    fn on_lock(&mut self, id: LockDataId) {
        self.0.lock().locks.push(id.0);
    }
    fn on_unlock(&mut self, _: LockDataId) {}
}

/// Our client (backend, worker thread, fake clipboard) and IronRDP's server,
/// joined by encoding each side's PDUs and feeding them to the other.
struct Wire {
    client: CliprdrClient,
    server: CliprdrServer,
    server_log: Arc<Mutex<ServerLog>>,
    from_client: mpsc::Receiver<Outgoing>,
    statuses: Vec<Status>,
    worker: ClipboardHandle,
    /// What the server answers format data requests with.
    remote_data: HashMap<u32, Vec<u8>>,
    /// What the server's files contain, by index.
    remote_files: Vec<Vec<u8>>,
    fake: Fake,
    _dir: tempfile::TempDir,
}

impl Wire {
    fn new(fake: Fake) -> Self {
        let (tx, from_client) = mpsc::channel();
        let tx = Mutex::new(tx);
        let outbox = move || -> Outbox {
            let tx = Mutex::new(tx.lock().clone());
            Box::new(move |out| {
                let _ = tx.lock().send(out);
            })
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let clipboard = fake.clone();
        // The poll never fires on its own: everything here is an event.
        let worker = spawn_worker_with(
            move || Some(Box::new(clipboard) as Box<dyn LocalClipboard>),
            outbox(),
            Duration::from_secs(3600),
            dir.path().join("session"),
        );
        let client = CliprdrClient::new(Box::new(ClipboardBackend::new(worker.clone(), outbox())));
        let server_backend = ServerBackend::default();
        let server_log = server_backend.0.clone();
        let server = CliprdrServer::new(Box::new(server_backend));
        let mut wire = Self {
            client,
            server,
            server_log,
            from_client,
            statuses: Vec::new(),
            worker,
            remote_data: HashMap::new(),
            remote_files: Vec::new(),
            fake,
            _dir: dir,
        };
        let start = wire.server.start().expect("server start");
        wire.send_to_client(start);
        wire
    }

    fn send_to_client(&mut self, messages: Vec<SvcMessage>) {
        for message in messages {
            let bytes = message.encode_unframed_pdu().expect("encode");
            let replies = self.client.process(&bytes).expect("client processes");
            self.send_to_server(replies);
        }
    }

    fn send_to_server(&mut self, messages: Vec<SvcMessage>) {
        for message in messages {
            let bytes = message.encode_unframed_pdu().expect("encode");
            let replies = self.server.process(&bytes).expect("server processes");
            self.send_to_client(replies);
        }
        self.answer_as_server();
    }

    /// The server's side answers what the client asked of it.
    fn answer_as_server(&mut self) {
        let (requests, file_requests) = {
            let mut log = self.server_log.lock();
            (
                std::mem::take(&mut log.data_requests),
                std::mem::take(&mut log.file_requests),
            )
        };
        for format in requests {
            let response = match self.remote_data.get(&format.value()) {
                Some(data) => OwnedFormatDataResponse::new_data(data.clone()),
                None => OwnedFormatDataResponse::new_error(),
            };
            let messages = self.server.submit_format_data(response).expect("submit");
            self.send_to_client(messages.into());
        }
        for request in file_requests {
            let file = &self.remote_files[request.index as usize];
            let response = if request.flags.contains(FileContentsFlags::SIZE) {
                FileContentsResponse::new_size_response(request.stream_id, file.len() as u64)
            } else {
                let start = request.position as usize;
                let end = (start + request.requested_size as usize).min(file.len());
                FileContentsResponse::new_data_response(
                    request.stream_id,
                    file[start..end].to_vec(),
                )
            };
            let messages = self.server.submit_file_contents(response).expect("submit");
            self.send_to_client(messages.into());
        }
    }

    /// Carries what the worker and backend send until `done` holds. Wakes on
    /// every message; the short timeout only lets it see what the worker
    /// does without a word (putting things on the clipboard).
    fn run_until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(self) {
            assert!(Instant::now() < deadline, "stuck waiting for {what}");
            let Ok(out) = self.from_client.recv_timeout(Duration::from_millis(20)) else {
                continue;
            };
            match out {
                Outgoing::Status(status) => self.statuses.push(status),
                Outgoing::Cliprdr(message) => {
                    if let Some(messages) = to_channel(&mut self.client, message) {
                        self.send_to_server(messages.into());
                    }
                }
            }
        }
    }

    fn server_copies(&self) -> usize {
        self.server_log.lock().copies.len()
    }
}

#[test]
fn end_to_end_text_both_ways_without_echo() {
    let fake = Fake::default();
    fake.copy_text("from here");
    let mut wire = Wire::new(fake);
    // Initial empty list, then the text once the channel is up.
    wire.run_until("the text announced", |w| w.server_copies() == 2);
    let copies = wire.server_log.lock().copies.clone();
    assert!(copies[0].is_empty());
    assert_eq!(copies[1][0].id(), ClipboardFormatId::CF_UNICODETEXT);

    // The server pastes.
    let messages = wire
        .server
        .initiate_paste(ClipboardFormatId::CF_UNICODETEXT)
        .expect("paste");
    wire.send_to_client(messages.into());
    wire.run_until("the text served", |w| !w.server_log.lock().data.is_empty());
    let data = wire.server_log.lock().data[0].clone().expect("data");
    assert_eq!(formats::decode_utf16_text(&data), "from here");

    // The server copies text with HTML.
    wire.remote_data
        .insert(13, formats::encode_utf16_text("über\r\nall"));
    wire.remote_data
        .insert(0xC123, formats::wrap_cf_html("<b>über</b>"));
    let messages = wire
        .server
        .initiate_copy(&[
            ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT),
            named(0xC123, ClipboardFormatName::HTML),
        ])
        .expect("copy");
    wire.send_to_client(messages.into());
    wire.run_until("the text stored", |w| {
        w.fake.0.lock().html.as_deref() == Some("<b>über</b>")
    });
    let expected = if cfg!(windows) {
        "über\r\nall"
    } else {
        "über\nall"
    };
    assert_eq!(wire.fake.0.lock().text.as_deref(), Some(expected));
    // A focus check after the store must not send it back: the next thing
    // the worker says is the answer to a request made after the check.
    wire.worker.send(WorkerMsg::Check { deep: true });
    wire.worker.send(WorkerMsg::Serve(ClipboardFormatId(1)));
    let out = wire
        .from_client
        .recv_timeout(Duration::from_secs(5))
        .expect("answer");
    assert!(
        matches!(out, Outgoing::Cliprdr(ClipboardMessage::SendFormatData(ref r)) if r.is_error()),
        "no echo, got {out:?}"
    );
}

#[test]
fn end_to_end_files_both_ways() {
    let dir = tempfile::tempdir().expect("tempdir");
    let folder = dir.path().join("Projekt");
    std::fs::create_dir_all(&folder).expect("mkdir");
    let big: Vec<u8> = (0..(files::CHUNK as usize + 1000))
        .map(|i| (i % 253) as u8)
        .collect();
    std::fs::write(folder.join("data.bin"), &big).expect("write");
    let fake = Fake::default();
    fake.copy(|s| s.files = Some(vec![folder.clone()]));
    let mut wire = Wire::new(fake);

    // Local → remote: the folder is announced as a file list.
    wire.run_until("the files announced", |w| w.server_copies() == 2);
    let file_list = wire.server_log.lock().copies[1]
        .iter()
        .find(|f| {
            f.name()
                .is_some_and(|n| n.value() == "FileGroupDescriptorW")
        })
        .map(ClipboardFormat::id)
        .expect("a file list format");
    let messages = wire.server.initiate_paste(file_list).expect("paste");
    wire.send_to_client(messages.into());
    let files = wire.server_log.lock().file_lists[0].clone();
    let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["Projekt", "data.bin"]);
    assert_eq!(files[1].relative_path.as_deref(), Some("Projekt"));
    // The server reads the file in two ranges.
    for (stream_id, position, size) in [(10, 0, files::CHUNK), (11, u64::from(files::CHUNK), 1000)]
    {
        let messages = wire
            .server
            .request_file_contents(FileContentsRequest {
                stream_id,
                index: 1,
                flags: FileContentsFlags::RANGE,
                position,
                requested_size: size,
                data_id: None,
            })
            .expect("request");
        wire.send_to_client(messages.into());
    }
    wire.run_until("both ranges", |w| w.server_log.lock().file_data.len() == 2);
    let mut got = Vec::new();
    for (_, data) in wire.server_log.lock().file_data.iter() {
        got.extend_from_slice(data.as_deref().expect("data"));
    }
    assert_eq!(got, big);

    // Remote → local: the server copies files; they land on the clipboard.
    wire.remote_files = vec![Vec::new(), b"remote text".to_vec()];
    let messages = wire
        .server
        .initiate_file_copy(vec![
            FileDescriptor::new("Ordner")
                .with_attributes(ClipboardFileAttributes::DIRECTORY)
                .with_file_size(0),
            FileDescriptor::new("a.txt")
                .with_attributes(ClipboardFileAttributes::ARCHIVE)
                .with_file_size(11)
                .with_relative_path("Ordner"),
        ])
        .expect("file copy");
    wire.send_to_client(messages.into());
    wire.run_until("the download", |w| {
        w.statuses.iter().any(|s| matches!(s, Status::Ready { .. }))
    });
    assert!(
        !wire.server_log.lock().locks.is_empty(),
        "the client locked the data"
    );
    let local = wire.fake.0.lock().files.clone().expect("files");
    assert_eq!(local.len(), 1);
    assert!(local[0].ends_with("Ordner"));
    assert_eq!(
        std::fs::read(local[0].join("a.txt")).expect("downloaded"),
        b"remote text"
    );
}
