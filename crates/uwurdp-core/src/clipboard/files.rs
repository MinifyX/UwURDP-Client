//! Files through the clipboard (MS-RDPECLIP file streams), both ways.
//!
//! - **Local → remote**: the files and folders on the local clipboard (or
//!   dropped on the desktop) become a `FileGroupDescriptorW` list, folders
//!   walked recursively. The server then asks for each file's size and for
//!   ranges of it; [`Served`] answers from the files themselves, one range at
//!   a time, never more than [`MAX_RANGE`] in memory.
//! - **Remote → local**: the server's list becomes a [`Download`] into a
//!   folder of the session's own, chunk by chunk with a few requests in
//!   flight. Once everything is there, the top-level entries go on the local
//!   clipboard as a file list, so pasting in Explorer, Finder or a Linux file
//!   manager works like any other copy.
//!
//! IronRDP's channel already encodes and decodes the descriptors, cleans the
//! server's names of absolute paths and `..`, and checks every request's
//! index against the list (locked snapshots included).

use ironrdp_cliprdr::pdu::{
    ClipboardFileAttributes, FileContentsFlags, FileContentsRequest, FileContentsResponse,
    FileDescriptor,
};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

/// The most one file contents response carries, whatever the server asks.
pub(crate) const MAX_RANGE: u32 = 8 * 1024 * 1024;

/// What one download request asks for.
pub(crate) const CHUNK: u32 = 1024 * 1024;

/// Requests in flight per download: enough to hide the round trip.
const WINDOW: usize = 4;

/// The longest name the descriptor holds (260 UTF-16 units with the NUL).
const MAX_WIRE_NAME: usize = 259;

/// More entries than anyone copies on purpose; stops a walk of a whole disk.
pub(crate) const MAX_ENTRIES: usize = 10_000;

/// Seconds between 1601-01-01 (FILETIME) and 1970-01-01.
const FILETIME_UNIX_OFFSET: u64 = 11_644_473_600;

fn to_filetime(time: SystemTime) -> Option<u64> {
    let since = time.duration_since(UNIX_EPOCH).ok()?;
    (since.as_secs() + FILETIME_UNIX_OFFSET)
        .checked_mul(10_000_000)?
        .checked_add(u64::from(since.subsec_nanos() / 100))
}

fn from_filetime(filetime: u64) -> Option<SystemTime> {
    let secs = (filetime / 10_000_000).checked_sub(FILETIME_UNIX_OFFSET)?;
    let nanos = (filetime % 10_000_000) * 100;
    UNIX_EPOCH.checked_add(Duration::new(secs, u32::try_from(nanos).ok()?))
}

// ── Local → remote ───────────────────────────────────────────────────────────

/// One entry of a list we offer: where it is here, and whether it's a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalEntry {
    pub path: PathBuf,
    pub dir: bool,
}

/// What we offer the server: the descriptors in wire order and, at the same
/// index, where each entry lives here.
#[derive(Debug, Default)]
pub(crate) struct LocalList {
    pub descriptors: Vec<FileDescriptor>,
    pub entries: Vec<LocalEntry>,
}

impl LocalList {
    /// Builds the list for `paths` (files and folders), folders recursively.
    /// Symbolic links to folders are not followed (no loops); entries whose
    /// name would not fit the descriptor are left out — the channel would
    /// drop them silently and every index after them would be off.
    pub(crate) fn collect(paths: &[PathBuf]) -> Self {
        let mut list = Self::default();
        for path in paths {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            list.add(path, name.to_owned(), None);
        }
        list
    }

    fn add(&mut self, path: &Path, name: String, parent: Option<&str>) {
        if self.entries.len() >= MAX_ENTRIES {
            return;
        }
        let Ok(meta) = std::fs::metadata(path) else {
            debug!(path = %path.display(), "skipping a file that can't be read");
            return;
        };
        let wire = match parent {
            Some(parent) => format!("{parent}\\{name}"),
            None => name.clone(),
        };
        if name.is_empty() || wire.encode_utf16().count() > MAX_WIRE_NAME {
            warn!(name = %wire, "skipping a file whose name is too long for the clipboard");
            return;
        }
        let dir = meta.is_dir();
        let mut attributes = if dir {
            ClipboardFileAttributes::DIRECTORY
        } else {
            ClipboardFileAttributes::ARCHIVE
        };
        if meta.permissions().readonly() {
            attributes |= ClipboardFileAttributes::READONLY;
        }
        let mut descriptor = FileDescriptor::new(name.clone())
            .with_attributes(attributes)
            .with_file_size(if dir { 0 } else { meta.len() });
        if let Some(time) = meta.modified().ok().and_then(to_filetime) {
            descriptor = descriptor.with_last_write_time(time);
        }
        if let Some(parent) = parent {
            descriptor = descriptor.with_relative_path(parent);
        }
        self.descriptors.push(descriptor);
        self.entries.push(LocalEntry {
            path: path.to_owned(),
            dir,
        });
        let is_link = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
        if !dir || is_link {
            return;
        }
        let Ok(children) = std::fs::read_dir(path) else {
            return;
        };
        let mut children: Vec<_> = children.filter_map(Result::ok).collect();
        children.sort_by_key(|c| c.file_name());
        for child in children {
            if let Some(child_name) = child.file_name().to_str() {
                self.add(&child.path(), child_name.to_owned(), Some(&wire));
            }
        }
    }
}

/// Answers the server's file contents requests from the files we offered.
#[derive(Debug, Default)]
pub(crate) struct Served {
    current: Option<Arc<Vec<LocalEntry>>>,
    /// Snapshots the server locked (MS-RDPECLIP 3.1.5.3.2), by clipDataId.
    locked: HashMap<u32, Arc<Vec<LocalEntry>>>,
    /// The file the last range came from, kept open for the next one.
    open: Option<(PathBuf, File)>,
}

impl Served {
    pub(crate) fn offer(&mut self, entries: Vec<LocalEntry>) {
        self.current = Some(Arc::new(entries));
        self.open = None;
    }

    /// Something else is on offer now (text, a picture, nothing).
    pub(crate) fn withdraw(&mut self) {
        self.current = None;
        self.open = None;
    }

    pub(crate) fn lock(&mut self, id: u32) {
        if let Some(current) = &self.current {
            self.locked.insert(id, current.clone());
        }
    }

    pub(crate) fn unlock(&mut self, id: u32) {
        self.locked.remove(&id);
    }

    pub(crate) fn serve(&mut self, request: &FileContentsRequest) -> FileContentsResponse<'static> {
        let stream = request.stream_id;
        match self.try_serve(request) {
            Ok(response) => response,
            Err(e) => {
                warn!(error = %e, index = request.index, "cannot serve a file to the server");
                FileContentsResponse::new_error(stream)
            }
        }
    }

    fn try_serve(
        &mut self,
        request: &FileContentsRequest,
    ) -> Result<FileContentsResponse<'static>, String> {
        let list = request
            .data_id
            .and_then(|id| self.locked.get(&id))
            .or(self.current.as_ref())
            .ok_or("no files on offer")?;
        let entry = usize::try_from(request.index)
            .ok()
            .and_then(|i| list.get(i))
            .ok_or("no such file")?
            .clone();
        if request.flags.contains(FileContentsFlags::SIZE) {
            let size = if entry.dir {
                0
            } else {
                std::fs::metadata(&entry.path)
                    .map_err(|e| e.to_string())?
                    .len()
            };
            return Ok(FileContentsResponse::new_size_response(
                request.stream_id,
                size,
            ));
        }
        if entry.dir {
            return Err("a folder has no contents".into());
        }
        if !matches!(&self.open, Some((path, _)) if *path == entry.path) {
            let file = File::open(&entry.path).map_err(|e| e.to_string())?;
            self.open = Some((entry.path.clone(), file));
        }
        let Some((_, file)) = self.open.as_mut() else {
            return Err("the file is gone".into());
        };
        let wanted = request.requested_size.min(MAX_RANGE);
        let data = read_range(file, request.position, wanted).map_err(|e| e.to_string())?;
        Ok(FileContentsResponse::new_data_response(
            request.stream_id,
            data,
        ))
    }
}

/// Up to `len` bytes from `position`; fewer only at the end of the file.
pub(crate) fn read_range(file: &mut File, position: u64, len: u32) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(position))?;
    let mut data = Vec::with_capacity(len as usize);
    file.take(u64::from(len)).read_to_end(&mut data)?;
    Ok(data)
}

// ── Remote → local ───────────────────────────────────────────────────────────

/// One entry of the server's list, as it will land here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteEntry {
    /// Where it is in the server's list (entries we skip leave holes).
    pub index: usize,
    /// Relative to the download's folder.
    pub path: PathBuf,
    pub dir: bool,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
    /// Directly in what was copied (not inside a copied folder).
    pub top: bool,
}

/// A path component from the server, if it is safe to use here.
fn safe_component(part: &str) -> Option<&str> {
    let bad = part.is_empty()
        || part == "."
        || part == ".."
        || part.contains(['/', '\\', '\0', ':'])
        || part.chars().all(|c| c == '.' || c == ' ');
    (!bad).then_some(part)
}

/// Turns the server's descriptors into local relative paths. IronRDP cleans
/// the names already; anything that still looks odd is skipped, not trusted.
pub(crate) fn plan(files: &[FileDescriptor]) -> Vec<RemoteEntry> {
    let mut out = Vec::with_capacity(files.len());
    for (index, file) in files.iter().enumerate().take(MAX_ENTRIES) {
        let parent: Option<Vec<&str>> = match file.relative_path.as_deref() {
            None | Some("") => Some(Vec::new()),
            Some(path) => path.split('\\').map(safe_component).collect(),
        };
        let (Some(parent), Some(name)) = (parent, safe_component(&file.name)) else {
            warn!(name = %file.name, "skipping a file with an unusable name from the server");
            continue;
        };
        let mut path = PathBuf::new();
        for part in &parent {
            path.push(part);
        }
        path.push(name);
        let dir = file
            .attributes
            .is_some_and(|a| a.contains(ClipboardFileAttributes::DIRECTORY));
        out.push(RemoteEntry {
            index,
            path,
            dir,
            size: if dir { Some(0) } else { file.file_size },
            modified: file.last_write_time.and_then(from_filetime),
            top: parent.is_empty(),
        });
    }
    out
}

/// What a download has to say after a response.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Progress {
    /// Still going; `done` of `total` bytes are written.
    Running {
        done: u64,
        total: u64,
    },
    /// Everything is there: these go on the clipboard.
    Finished(Vec<PathBuf>),
    Failed(String),
}

/// A request in flight: which file, and which range (`None` asks the size).
#[derive(Debug, Clone, Copy)]
struct InFlight {
    index: usize,
    range: Option<(u64, u32)>,
}

/// One download of the server's files into `base`.
#[derive(Debug)]
pub(crate) struct Download {
    base: PathBuf,
    entries: Vec<RemoteEntry>,
    clip_data_id: Option<u32>,
    /// Index into `entries` of the file being fetched.
    current: usize,
    file: Option<File>,
    size: Option<u64>,
    /// Where the next new range starts.
    next: u64,
    /// Ranges a short answer left open, asked for again first.
    gaps: Vec<(u64, u32)>,
    in_flight: HashMap<u32, InFlight>,
    done: u64,
    total: u64,
    finished: bool,
}

impl Download {
    pub(crate) fn new(base: PathBuf, entries: Vec<RemoteEntry>, clip_data_id: Option<u32>) -> Self {
        let total = entries.iter().filter_map(|e| e.size).sum();
        Self {
            base,
            entries,
            clip_data_id,
            current: 0,
            file: None,
            size: None,
            next: 0,
            gaps: Vec::new(),
            in_flight: HashMap::new(),
            done: 0,
            total,
            finished: false,
        }
    }

    pub(crate) fn base(&self) -> &Path {
        &self.base
    }

    pub(crate) fn owns(&self, stream_id: u32) -> bool {
        self.in_flight.contains_key(&stream_id)
    }

    /// Starts on the files: creates folders and empty files right away and
    /// returns the first requests (or the result, if nothing needs fetching).
    pub(crate) fn start(
        &mut self,
        next_id: &mut impl FnMut() -> u32,
    ) -> Result<Vec<FileContentsRequest>, Progress> {
        std::fs::create_dir_all(&self.base).map_err(|e| Progress::Failed(e.to_string()))?;
        self.pump(next_id)
    }

    /// Fills the window with requests, moving on to the next file whenever
    /// one is complete. `Err` carries the end: finished or failed.
    fn pump(
        &mut self,
        next_id: &mut impl FnMut() -> u32,
    ) -> Result<Vec<FileContentsRequest>, Progress> {
        let mut requests = Vec::new();
        loop {
            if self.current >= self.entries.len() {
                if self.in_flight.is_empty() {
                    self.finished = true;
                    return Err(Progress::Finished(self.top_level()));
                }
                return Ok(requests);
            }
            if self.file.is_none() {
                self.open_current().map_err(Progress::Failed)?;
                if self.file.is_none() {
                    // A folder, or an empty file: nothing to fetch.
                    self.current += 1;
                    continue;
                }
            }
            let Some(size) = self.size else {
                // The descriptor had no size: ask for it first.
                if self.in_flight.is_empty() {
                    let id = next_id();
                    self.in_flight.insert(
                        id,
                        InFlight {
                            index: self.current,
                            range: None,
                        },
                    );
                    requests.push(self.request(id, None));
                }
                return Ok(requests);
            };
            while self.in_flight.len() < WINDOW {
                let range = if let Some(gap) = self.gaps.pop() {
                    gap
                } else if self.next < size {
                    let len = (size - self.next).min(u64::from(CHUNK)) as u32;
                    let range = (self.next, len);
                    self.next += u64::from(len);
                    range
                } else {
                    break;
                };
                let id = next_id();
                self.in_flight.insert(
                    id,
                    InFlight {
                        index: self.current,
                        range: Some(range),
                    },
                );
                requests.push(self.request(id, Some(range)));
            }
            if self.in_flight.is_empty() {
                // Every byte of this file is written.
                self.close_current();
                continue;
            }
            return Ok(requests);
        }
    }

    fn request(&self, stream_id: u32, range: Option<(u64, u32)>) -> FileContentsRequest {
        let (flags, position, requested_size) = match range {
            None => (FileContentsFlags::SIZE, 0, 8),
            Some((position, len)) => (FileContentsFlags::RANGE, position, len),
        };
        FileContentsRequest {
            stream_id,
            index: i32::try_from(self.entries[self.current].index).unwrap_or(i32::MAX),
            flags,
            position,
            requested_size,
            data_id: self.clip_data_id,
        }
    }

    fn open_current(&mut self) -> Result<(), String> {
        let entry = &self.entries[self.current];
        let path = self.base.join(&entry.path);
        if entry.dir {
            std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let file = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.size = entry.size;
        self.next = 0;
        self.gaps.clear();
        if entry.size == Some(0) {
            set_modified(&file, entry.modified);
            return Ok(());
        }
        self.file = Some(file);
        Ok(())
    }

    fn close_current(&mut self) {
        if let Some(file) = self.file.take() {
            set_modified(&file, self.entries[self.current].modified);
        }
        self.size = None;
        self.current += 1;
    }

    fn top_level(&self) -> Vec<PathBuf> {
        self.entries
            .iter()
            .filter(|e| e.top)
            .map(|e| self.base.join(&e.path))
            .collect()
    }

    /// Takes the answer to one of our requests.
    pub(crate) fn on_response(
        &mut self,
        stream_id: u32,
        data: Option<&[u8]>,
        next_id: &mut impl FnMut() -> u32,
    ) -> (Vec<FileContentsRequest>, Progress) {
        let Some(request) = self.in_flight.remove(&stream_id) else {
            return (Vec::new(), self.running());
        };
        let name = self.entries[request.index].path.display().to_string();
        let Some(data) = data else {
            return (
                Vec::new(),
                Progress::Failed(format!("the server could not send {name}")),
            );
        };
        match request.range {
            None => {
                let Ok(bytes) = <[u8; 8]>::try_from(data) else {
                    return (Vec::new(), Progress::Failed(format!("no size for {name}")));
                };
                let size = u64::from_le_bytes(bytes);
                self.total += size;
                self.size = Some(size);
                if size == 0 {
                    self.close_current();
                }
            }
            Some((position, len)) => {
                if data.is_empty() || data.len() > len as usize {
                    return (
                        Vec::new(),
                        Progress::Failed(format!("the server sent a broken piece of {name}")),
                    );
                }
                let Some(file) = self.file.as_mut() else {
                    return (Vec::new(), Progress::Failed(format!("{name} is not open")));
                };
                let written = file
                    .seek(SeekFrom::Start(position))
                    .and_then(|_| file.write_all(data));
                if let Err(e) = written {
                    return (Vec::new(), Progress::Failed(format!("{name}: {e}")));
                }
                self.done += data.len() as u64;
                let got = data.len() as u32;
                if got < len {
                    self.gaps.push((position + u64::from(got), len - got));
                }
            }
        }
        match self.pump(next_id) {
            Ok(requests) => (requests, self.running()),
            Err(end) => (Vec::new(), end),
        }
    }

    fn running(&self) -> Progress {
        Progress::Running {
            done: self.done,
            total: self.total.max(self.done),
        }
    }
}

impl Drop for Download {
    fn drop(&mut self) {
        // A download that didn't finish leaves nothing behind. A finished
        // one stays until the session ends (or the next one replaces it):
        // its files are on the clipboard.
        if !self.finished {
            self.file = None;
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }
}

fn set_modified(file: &File, time: Option<SystemTime>) {
    if let Some(time) = time {
        let _ = file.set_modified(time);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_cliprdr::pdu::{OwnedFormatDataResponse, PackedFileList};

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn ids() -> impl FnMut() -> u32 {
        let mut n = 0;
        move || {
            n += 1;
            n
        }
    }

    #[test]
    fn filetime_round_trips() {
        let time = UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_700);
        let ft = to_filetime(time).expect("filetime");
        assert_eq!(
            ft,
            (1_700_000_000 + FILETIME_UNIX_OFFSET) * 10_000_000 + 1_234_567
        );
        assert_eq!(from_filetime(ft), Some(time));
        assert_eq!(from_filetime(0), None, "before 1970");
    }

    #[test]
    fn a_folder_is_listed_recursively_with_relative_paths() {
        let dir = tempdir();
        let root = dir.path().join("Bericht");
        std::fs::create_dir_all(root.join("Bilder")).expect("mkdir");
        std::fs::write(root.join("a.txt"), b"hello").expect("write");
        std::fs::write(root.join("Bilder").join("b.png"), b"png!!!").expect("write");
        let single = dir.path().join("single.bin");
        std::fs::write(&single, [0u8; 3]).expect("write");

        let list = LocalList::collect(&[root.clone(), single.clone()]);
        let names: Vec<(Option<&str>, &str, bool, Option<u64>)> = list
            .descriptors
            .iter()
            .map(|d| {
                (
                    d.relative_path.as_deref(),
                    d.name.as_str(),
                    d.attributes
                        .is_some_and(|a| a.contains(ClipboardFileAttributes::DIRECTORY)),
                    d.file_size,
                )
            })
            .collect();
        assert_eq!(
            names,
            vec![
                (None, "Bericht", true, Some(0)),
                (Some("Bericht"), "Bilder", true, Some(0)),
                (Some("Bericht\\Bilder"), "b.png", false, Some(6)),
                (Some("Bericht"), "a.txt", false, Some(5)),
                (None, "single.bin", false, Some(3)),
            ]
        );
        assert_eq!(list.entries[2].path, root.join("Bilder").join("b.png"));
        assert!(list.descriptors.iter().all(|d| d.last_write_time.is_some()));

        // What goes over the wire comes back the same.
        let packed = PackedFileList {
            files: list.descriptors.clone(),
        };
        let response = OwnedFormatDataResponse::new_file_list(&packed).expect("encode");
        let decoded = response.to_file_list().expect("decode");
        assert_eq!(decoded.files.len(), 5);
        assert_eq!(decoded.files[2].name, "Bericht\\Bilder\\b.png");
        assert_eq!(decoded.files[2].file_size, Some(6));
    }

    #[test]
    fn names_too_long_for_the_descriptor_are_left_out() {
        let dir = tempdir();
        let long = dir.path().join("x".repeat(200));
        std::fs::create_dir_all(&long).expect("mkdir");
        std::fs::write(long.join("y".repeat(100)), b"").expect("write");
        let list = LocalList::collect(&[long]);
        assert_eq!(list.descriptors.len(), 1, "the folder fits, its file not");
        assert_eq!(list.entries.len(), list.descriptors.len());
    }

    fn request(
        index: i32,
        flags: FileContentsFlags,
        position: u64,
        size: u32,
    ) -> FileContentsRequest {
        FileContentsRequest {
            stream_id: 7,
            index,
            flags,
            position,
            requested_size: size,
            data_id: None,
        }
    }

    #[test]
    fn files_are_served_by_size_and_range() {
        let dir = tempdir();
        let path = dir.path().join("data.bin");
        let content: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
        std::fs::write(&path, &content).expect("write");
        let mut served = Served::default();
        served.offer(vec![
            LocalEntry {
                path: dir.path().to_owned(),
                dir: true,
            },
            LocalEntry {
                path: path.clone(),
                dir: false,
            },
        ]);

        let size = served.serve(&request(1, FileContentsFlags::SIZE, 0, 8));
        assert_eq!(size.data_as_size().expect("size"), 10_000);
        let range = served.serve(&request(1, FileContentsFlags::RANGE, 4_000, 3_000));
        assert_eq!(range.stream_id(), 7);
        assert_eq!(range.data(), &content[4_000..7_000]);
        // At the end, fewer bytes than asked.
        let tail = served.serve(&request(1, FileContentsFlags::RANGE, 9_990, 100));
        assert_eq!(tail.data(), &content[9_990..]);
        // A folder has size 0 and no contents; a wrong index is an error.
        assert_eq!(
            served
                .serve(&request(0, FileContentsFlags::SIZE, 0, 8))
                .data_as_size()
                .expect("size"),
            0
        );
        assert!(served
            .serve(&request(0, FileContentsFlags::RANGE, 0, 8))
            .is_error());
        assert!(served
            .serve(&request(5, FileContentsFlags::SIZE, 0, 8))
            .is_error());
    }

    #[test]
    fn a_locked_list_outlives_a_new_offer() {
        let dir = tempdir();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::write(&first, b"1").expect("write");
        std::fs::write(&second, b"22").expect("write");
        let entry = |path: &Path| LocalEntry {
            path: path.to_owned(),
            dir: false,
        };
        let mut served = Served::default();
        served.offer(vec![entry(&first)]);
        served.lock(3);
        served.offer(vec![entry(&second)]);
        let mut locked = request(0, FileContentsFlags::RANGE, 0, 10);
        locked.data_id = Some(3);
        assert_eq!(served.serve(&locked).data(), b"1");
        assert_eq!(
            served
                .serve(&request(0, FileContentsFlags::RANGE, 0, 10))
                .data(),
            b"22"
        );
        served.unlock(3);
        // Unknown lock: the current list.
        assert_eq!(served.serve(&locked).data(), b"22");
        served.withdraw();
        assert!(served
            .serve(&request(0, FileContentsFlags::SIZE, 0, 8))
            .is_error());
    }

    fn descriptor(
        name: &str,
        parent: Option<&str>,
        dir: bool,
        size: Option<u64>,
    ) -> FileDescriptor {
        let mut d = FileDescriptor::new(name).with_attributes(if dir {
            ClipboardFileAttributes::DIRECTORY
        } else {
            ClipboardFileAttributes::ARCHIVE
        });
        if let Some(size) = size {
            d = d.with_file_size(size);
        }
        if let Some(parent) = parent {
            d = d.with_relative_path(parent);
        }
        d
    }

    #[test]
    fn the_plan_keeps_safe_relative_paths_only() {
        let plan = plan(&[
            descriptor("Ordner", None, true, None),
            descriptor("a.txt", Some("Ordner\\Sub"), false, Some(3)),
            descriptor("evil", Some("Ordner\\.."), false, Some(1)),
            descriptor("..", None, false, Some(1)),
            descriptor("b.txt", None, false, Some(2)),
        ]);
        let paths: Vec<_> = plan.iter().map(|e| (e.path.clone(), e.top)).collect();
        assert_eq!(
            paths,
            vec![
                (PathBuf::from("Ordner"), true),
                (Path::new("Ordner").join("Sub").join("a.txt"), false),
                (PathBuf::from("b.txt"), true),
            ]
        );
    }

    /// Plays the server: answers every request from `files` (index → bytes).
    fn answer(request: &FileContentsRequest, files: &[&[u8]], short: bool) -> Option<Vec<u8>> {
        let file = files[request.index as usize];
        if request.flags.contains(FileContentsFlags::SIZE) {
            return Some((file.len() as u64).to_le_bytes().to_vec());
        }
        let start = request.position as usize;
        let mut end = (start + request.requested_size as usize).min(file.len());
        if short && end - start > 1 {
            end -= 1;
        }
        Some(file[start..end].to_vec())
    }

    #[test]
    fn a_download_fetches_everything_and_names_the_top_level() {
        let dir = tempdir();
        let base = dir.path().join("dl");
        let big: Vec<u8> = (0..(CHUNK as usize * 2 + 17))
            .map(|i| (i % 251) as u8)
            .collect();
        let small = b"hello".to_vec();
        let files: Vec<&[u8]> = vec![b"", &big, b"", &small];
        let entries = plan(&[
            descriptor("Ordner", None, true, None),
            descriptor("big.bin", Some("Ordner"), false, Some(big.len() as u64)),
            descriptor("empty.txt", None, false, Some(0)),
            // No size in the descriptor: asked for first.
            descriptor("small.txt", None, false, None),
        ]);
        let mut download = Download::new(base.clone(), entries, Some(9));
        let mut next = ids();
        let mut queue = download.start(&mut next).expect("requests");
        assert!(queue.iter().all(|r| r.data_id == Some(9)));
        assert!(queue.len() <= WINDOW);
        let mut last = None;
        let mut first = true;
        while let Some(request) = queue.pop() {
            // One short answer, to see the gap asked for again.
            let data = answer(&request, &files, first && request.requested_size > 8);
            first = false;
            let (more, progress) =
                download.on_response(request.stream_id, data.as_deref(), &mut next);
            queue.extend(more);
            last = Some(progress);
        }
        assert_eq!(
            last,
            Some(Progress::Finished(vec![
                base.join("Ordner"),
                base.join("empty.txt"),
                base.join("small.txt"),
            ]))
        );
        assert_eq!(
            std::fs::read(base.join("Ordner").join("big.bin")).expect("big"),
            big
        );
        assert_eq!(std::fs::read(base.join("small.txt")).expect("small"), small);
        assert_eq!(std::fs::read(base.join("empty.txt")).expect("empty"), b"");
        drop(download);
        assert!(base.exists(), "a finished download stays");
    }

    #[test]
    fn a_failed_or_dropped_download_leaves_nothing() {
        let dir = tempdir();
        let base = dir.path().join("dl");
        let entries = plan(&[descriptor("a.bin", None, false, Some(10))]);
        let mut download = Download::new(base.clone(), entries, None);
        let mut next = ids();
        let queue = download.start(&mut next).expect("requests");
        assert_eq!(queue.len(), 1);
        let (_, progress) = download.on_response(queue[0].stream_id, None, &mut next);
        assert!(matches!(progress, Progress::Failed(_)), "{progress:?}");
        drop(download);
        assert!(!base.exists());
    }
}
