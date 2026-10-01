//! The clipboard in both directions (CLIPRDR): text, HTML, pictures and
//! files, backed by `arboard`.
//!
//! IronRDP's [`CliprdrBackend`] callbacks run inside the session task, in the
//! middle of processing a PDU, so they must not touch the OS clipboard or the
//! disk themselves: that can block (X11 round trips, another app holding the
//! Windows clipboard open, a slow disk). Instead a small worker thread owns
//! the local clipboard, and the two sides talk through channels:
//!
//! - **Remote copy** → the backend fetches what it can use right away, one
//!   request at a time (text and its HTML, else a picture, else the file
//!   list) → the worker puts it on the local clipboard. Files are downloaded
//!   into the session's own folder first (see [`files`]); big sets wait until
//!   the user asks for them.
//! - **Local copy** → the worker notices (its poll: the platform's change
//!   counter four times a second on Windows and macOS, the contents twice a
//!   second on Linux; or the page getting the focus back) and announces a
//!   format list → when the server asks for data, the worker reads it then
//!   (delayed rendering) and always answers, with an error if it has to.
//!
//! What the worker put on the local clipboard itself is remembered (by a
//! fingerprint of what reading it back gives), so it is never announced back
//! to the server as if the user had copied it.
//!
//! On Linux (X11) the thread also keeps the `arboard::Clipboard` alive for
//! the whole session, which is what keeps what we copied available to other
//! applications.
//!
//! Every failure here is logged and swallowed: a broken clipboard must never
//! take the session down with it.

mod files;
mod formats;
mod local;

use files::{Download, LocalList, Progress, RemoteEntry, Served};
use formats::Rgba;
use ironrdp_cliprdr::backend::{ClipboardMessage, CliprdrBackend};
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardFormatName, ClipboardGeneralCapabilityFlags,
    FileContentsRequest, FileContentsResponse, FileDescriptor, FormatDataRequest,
    FormatDataResponse, LockDataId, OwnedFormatDataResponse,
};
use ironrdp_cliprdr::{CliprdrClient, CliprdrSvcMessages};
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
use tracing::{debug, warn};

pub(crate) use local::LocalClipboard;

/// How often the worker looks for local changes where the platform has a
/// cheap change counter (Windows, macOS).
pub(crate) const POLL_WITH_COUNTER: Duration = Duration::from_millis(250);

/// How often it compares contents where there is none (Linux).
pub(crate) const POLL_CONTENTS: Duration = Duration::from_millis(500);

/// Files from the server up to this size (all together) are downloaded as
/// soon as they are copied; bigger sets wait for the user to ask.
pub(crate) const AUTO_DOWNLOAD_LIMIT: u64 = 256 * 1024 * 1024;

/// How long a format data request may go unanswered before the next one is
/// sent anyway.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// How often download progress reaches the page.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// The id we announce our HTML under; the server goes by its name.
const HTML_FORMAT_ID: ClipboardFormatId = ClipboardFormatId(0xC0F0);

/// What a session's clipboard tells the page (as a `CLIPBOARD` message).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub(crate) enum Status {
    /// Files from the server are being downloaded.
    Downloading {
        files: usize,
        done: u64,
        total: u64,
    },
    /// Files from the server are on the local clipboard now.
    Ready {
        files: usize,
    },
    /// The server offers more than [`AUTO_DOWNLOAD_LIMIT`] (or of unknown
    /// size); the page may ask for them with `clipboard_download`.
    Offered {
        files: usize,
        total: Option<u64>,
    },
    /// Files dropped on the desktop are on the server's clipboard.
    Sent {
        files: usize,
    },
    Failed {
        error: Failure,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Failure {
    /// The server doesn't take files through the clipboard.
    Unsupported,
    /// Downloading the server's files failed.
    Download,
    /// The dropped files could not be offered.
    Offer,
}

/// What the worker and the backend send the session loop.
#[derive(Debug)]
pub(crate) enum Outgoing {
    Cliprdr(ClipboardMessage),
    Status(Status),
}

/// Where the worker and the backend send messages for the session loop.
pub(crate) type Outbox = Box<dyn Fn(Outgoing) + Send>;

/// What the server copied, fetched and ready for the local clipboard.
#[derive(Debug, Default)]
pub(crate) struct RemoteContent {
    text: Option<String>,
    html: Option<String>,
    image: Option<Rgba>,
}

impl RemoteContent {
    fn is_empty(&self) -> bool {
        self.text.is_none() && self.image.is_none()
    }
}

#[derive(Debug)]
pub(crate) enum WorkerMsg {
    /// The channel asks for our initial format list; it must be answered
    /// (even with an empty list) or the channel never finishes initializing.
    Initial,
    /// The server accepted it: the channel is up, announce what is there.
    Ready,
    Capabilities(ClipboardGeneralCapabilityFlags),
    /// Look at the local clipboard now; `deep` includes a picture even on
    /// Linux, where the poll leaves pictures out. Sent when the page gets
    /// the focus back.
    Check {
        deep: bool,
    },
    /// The server wants our data in this format.
    Serve(ClipboardFormatId),
    /// The server took the clipboard: drop any download of its old files.
    RemoteCopy,
    /// What the server copied, to be put on the local clipboard.
    Store(RemoteContent),
    /// The server copied files.
    RemoteFiles {
        files: Vec<FileDescriptor>,
        clip_data_id: Option<u32>,
    },
    /// The user asked for files that were too big to fetch on their own.
    Download,
    /// Offer these files to the server (dropped on the desktop).
    OfferFiles(Vec<PathBuf>),
    FileRequest(FileContentsRequest),
    FileResponse {
        stream_id: u32,
        data: Option<Vec<u8>>,
    },
    Lock(u32),
    Unlock(u32),
}

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
    let poll = if cfg!(any(windows, target_os = "macos")) {
        POLL_WITH_COUNTER
    } else {
        POLL_CONTENTS
    };
    let folder = std::env::temp_dir().join(format!("{TEMP_PREFIX}{}", uuid::Uuid::new_v4()));
    spawn_worker_with(local::open_system_clipboard, outbox, poll, folder)
}

pub(crate) fn spawn_worker_with(
    open: impl FnOnce() -> Option<Box<dyn LocalClipboard>> + Send + 'static,
    outbox: Outbox,
    poll: Duration,
    folder: PathBuf,
) -> ClipboardHandle {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("uwurdp-clipboard".into())
        .spawn(move || {
            if let Some(parent) = folder.parent() {
                remove_stale_folders(parent);
            }
            let mut worker = Worker::new(open(), outbox, folder);
            run_worker(&mut worker, &rx, poll);
        });
    if let Err(e) = spawned {
        warn!(error = %e, "cannot start the clipboard thread");
    }
    ClipboardHandle { tx }
}

/// The session folders for downloaded files are named like this.
const TEMP_PREFIX: &str = "uwurdp-clipboard-";

/// Folders a crashed app left behind, older than a day.
fn remove_stale_folders(temp: &Path) {
    let Ok(entries) = std::fs::read_dir(temp) else {
        return;
    };
    let day = Duration::from_secs(24 * 3600);
    for entry in entries.filter_map(Result::ok) {
        let stale = entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX)
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > day);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn run_worker(worker: &mut Worker, rx: &mpsc::Receiver<WorkerMsg>, poll: Duration) {
    let mut next_poll = Instant::now() + poll;
    loop {
        match rx.recv_timeout(next_poll.saturating_duration_since(Instant::now())) {
            Ok(msg) => worker.handle(msg),
            Err(RecvTimeoutError::Timeout) => {
                worker.check(false);
                next_poll = Instant::now() + poll;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    worker.shutdown();
    debug!("clipboard worker stopped");
}

fn hash_of(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// What the poll reads every time: cheap on every platform.
#[derive(Debug, Hash)]
enum Shallow {
    Empty,
    Text(String),
    Files(Vec<PathBuf>),
}

fn read_shallow(clipboard: &mut dyn LocalClipboard) -> Shallow {
    if let Some(files) = clipboard.files() {
        Shallow::Files(files)
    } else if let Some(text) = clipboard.text() {
        Shallow::Text(text)
    } else {
        Shallow::Empty
    }
}

struct Worker {
    clipboard: Option<Box<dyn LocalClipboard>>,
    outbox: Outbox,
    /// Between the channel's first request and the server's acceptance only
    /// the initial list may go out.
    ready: bool,
    files_ok: bool,
    /// The change counter, contents and picture as last seen (or as left by
    /// our own writes). `None`: not looked yet.
    last_token: Option<u64>,
    last_shallow: Option<u64>,
    last_image: Option<u64>,
    /// Counts local changes, so a download can tell the user copied
    /// something else meanwhile.
    changes: u64,
    served: Served,
    /// This session's folder for downloaded files.
    folder: PathBuf,
    downloads: u32,
    download: Option<Download>,
    /// Top-level entries of the running download, and `changes` at its start.
    download_files: usize,
    download_changes: u64,
    /// The folder of the last finished download, whose files are on the
    /// clipboard; removed when the next one is.
    finished: Option<PathBuf>,
    offered: Option<(Vec<RemoteEntry>, Option<u32>)>,
    next_stream: u32,
    last_progress: Option<Instant>,
}

impl Worker {
    fn new(clipboard: Option<Box<dyn LocalClipboard>>, outbox: Outbox, folder: PathBuf) -> Self {
        Self {
            clipboard,
            outbox,
            ready: false,
            files_ok: false,
            last_token: None,
            last_shallow: None,
            last_image: None,
            changes: 0,
            served: Served::default(),
            folder,
            downloads: 0,
            download: None,
            download_files: 0,
            download_changes: 0,
            finished: None,
            offered: None,
            next_stream: 0,
            last_progress: None,
        }
    }

    fn send(&self, message: ClipboardMessage) {
        (self.outbox)(Outgoing::Cliprdr(message));
    }

    fn status(&self, status: Status) {
        (self.outbox)(Outgoing::Status(status));
    }

    fn handle(&mut self, msg: WorkerMsg) {
        match msg {
            // The first time the channel is still starting: only an empty
            // list may go out, the real one follows once it is ready. A
            // server that starts the channel over (a new Monitor Ready) gets
            // what is there right away.
            WorkerMsg::Initial if self.ready => {
                if !self.announce_current() {
                    self.send(ClipboardMessage::SendInitiateCopy(Vec::new()));
                }
            }
            WorkerMsg::Initial => self.send(ClipboardMessage::SendInitiateCopy(Vec::new())),
            WorkerMsg::Ready => {
                self.ready = true;
                self.announce_current();
            }
            WorkerMsg::Capabilities(flags) => {
                self.files_ok =
                    flags.contains(ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED);
            }
            WorkerMsg::Check { deep } => {
                self.check(deep);
            }
            WorkerMsg::Serve(format) => {
                let response = self
                    .render(format)
                    .map(OwnedFormatDataResponse::new_data)
                    .unwrap_or_else(OwnedFormatDataResponse::new_error);
                self.send(ClipboardMessage::SendFormatData(response));
            }
            WorkerMsg::RemoteCopy => {
                self.download = None;
                self.offered = None;
                self.served.withdraw();
            }
            WorkerMsg::Store(content) => self.store(content),
            WorkerMsg::RemoteFiles {
                files,
                clip_data_id,
            } => self.remote_files(&files, clip_data_id),
            WorkerMsg::Download => {
                if let Some((entries, clip_data_id)) = self.offered.take() {
                    self.start_download(entries, clip_data_id);
                }
            }
            WorkerMsg::OfferFiles(paths) => self.offer_files(&paths, true),
            WorkerMsg::FileRequest(request) => {
                let response = self.served.serve(&request);
                self.send(ClipboardMessage::SendFileContentsResponse(response));
            }
            WorkerMsg::FileResponse { stream_id, data } => self.file_response(stream_id, data),
            WorkerMsg::Lock(id) => self.served.lock(id),
            WorkerMsg::Unlock(id) => self.served.unlock(id),
        }
    }

    /// Announces whatever is on the local clipboard, changed or not.
    fn announce_current(&mut self) -> bool {
        self.last_token = None;
        self.last_shallow = None;
        self.last_image = None;
        self.check(true)
    }

    /// Looks at the local clipboard and announces it if it changed. Returns
    /// whether it did.
    fn check(&mut self, deep: bool) -> bool {
        if !self.ready {
            return false;
        }
        let Some((shallow, image, changed)) = self.look(deep) else {
            return false;
        };
        if changed {
            self.changes += 1;
            return self.announce(shallow, image);
        }
        false
    }

    /// Reads what changed since the last look and remembers it. `None` when
    /// nothing can have changed (or there is no clipboard).
    fn look(&mut self, deep: bool) -> Option<(Shallow, Option<Rgba>, bool)> {
        let clipboard = self.clipboard.as_deref_mut()?;
        let token = clipboard.change_token();
        if token.is_some() && token == self.last_token && self.last_shallow.is_some() {
            return None;
        }
        // With a change counter every change is worth a full look.
        let deep = deep || token.is_some();
        self.last_token = token;
        let shallow = read_shallow(clipboard);
        let shallow_hash = hash_of(&shallow);
        let mut changed = self.last_shallow != Some(shallow_hash);
        self.last_shallow = Some(shallow_hash);
        let mut image = None;
        if matches!(shallow, Shallow::Empty) {
            if deep || changed {
                image = clipboard.image();
                let image_hash = image
                    .as_ref()
                    .map(|i| hash_of(&(i.width, i.height, &i.bytes)));
                changed |= image_hash != self.last_image;
                self.last_image = image_hash;
            }
        } else {
            self.last_image = None;
        }
        Some((shallow, image, changed))
    }

    fn announce(&mut self, shallow: Shallow, image: Option<Rgba>) -> bool {
        let formats = match shallow {
            Shallow::Files(paths) => {
                self.offer_files(&paths, false);
                return true;
            }
            Shallow::Text(_) => {
                let mut formats = vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)];
                let html = self.clipboard.as_deref_mut().and_then(|c| c.html());
                if html.is_some() {
                    formats.push(
                        ClipboardFormat::new(HTML_FORMAT_ID).with_name(ClipboardFormatName::HTML),
                    );
                }
                formats
            }
            // A clipboard that went empty is no news for the server.
            Shallow::Empty if image.is_none() => return false,
            Shallow::Empty => vec![ClipboardFormat::new(ClipboardFormatId::CF_DIB)],
        };
        self.served.withdraw();
        self.send(ClipboardMessage::SendInitiateCopy(formats));
        true
    }

    /// The data for a format we announced, read now (delayed rendering).
    fn render(&mut self, format: ClipboardFormatId) -> Option<Vec<u8>> {
        let clipboard = self.clipboard.as_deref_mut()?;
        match format {
            ClipboardFormatId::CF_UNICODETEXT => {
                clipboard.text().map(|t| formats::encode_utf16_text(&t))
            }
            HTML_FORMAT_ID => clipboard.html().map(|h| formats::wrap_cf_html(&h)),
            ClipboardFormatId::CF_DIB => clipboard.image().and_then(|i| formats::rgba_to_dib(&i)),
            _ => None,
        }
    }

    fn offer_files(&mut self, paths: &[PathBuf], dropped: bool) {
        if !self.files_ok {
            if dropped {
                self.status(Status::Failed {
                    error: Failure::Unsupported,
                    message: "The server doesn't take files through the clipboard.".into(),
                });
            } else {
                // The server can't have them; it shouldn't keep the old content either.
                self.served.withdraw();
                self.send(ClipboardMessage::SendInitiateCopy(Vec::new()));
            }
            return;
        }
        let list = LocalList::collect(paths);
        if list.descriptors.is_empty() {
            if dropped {
                self.status(Status::Failed {
                    error: Failure::Offer,
                    message: "None of the files can be read.".into(),
                });
            }
            return;
        }
        self.served.offer(list.entries);
        self.send(ClipboardMessage::SendInitiateFileCopy(list.descriptors));
        if dropped {
            self.status(Status::Sent { files: paths.len() });
        }
    }

    fn store(&mut self, content: RemoteContent) {
        let Some(clipboard) = self.clipboard.as_deref_mut() else {
            return;
        };
        let result = match content {
            RemoteContent {
                text: Some(text),
                html,
                ..
            } => clipboard.set_text(&formats::from_remote_line_endings(&text), html.as_deref()),
            RemoteContent {
                image: Some(image), ..
            } => clipboard.set_image(image),
            _ => return,
        };
        match result {
            Ok(()) => self.absorb(),
            Err(e) => warn!(error = %e, "cannot set the local clipboard"),
        }
    }

    /// Remembers what is on the clipboard now as already known, so our own
    /// write is never announced back.
    fn absorb(&mut self) {
        let _ = self.look(true);
    }

    fn remote_files(&mut self, files: &[FileDescriptor], clip_data_id: Option<u32>) {
        let entries = files::plan(files);
        if entries.is_empty() {
            return;
        }
        // Sizes come from the server: a sum that overflows counts as unknown,
        // so the user is asked instead of a wrapped-around small total.
        let sizes: Option<u64> = entries
            .iter()
            .try_fold(0u64, |sum, e| sum.checked_add(e.size?));
        match sizes {
            Some(total) if total <= AUTO_DOWNLOAD_LIMIT => {
                self.start_download(entries, clip_data_id);
            }
            total => {
                let files = entries.iter().filter(|e| e.top).count();
                self.offered = Some((entries, clip_data_id));
                self.status(Status::Offered { files, total });
            }
        }
    }

    fn start_download(&mut self, entries: Vec<RemoteEntry>, clip_data_id: Option<u32>) {
        self.downloads += 1;
        self.download_files = entries.iter().filter(|e| e.top).count();
        self.download_changes = self.changes;
        self.last_progress = None;
        let base = self.folder.join(self.downloads.to_string());
        let mut download = Download::new(base, entries, clip_data_id);
        let next = &mut self.next_stream;
        let mut next_id = || {
            *next = next.wrapping_add(1).max(1);
            *next
        };
        match download.start(&mut next_id) {
            Ok(requests) => {
                self.download = Some(download);
                for request in requests {
                    self.send(ClipboardMessage::SendFileContentsRequest(request));
                }
                self.progress(0, 0);
            }
            Err(end) => {
                self.download = Some(download);
                self.download_ended(end);
            }
        }
    }

    fn file_response(&mut self, stream_id: u32, data: Option<Vec<u8>>) {
        let Some(download) = self.download.as_mut().filter(|d| d.owns(stream_id)) else {
            debug!(stream_id, "file contents for no download in progress");
            return;
        };
        let next = &mut self.next_stream;
        let mut next_id = || {
            *next = next.wrapping_add(1).max(1);
            *next
        };
        let (requests, progress) = download.on_response(stream_id, data.as_deref(), &mut next_id);
        for request in requests {
            self.send(ClipboardMessage::SendFileContentsRequest(request));
        }
        self.download_ended(progress);
    }

    fn progress(&mut self, done: u64, total: u64) {
        let due = self
            .last_progress
            .is_none_or(|at| at.elapsed() >= PROGRESS_INTERVAL);
        if due {
            self.last_progress = Some(Instant::now());
            self.status(Status::Downloading {
                files: self.download_files,
                done,
                total,
            });
        }
    }

    fn download_ended(&mut self, progress: Progress) {
        match progress {
            Progress::Running { done, total } => self.progress(done, total),
            Progress::Failed(message) => {
                warn!(%message, "downloading files from the server failed");
                self.download = None;
                self.status(Status::Failed {
                    error: Failure::Download,
                    message,
                });
            }
            Progress::Finished(top) => {
                let Some(download) = self.download.take() else {
                    return;
                };
                let base = download.base().to_owned();
                drop(download);
                // The user copied something else meanwhile: that wins.
                if self.check(false) || self.changes != self.download_changes {
                    debug!("the local clipboard changed during the download; dropping it");
                    let _ = std::fs::remove_dir_all(&base);
                    return;
                }
                let set = match self.clipboard.as_deref_mut() {
                    Some(clipboard) => clipboard.set_files(&top),
                    None => Err("no local clipboard".into()),
                };
                match set {
                    Ok(()) => {
                        self.absorb();
                        if let Some(old) = self.finished.replace(base) {
                            let _ = std::fs::remove_dir_all(old);
                        }
                        self.status(Status::Ready { files: top.len() });
                    }
                    Err(message) => {
                        warn!(%message, "cannot put the downloaded files on the clipboard");
                        let _ = std::fs::remove_dir_all(&base);
                        self.status(Status::Failed {
                            error: Failure::Download,
                            message,
                        });
                    }
                }
            }
        }
    }

    /// The downloaded files stay: the local clipboard may still point at
    /// them, and pasting after disconnecting should work. A download cut off
    /// halfway goes; the rest is cleaned up after a day (`remove_stale_folders`).
    fn shutdown(&mut self) {
        if let Some(download) = self.download.take() {
            let _ = std::fs::remove_dir_all(download.base());
        }
    }
}

/// Turns a message from the worker or the backend into PDUs for the channel.
/// A request the channel refuses is answered with an error right here, so
/// nobody waits for a response that will never come.
pub(crate) fn to_channel(
    cliprdr: &mut CliprdrClient,
    message: ClipboardMessage,
) -> Option<CliprdrSvcMessages<ironrdp_cliprdr::Client>> {
    let (result, refused) = match message {
        ClipboardMessage::SendInitiateCopy(formats) => (cliprdr.initiate_copy(&formats), None),
        ClipboardMessage::SendFormatData(response) => (cliprdr.submit_format_data(response), None),
        ClipboardMessage::SendInitiatePaste(format) => {
            (cliprdr.initiate_paste(format), Some(Refused::Paste))
        }
        ClipboardMessage::SendFileContentsRequest(request) => {
            let stream_id = request.stream_id;
            (
                cliprdr.request_file_contents(request),
                Some(Refused::FileContents(stream_id)),
            )
        }
        ClipboardMessage::SendFileContentsResponse(response) => {
            (cliprdr.submit_file_contents(response), None)
        }
        ClipboardMessage::SendInitiateFileCopy(files) => (cliprdr.initiate_file_copy(files), None),
        ClipboardMessage::Error(e) => {
            warn!(error = %e, "clipboard error");
            return None;
        }
    };
    match result {
        Ok(messages) => Some(messages),
        Err(e) => {
            warn!(error = %e, "clipboard message could not be encoded");
            if let Some(backend) = cliprdr.downcast_backend_mut::<ClipboardBackend>() {
                match refused {
                    Some(Refused::Paste) => backend.refused(),
                    Some(Refused::FileContents(stream_id)) => {
                        backend
                            .on_file_contents_response(FileContentsResponse::new_error(stream_id));
                    }
                    None => {}
                }
            }
            None
        }
    }
}

enum Refused {
    Paste,
    FileContents(u32),
}

/// The channel's timers: expired locks and stale transfers (IronRDP wants
/// this every few seconds), and requests the server never answered.
pub(crate) fn tick(
    cliprdr: &mut CliprdrClient,
) -> Option<CliprdrSvcMessages<ironrdp_cliprdr::Client>> {
    if let Some(backend) = cliprdr.downcast_backend_mut::<ClipboardBackend>() {
        backend.tick();
    }
    match cliprdr.drive_timeouts() {
        Ok(messages) => Some(messages),
        Err(e) => {
            warn!(error = %e, "clipboard timers failed");
            None
        }
    }
}

/// What the backend asked the server for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wanted {
    Text,
    Html(ClipboardFormatId),
    Image(ClipboardFormatId),
    FileList(ClipboardFormatId),
}

impl Wanted {
    fn format(self) -> ClipboardFormatId {
        match self {
            Wanted::Text => ClipboardFormatId::CF_UNICODETEXT,
            Wanted::Html(id) | Wanted::Image(id) | Wanted::FileList(id) => id,
        }
    }
}

/// The CLIPRDR backend: forwards everything to the worker, and fetches what
/// the server copies.
///
/// The channel matches a format data response to the request before it by
/// order alone (and recognizes the file list that way), so the backend keeps
/// exactly one request in flight and queues the rest.
pub(crate) struct ClipboardBackend {
    worker: ClipboardHandle,
    /// Straight to the session loop, for what needs no clipboard access.
    session: Outbox,
    /// Counts the server's copies; answers to an older one are dropped.
    generation: u64,
    queue: VecDeque<Wanted>,
    in_flight: Option<(u64, Wanted, Instant)>,
    content: RemoteContent,
}

impl std::fmt::Debug for ClipboardBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardBackend")
            .field("generation", &self.generation)
            .field("queue", &self.queue)
            .finish_non_exhaustive()
    }
}

impl ClipboardBackend {
    pub(crate) fn new(worker: ClipboardHandle, session: Outbox) -> Self {
        Self {
            worker,
            session,
            generation: 0,
            queue: VecDeque::new(),
            in_flight: None,
            content: RemoteContent::default(),
        }
    }

    /// Called by the session every few seconds: a request the server never
    /// answered must not block the next ones forever.
    pub(crate) fn tick(&mut self) {
        if self
            .in_flight
            .is_some_and(|(_, _, at)| at.elapsed() >= REQUEST_TIMEOUT)
        {
            debug!("the server never answered a clipboard request");
            self.in_flight = None;
            self.pump();
        }
    }

    /// The channel refused the request in flight; go on with the next.
    fn refused(&mut self) {
        self.in_flight = None;
        self.pump();
    }

    fn pump(&mut self) {
        if self.in_flight.is_some() {
            return;
        }
        if let Some(wanted) = self.queue.pop_front() {
            self.in_flight = Some((self.generation, wanted, Instant::now()));
            (self.session)(Outgoing::Cliprdr(ClipboardMessage::SendInitiatePaste(
                wanted.format(),
            )));
        }
    }

    /// Takes the request an answer belongs to; `Some` if it is current.
    fn answered(&mut self) -> Option<Wanted> {
        let (generation, wanted, _) = self.in_flight.take()?;
        (generation == self.generation).then_some(wanted)
    }

    /// Everything of the current copy is fetched: hand it to the worker.
    fn deliver_if_complete(&mut self) {
        if self.queue.is_empty() && !self.content.is_empty() {
            let content = std::mem::take(&mut self.content);
            self.worker.send(WorkerMsg::Store(content));
        }
    }
}

impl ironrdp_core::AsAny for ClipboardBackend {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl CliprdrBackend for ClipboardBackend {
    fn temporary_directory(&self) -> &str {
        // Only a hint for servers that want paths, which we tell them not to.
        ".cliprdr"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
            | ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
            | ClipboardGeneralCapabilityFlags::FILECLIP_NO_FILE_PATHS
            | ClipboardGeneralCapabilityFlags::CAN_LOCK_CLIPDATA
            | ClipboardGeneralCapabilityFlags::HUGE_FILE_SUPPORT_ENABLED
    }

    fn on_ready(&mut self) {
        self.worker.send(WorkerMsg::Ready);
    }

    fn on_request_format_list(&mut self) {
        self.worker.send(WorkerMsg::Initial);
    }

    fn on_process_negotiated_capabilities(&mut self, flags: ClipboardGeneralCapabilityFlags) {
        self.worker.send(WorkerMsg::Capabilities(flags));
    }

    fn on_remote_copy(&mut self, formats: &[ClipboardFormat]) {
        // Fetch eagerly: the local clipboard has no way to render lazily
        // across the network. One kind only: files, else text (with its
        // HTML), else a picture.
        self.generation += 1;
        self.queue.clear();
        self.content = RemoteContent::default();
        self.worker.send(WorkerMsg::RemoteCopy);
        let named = |name: &ClipboardFormatName| {
            formats
                .iter()
                .find(|f| f.name().is_some_and(|n| n.value() == name.value()))
                .map(ClipboardFormat::id)
        };
        let has = |id: ClipboardFormatId| formats.iter().any(|f| f.id() == id);
        if let Some(id) = named(&ClipboardFormatName::FILE_LIST) {
            self.queue.push_back(Wanted::FileList(id));
        } else if has(ClipboardFormatId::CF_UNICODETEXT) {
            self.queue.push_back(Wanted::Text);
            if let Some(id) = named(&ClipboardFormatName::HTML) {
                self.queue.push_back(Wanted::Html(id));
            }
        } else if has(ClipboardFormatId::CF_DIB) {
            self.queue
                .push_back(Wanted::Image(ClipboardFormatId::CF_DIB));
        } else if has(ClipboardFormatId::CF_DIBV5) {
            self.queue
                .push_back(Wanted::Image(ClipboardFormatId::CF_DIBV5));
        }
        self.pump();
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        // The worker answers every request, with an error if it must.
        self.worker.send(WorkerMsg::Serve(request.format));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if let Some(wanted) = self.answered() {
            if response.is_error() {
                debug!(?wanted, "the server could not deliver clipboard data");
            } else {
                let data = response.data();
                match wanted {
                    Wanted::Text => {
                        let text = formats::decode_utf16_text(data);
                        self.content.text = (!text.is_empty()).then_some(text);
                    }
                    Wanted::Html(_) => self.content.html = formats::unwrap_cf_html(data),
                    Wanted::Image(_) => match formats::dib_to_rgba(data) {
                        Some(image) => self.content.image = Some(image),
                        None => warn!("a picture from the server could not be decoded"),
                    },
                    // Only a broken list ends up here; the channel logged it.
                    Wanted::FileList(_) => {}
                }
            }
            self.deliver_if_complete();
        }
        self.pump();
    }

    fn on_remote_file_list(&mut self, files: &[FileDescriptor], clip_data_id: Option<u32>) {
        if let Some(Wanted::FileList(_)) = self.answered() {
            self.worker.send(WorkerMsg::RemoteFiles {
                files: files.to_vec(),
                clip_data_id,
            });
        }
        self.pump();
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        self.worker.send(WorkerMsg::FileRequest(request));
    }

    fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
        self.worker.send(WorkerMsg::FileResponse {
            stream_id: response.stream_id(),
            data: (!response.is_error()).then(|| response.data().to_vec()),
        });
    }

    fn on_lock(&mut self, id: LockDataId) {
        self.worker.send(WorkerMsg::Lock(id.0));
    }

    fn on_unlock(&mut self, id: LockDataId) {
        self.worker.send(WorkerMsg::Unlock(id.0));
    }
}

#[cfg(test)]
mod tests;
