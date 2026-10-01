//! Drive redirection: local folders as `\\tsclient\<name>` on the server.
//!
//! RDPDR (MS-RDPEFS) is IronRDP's `ironrdp-rdpdr`; the file system behind it
//! is ours. `ironrdp-rdpdr-native` only builds on Unix, and it trusts the
//! server's paths. Here every request goes through [`FsBackend`], which uses
//! nothing but `std::fs` and holds to three rules:
//!
//! - **Every path stays inside its shared folder.** A server path is split
//!   into components; `..`, drive letters, stream names (`a:b`) and anything
//!   with a separator of this system in it are refused before a file is
//!   touched, and what exists is canonicalised and must still be under the
//!   shared folder's canonical path — a symbolic link out of it is refused
//!   like `..` is.
//! - **Nothing the server sends panics.** No indexing, no unwrap; an unknown
//!   request gets `STATUS_NOT_SUPPORTED`. Even a PDU IronRDP itself cannot
//!   decode is answered rather than ending the session ([`SafeRdpdr`]).
//! - **Sizes are capped**: one read returns at most [`MAX_READ`], a session
//!   holds at most [`MAX_OPEN`] open files, a listing at most
//!   [`MAX_LISTING`] entries.
//!
//! Change notifications are never completed (FreeRDP does the same off
//! Windows): Explorer on the server refreshes when it is asked to.
//!
//! The work happens on the session's task, synchronously. A shared folder is
//! local, so a request takes as long as the disk does.

use crate::config::{DriveShare, SessionSettings};
use ironrdp_connector::ClientConnector;
use ironrdp_core::impl_as_any;
use ironrdp_pdu::gcc::ChannelName;
use ironrdp_pdu::PduResult;
use ironrdp_rdpdr::pdu::efs::{
    Boolean, Characteristics, ClientDriveQueryDirectoryResponse,
    ClientDriveQueryInformationResponse, ClientDriveQueryVolumeInformationResponse,
    ClientDriveSetInformationResponse, CreateDisposition, CreateOptions, DesiredAccess,
    DeviceCloseRequest, DeviceCloseResponse, DeviceControlRequest, DeviceControlResponse,
    DeviceCreateRequest, DeviceCreateResponse, DeviceIoRequest, DeviceIoResponse,
    DeviceReadRequest, DeviceReadResponse, DeviceWriteRequest, DeviceWriteResponse,
    FileAttributeTagInformation, FileAttributes, FileBasicInformation,
    FileBothDirectoryInformation, FileDirectoryInformation, FileFsAttributeInformation,
    FileFsDeviceInformation, FileFsFullSizeInformation, FileFsSizeInformation,
    FileFsVolumeInformation, FileFullDirectoryInformation, FileInformationClass,
    FileInformationClassLevel, FileNamesInformation, FileStandardInformation, FileSystemAttributes,
    FileSystemInformationClass, FileSystemInformationClassLevel, Information, NtStatus,
    ServerDeviceAnnounceResponse, ServerDriveIoRequest, ServerDriveQueryDirectoryRequest,
    ServerDriveQueryInformationRequest, ServerDriveQueryVolumeInformationRequest,
    ServerDriveSetInformationRequest,
};
use ironrdp_rdpdr::pdu::esc::{ScardCall, ScardIoCtlCode};
use ironrdp_rdpdr::pdu::RdpdrPdu;
use ironrdp_rdpdr::{Rdpdr, RdpdrBackend};
use ironrdp_svc::{CompressionCondition, SvcClientProcessor, SvcMessage, SvcProcessor};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

/// The most one read hands back. Windows asks for 64 KiB at a time.
pub(crate) const MAX_READ: u32 = 1024 * 1024;
/// Open files and folders per session; well under the 1024 descriptors a
/// Linux process gets by default, which the whole app shares.
pub(crate) const MAX_OPEN: usize = 256;
/// Entries one directory listing returns.
pub(crate) const MAX_LISTING: usize = 100_000;

// NTSTATUS values IronRDP has no constant for.
const STATUS_INVALID_PARAMETER: u32 = 0xC000_000D;
const STATUS_OBJECT_NAME_INVALID: u32 = 0xC000_0033;
const STATUS_OBJECT_NAME_NOT_FOUND: u32 = 0xC000_0034;
const STATUS_OBJECT_PATH_NOT_FOUND: u32 = 0xC000_003A;
const STATUS_INSUFFICIENT_RESOURCES: u32 = 0xC000_009A;
const STATUS_FILE_IS_A_DIRECTORY: u32 = 0xC000_00BA;
const STATUS_INVALID_DEVICE_REQUEST: u32 = 0xC000_0010;

/// `FILE_CREATED` in a create response; IronRDP lists the other three.
const FILE_CREATED: u8 = 2;
/// Seconds between 1601 (Windows' epoch) and 1970, in 100 ns.
const EPOCH_DIFFERENCE: i64 = 116_444_736_000_000_000;
/// `FILE_DEVICE_DISK`.
const FILE_DEVICE_DISK: u32 = 7;

fn status(code: u32) -> NtStatus {
    NtStatus::from(code)
}

/// Adds the drive channel when the settings share something that exists
/// here. RDPDR is only served alongside the sound channel, so that one is
/// announced too when sound itself isn't played here.
pub(crate) fn attach(connector: &mut ClientConnector, settings: &SessionSettings) {
    if settings.drives.is_empty() {
        return;
    }
    let shares = resolve_shares(&settings.drives);
    if shares.is_empty() {
        info!("no shared folder exists on this computer; drive redirection stays off");
        return;
    }
    let announced: Vec<(u32, String)> = shares
        .iter()
        .map(|(id, share)| (*id, share.name.clone()))
        .collect();
    info!(
        count = announced.len(),
        "sharing local folders with the server"
    );
    let backend = FsBackend::new(shares);
    let rdpdr =
        Rdpdr::new(Box::new(backend), settings.client_name.clone()).with_drives(Some(announced));
    connector.attach_static_channel(SafeRdpdr(rdpdr));

    let sound_here = cfg!(feature = "audio") && settings.audio == crate::AudioMode::Local;
    if !sound_here {
        connector.attach_static_channel(ironrdp_rdpsnd::client::Rdpsnd::new(Box::new(
            ironrdp_rdpsnd::client::NoopRdpsndBackend,
        )));
    }
}

/// A shared folder as the backend serves it.
#[derive(Debug, Clone)]
pub(crate) struct Share {
    pub name: String,
    /// Canonical, so that every path under it can be checked by prefix.
    pub root: PathBuf,
}

/// The shares that exist on this computer, numbered from 1 as device ids.
/// A missing one is logged and left out.
pub(crate) fn resolve_shares(drives: &[DriveShare]) -> Vec<(u32, Share)> {
    let mut wanted: Vec<(String, PathBuf)> = Vec::new();
    for drive in drives {
        if drive.path.as_os_str() == DriveShare::ALL {
            let fixed = fixed_drives();
            if fixed.is_empty() {
                info!("\"all drives\" shares nothing on this system");
            }
            wanted.extend(fixed);
        } else {
            wanted.push((drive.name.clone(), drive.path.clone()));
        }
    }

    let mut shares = Vec::new();
    let mut names = std::collections::HashSet::new();
    for (name, path) in wanted {
        let name = announce_name(&name, &path);
        match fs::canonicalize(&path) {
            Ok(root) if root.is_dir() => {
                if !names.insert(name.to_lowercase()) {
                    info!(%name, "a second shared folder of the same name is left out");
                    continue;
                }
                let id = u32::try_from(shares.len() + 1).unwrap_or(u32::MAX);
                shares.push((id, Share { name, root }));
            }
            Ok(_) => info!(path = %path.display(), "not a folder; not shared"),
            Err(error) => {
                info!(path = %path.display(), %error, "shared folder not on this computer; skipped")
            }
        }
    }
    shares
}

/// What the server shows: printable, without separators, never empty.
fn announce_name(name: &str, path: &Path) -> String {
    let clean: String = name
        .chars()
        .filter(|c| !c.is_control() && !r#"\/:*?"<>|"#.contains(*c))
        .take(32)
        .collect();
    let clean = clean.trim();
    if !clean.is_empty() {
        return clean.to_string();
    }
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Drive".into())
}

#[cfg(windows)]
fn fixed_drives() -> Vec<(String, PathBuf)> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    const DRIVE_FIXED: u32 = 3;
    // SAFETY: no arguments, returns a bit mask.
    let mask = unsafe { GetLogicalDrives() };
    (0u8..26)
        .filter(|bit| mask & (1u32 << bit) != 0)
        .filter_map(|bit| {
            let letter = char::from(b'A' + bit);
            let root: Vec<u16> = format!("{letter}:\\")
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: `root` is a NUL-terminated UTF-16 string that outlives the call.
            let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
            (kind == DRIVE_FIXED)
                .then(|| (letter.to_string(), PathBuf::from(format!("{letter}:\\"))))
        })
        .collect()
}

#[cfg(not(windows))]
fn fixed_drives() -> Vec<(String, PathBuf)> {
    Vec::new()
}

/// Bytes free for the user and in total on the volume holding `path`.
#[cfg(unix)]
fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: all-zero is a valid `statvfs`, which is plain integers.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid C string and a buffer of the right type.
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    // The field types differ between systems (u32 on some, u64 on others).
    #[allow(clippy::unnecessary_cast)]
    let (unit, free, total) = (
        (stat.f_frsize as u64).max(1),
        stat.f_bavail as u64,
        stat.f_blocks as u64,
    );
    Some((free.saturating_mul(unit), total.saturating_mul(unit)))
}

#[cfg(windows)]
fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: a NUL-terminated path and two out parameters that live long enough.
    let ok =
        unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, std::ptr::null_mut()) };
    (ok != 0).then_some((free, total))
}

#[cfg(not(any(unix, windows)))]
fn disk_space(_path: &Path) -> Option<(u64, u64)> {
    None
}

// ── The channel, made sturdy ────────────────────────────────────────────────

/// IronRDP's RDPDR channel, with one change: a PDU it cannot decode (a file
/// information class it doesn't know, say) is answered with
/// `STATUS_NOT_SUPPORTED` instead of ending the whole session.
#[derive(Debug)]
pub(crate) struct SafeRdpdr(pub Rdpdr);

impl_as_any!(SafeRdpdr);

impl SvcProcessor for SafeRdpdr {
    fn channel_name(&self) -> ChannelName {
        Rdpdr::NAME
    }

    fn compression_condition(&self) -> CompressionCondition {
        self.0.compression_condition()
    }

    fn start(&mut self) -> PduResult<Vec<SvcMessage>> {
        self.0.start()
    }

    fn process(&mut self, payload: &[u8]) -> PduResult<Vec<SvcMessage>> {
        match self.0.process(payload) {
            Ok(messages) => Ok(messages),
            Err(error) => {
                warn!(%error, "a drive request could not be handled");
                Ok(not_supported_reply(payload).into_iter().collect())
            }
        }
    }
}

impl SvcClientProcessor for SafeRdpdr {}

/// A bare "not supported" for an I/O request whose body could not be read,
/// so the server isn't left waiting on it.
fn not_supported_reply(payload: &[u8]) -> Option<SvcMessage> {
    let word16 = |at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(
            payload.get(at..at + 2)?.try_into().ok()?,
        ))
    };
    let word32 = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(
            payload.get(at..at + 4)?.try_into().ok()?,
        ))
    };
    // RDPDR_CTYP_CORE, PAKID_CORE_DEVICE_IOREQUEST.
    if word16(0)? != 0x4472 || word16(2)? != 0x4952 {
        return None;
    }
    let reply = DeviceIoResponse {
        device_id: word32(4)?,
        completion_id: word32(12)?,
        io_status: NtStatus::NOT_SUPPORTED,
    };
    Some(SvcMessage::from(RdpdrPdu::DeviceCloseResponse(
        DeviceCloseResponse {
            device_io_response: reply,
        },
    )))
}

// ── The file system ─────────────────────────────────────────────────────────

/// One open file or folder.
#[derive(Debug)]
struct Open {
    device_id: u32,
    path: PathBuf,
    /// `None` for a folder: Windows cannot open one as a `File`.
    file: Option<File>,
    writable: bool,
    delete_on_close: bool,
    /// What is left of a directory listing, between query-directory calls.
    listing: Option<VecDeque<String>>,
}

#[derive(Debug)]
pub(crate) struct FsBackend {
    shares: HashMap<u32, Share>,
    open: HashMap<u32, Open>,
    next_id: u32,
    max_open: usize,
}

impl_as_any!(FsBackend);

impl FsBackend {
    pub(crate) fn new(shares: Vec<(u32, Share)>) -> Self {
        Self {
            shares: shares.into_iter().collect(),
            open: HashMap::new(),
            next_id: 1,
            max_open: MAX_OPEN,
        }
    }

    fn share(&self, device_id: u32) -> Result<&Share, NtStatus> {
        self.shares
            .get(&device_id)
            .ok_or(status(STATUS_INVALID_DEVICE_REQUEST))
    }

    /// The open file a request names, on the device it names.
    fn opened(&mut self, request: &DeviceIoRequest) -> Result<&mut Open, NtStatus> {
        match self.open.get_mut(&request.file_id) {
            Some(open) if open.device_id == request.device_id => Ok(open),
            _ => Err(status(STATUS_INVALID_PARAMETER)),
        }
    }

    fn create(&mut self, req: &DeviceCreateRequest) -> Result<(u32, Information), NtStatus> {
        if self.open.len() >= self.max_open {
            return Err(status(STATUS_INSUFFICIENT_RESOURCES));
        }
        let root = self.share(req.device_io_request.device_id)?.root.clone();
        let path = resolve(&root, &req.path)?;
        let options = &req.create_options;
        let wants_dir = options.contains(CreateOptions::FILE_DIRECTORY_FILE);
        let wants_file = options.contains(CreateOptions::FILE_NON_DIRECTORY_FILE);
        let disposition = req.create_disposition;
        let mut writable = wants_write(&req.desired_access);

        let (file, information) = match fs::metadata(&path) {
            Ok(meta) => {
                if disposition == CreateDisposition::FILE_CREATE {
                    return Err(NtStatus::OBJECT_NAME_COLLISION);
                }
                if meta.is_dir() {
                    if wants_file {
                        return Err(status(STATUS_FILE_IS_A_DIRECTORY));
                    }
                    (None, Information::FILE_OPENED)
                } else {
                    if wants_dir {
                        return Err(NtStatus::NOT_A_DIRECTORY);
                    }
                    let truncate = matches!(
                        disposition,
                        CreateDisposition::FILE_OVERWRITE
                            | CreateDisposition::FILE_OVERWRITE_IF
                            | CreateDisposition::FILE_SUPERSEDE
                    );
                    let opened = OpenOptions::new()
                        .read(true)
                        .write(writable || truncate)
                        .truncate(truncate)
                        .open(&path);
                    let file = match opened {
                        // "Whatever I may" (MAXIMUM_ALLOWED, GENERIC_ALL) on a
                        // read-only file: reading it is allowed.
                        Err(error)
                            if error.kind() == io::ErrorKind::PermissionDenied
                                && writable
                                && !truncate =>
                        {
                            writable = false;
                            File::open(&path).map_err(io_status)?
                        }
                        other => other.map_err(io_status)?,
                    };
                    let information = if disposition == CreateDisposition::FILE_SUPERSEDE {
                        Information::FILE_SUPERSEDED
                    } else if truncate {
                        Information::FILE_OVERWRITTEN
                    } else {
                        Information::FILE_OPENED
                    };
                    (Some(file), information)
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if matches!(
                    disposition,
                    CreateDisposition::FILE_OPEN | CreateDisposition::FILE_OVERWRITE
                ) {
                    return Err(missing_status(&path));
                }
                let created = Information::from_bits_retain(FILE_CREATED);
                if wants_dir {
                    fs::create_dir(&path).map_err(io_status)?;
                    (None, created)
                } else {
                    let file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .map_err(io_status)?;
                    (Some(file), created)
                }
            }
            Err(error) => return Err(io_status(error)),
        };

        // The shared folder itself is never deleted, however it is opened.
        let delete_on_close = options.contains(CreateOptions::FILE_DELETE_ON_CLOSE) && path != root;
        let id = self.next_free_id();
        self.open.insert(
            id,
            Open {
                device_id: req.device_io_request.device_id,
                path,
                writable: file.is_some() && writable,
                file,
                delete_on_close,
                listing: None,
            },
        );
        Ok((id, information))
    }

    fn next_free_id(&mut self) -> u32 {
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if !self.open.contains_key(&id) {
                return id;
            }
        }
    }

    fn close(&mut self, request: &DeviceIoRequest) -> NtStatus {
        let Some(open) = self.open.remove(&request.file_id) else {
            return status(STATUS_INVALID_PARAMETER);
        };
        let Open {
            path,
            file,
            delete_on_close,
            ..
        } = open;
        drop(file);
        if delete_on_close {
            let removed = if path.is_dir() {
                fs::remove_dir(&path)
            } else {
                fs::remove_file(&path)
            };
            if let Err(error) = removed {
                debug!(%error, "delete on close failed");
            }
        }
        NtStatus::SUCCESS
    }

    fn read(&mut self, req: &DeviceReadRequest) -> Result<Vec<u8>, NtStatus> {
        let open = self.opened(&req.device_io_request)?;
        let file = open
            .file
            .as_mut()
            .ok_or(status(STATUS_INVALID_DEVICE_REQUEST))?;
        let length = req.length.min(MAX_READ) as usize;
        file.seek(SeekFrom::Start(req.offset)).map_err(io_status)?;
        let mut data = Vec::with_capacity(length);
        file.take(length as u64)
            .read_to_end(&mut data)
            .map_err(io_status)?;
        Ok(data)
    }

    fn write(&mut self, req: &DeviceWriteRequest) -> Result<u32, NtStatus> {
        let open = self.opened(&req.device_io_request)?;
        if !open.writable {
            return Err(NtStatus::ACCESS_DENIED);
        }
        let file = open
            .file
            .as_mut()
            .ok_or(status(STATUS_INVALID_DEVICE_REQUEST))?;
        let length = u32::try_from(req.write_data.len()).map_err(|_| invalid())?;
        file.seek(SeekFrom::Start(req.offset)).map_err(io_status)?;
        file.write_all(&req.write_data).map_err(io_status)?;
        Ok(length)
    }

    fn query_information(
        &mut self,
        req: &ServerDriveQueryInformationRequest,
    ) -> Result<FileInformationClass, NtStatus> {
        let open = self.opened(&req.device_io_request)?;
        let meta = fs::metadata(&open.path).map_err(io_status)?;
        let attributes = attributes(&meta, &open.path);
        let level = req.file_info_class_lvl.clone();
        Ok(
            if level == FileInformationClassLevel::FILE_BASIC_INFORMATION {
                FileInformationClass::Basic(FileBasicInformation {
                    creation_time: created(&meta),
                    last_access_time: filetime(meta.accessed()),
                    last_write_time: filetime(meta.modified()),
                    change_time: filetime(meta.modified()),
                    file_attributes: attributes,
                })
            } else if level == FileInformationClassLevel::FILE_STANDARD_INFORMATION {
                let size = size(&meta);
                FileInformationClass::Standard(FileStandardInformation {
                    allocation_size: size,
                    end_of_file: size,
                    number_of_links: 1,
                    delete_pending: boolean(open.delete_on_close),
                    directory: boolean(meta.is_dir()),
                })
            } else if level == FileInformationClassLevel::FILE_ATTRIBUTE_TAG_INFORMATION {
                FileInformationClass::AttributeTag(FileAttributeTagInformation {
                    file_attributes: attributes,
                    reparse_tag: 0,
                })
            } else {
                debug!(?level, "file information class not supported");
                return Err(NtStatus::NOT_SUPPORTED);
            },
        )
    }

    fn query_volume(
        &mut self,
        req: &ServerDriveQueryVolumeInformationRequest,
    ) -> Result<FileSystemInformationClass, NtStatus> {
        let device_id = req.device_io_request.device_id;
        self.opened(&req.device_io_request)?;
        let share = self.share(device_id)?;
        let level = req.fs_info_class_lvl.clone();
        // 4 KiB units; a volume whose size can't be read says 1 TiB free.
        let (free, total) = disk_space(&share.root).unwrap_or((1 << 40, 1 << 40));
        let units = |bytes: u64| i64::try_from(bytes / 4096).unwrap_or(i64::MAX);
        Ok(
            if level == FileSystemInformationClassLevel::FILE_FS_VOLUME_INFORMATION {
                FileSystemInformationClass::FileFsVolumeInformation(FileFsVolumeInformation {
                    volume_creation_time: fs::metadata(&share.root)
                        .map(|meta| created(&meta))
                        .unwrap_or(0),
                    volume_serial_number: serial(&share.name),
                    supports_objects: Boolean::False,
                    volume_label: share.name.clone(),
                })
            } else if level == FileSystemInformationClassLevel::FILE_FS_SIZE_INFORMATION {
                FileSystemInformationClass::FileFsSizeInformation(FileFsSizeInformation {
                    total_alloc_units: units(total),
                    available_alloc_units: units(free),
                    sectors_per_alloc_unit: 8,
                    bytes_per_sector: 512,
                })
            } else if level == FileSystemInformationClassLevel::FILE_FS_FULL_SIZE_INFORMATION {
                FileSystemInformationClass::FileFsFullSizeInformation(FileFsFullSizeInformation {
                    total_alloc_units: units(total),
                    caller_available_alloc_units: units(free),
                    actual_available_alloc_units: units(free),
                    sectors_per_alloc_unit: 8,
                    bytes_per_sector: 512,
                })
            } else if level == FileSystemInformationClassLevel::FILE_FS_ATTRIBUTE_INFORMATION {
                FileSystemInformationClass::FileFsAttributeInformation(FileFsAttributeInformation {
                    file_system_attributes: FileSystemAttributes::FILE_CASE_PRESERVED_NAMES
                        | FileSystemAttributes::FILE_UNICODE_ON_DISK,
                    max_component_name_len: 255,
                    file_system_name: "FAT32".into(),
                })
            } else if level == FileSystemInformationClassLevel::FILE_FS_DEVICE_INFORMATION {
                FileSystemInformationClass::FileFsDeviceInformation(FileFsDeviceInformation {
                    device_type: FILE_DEVICE_DISK,
                    characteristics: Characteristics::FILE_REMOTE_DEVICE,
                })
            } else {
                debug!(?level, "volume information class not supported");
                return Err(NtStatus::NOT_SUPPORTED);
            },
        )
    }

    fn set_information(&mut self, req: &ServerDriveSetInformationRequest) -> NtStatus {
        match self.try_set_information(req) {
            Ok(()) => NtStatus::SUCCESS,
            Err(status) => status,
        }
    }

    fn try_set_information(
        &mut self,
        req: &ServerDriveSetInformationRequest,
    ) -> Result<(), NtStatus> {
        let device_id = req.device_io_request.device_id;
        let root = self.share(device_id)?.root.clone();
        let open = self.opened(&req.device_io_request)?;
        match &req.set_buffer {
            FileInformationClass::EndOfFile(info) => {
                let file = open
                    .file
                    .as_ref()
                    .ok_or(status(STATUS_INVALID_DEVICE_REQUEST))?;
                if !open.writable {
                    return Err(NtStatus::ACCESS_DENIED);
                }
                let length = u64::try_from(info.end_of_file).map_err(|_| invalid())?;
                file.set_len(length).map_err(io_status)
            }
            // Space is not reserved ahead of writing.
            FileInformationClass::Allocation(_) => Ok(()),
            FileInformationClass::Basic(info) => {
                if let (Some(file), true) = (&open.file, open.writable) {
                    if let Some(time) = from_filetime(info.last_write_time) {
                        if let Err(error) = file.set_modified(time) {
                            debug!(%error, "could not set the modification time");
                        }
                    }
                }
                Ok(())
            }
            FileInformationClass::Disposition(info) => {
                let delete = info.delete_pending != 0;
                if delete && open.path == root {
                    return Err(NtStatus::ACCESS_DENIED);
                }
                if delete && open.path.is_dir() {
                    let mut entries = fs::read_dir(&open.path).map_err(io_status)?;
                    if entries.next().is_some() {
                        return Err(NtStatus::DIRECTORY_NOT_EMPTY);
                    }
                }
                open.delete_on_close = delete;
                Ok(())
            }
            FileInformationClass::Rename(info) => {
                if open.path == root {
                    return Err(NtStatus::ACCESS_DENIED);
                }
                let target = resolve(&root, &info.file_name)?;
                if target == root {
                    return Err(NtStatus::ACCESS_DENIED);
                }
                let exists = fs::symlink_metadata(&target).is_ok();
                let same = exists && same_file(&open.path, &target);
                if exists && !same && info.replace_if_exists == Boolean::False {
                    return Err(NtStatus::OBJECT_NAME_COLLISION);
                }
                // Windows cannot rename a file this process holds open.
                let reopen = open.file.take().is_some();
                let renamed = fs::rename(&open.path, &target);
                let path = if renamed.is_ok() {
                    target
                } else {
                    open.path.clone()
                };
                open.path = path;
                if reopen {
                    open.file = Some(
                        OpenOptions::new()
                            .read(true)
                            .write(open.writable)
                            .open(&open.path)
                            .map_err(io_status)?,
                    );
                }
                renamed.map_err(io_status)
            }
            _ => Err(NtStatus::NOT_SUPPORTED),
        }
    }

    fn query_directory(
        &mut self,
        req: &ServerDriveQueryDirectoryRequest,
    ) -> Result<FileInformationClass, NtStatus> {
        let device_id = req.device_io_request.device_id;
        let root = self.share(device_id)?.root.clone();
        let open = self.opened(&req.device_io_request)?;
        if req.initial_query != 0 {
            open.listing = None;
            let listing = list(&root, &open.path, &req.path)?;
            if listing.is_empty() {
                return Err(NtStatus::NO_SUCH_FILE);
            }
            open.listing = Some(listing);
        }
        let listing = open.listing.as_mut().ok_or(NtStatus::NO_MORE_FILES)?;
        let parent = open.path.clone();
        while let Some(name) = listing.pop_front() {
            let path = parent.join(&name);
            // An entry that is a link out of the share is not shown.
            let Ok(meta) = inside(&root, &path).and_then(|p| fs::metadata(p).map_err(io_status))
            else {
                continue;
            };
            return entry(req.file_info_class_lvl.clone(), name, &path, &meta);
        }
        Err(NtStatus::NO_MORE_FILES)
    }
}

impl RdpdrBackend for FsBackend {
    fn handle_server_device_announce_response(
        &mut self,
        pdu: ServerDeviceAnnounceResponse,
    ) -> PduResult<()> {
        if pdu.result_code != NtStatus::SUCCESS {
            warn!(device = pdu.device_id, result = ?pdu.result_code, "the server refused a shared folder");
        }
        Ok(())
    }

    fn handle_scard_call(
        &mut self,
        _req: DeviceControlRequest<ScardIoCtlCode>,
        _call: ScardCall,
    ) -> PduResult<()> {
        Ok(())
    }

    fn handle_drive_io_request(&mut self, req: ServerDriveIoRequest) -> PduResult<Vec<SvcMessage>> {
        let pdu = match req {
            ServerDriveIoRequest::ServerCreateDriveRequest(req) => {
                let (file_id, information, io_status) = match self.create(&req) {
                    Ok((id, information)) => (id, information, NtStatus::SUCCESS),
                    Err(status) => (0, Information::empty(), status),
                };
                RdpdrPdu::DeviceCreateResponse(DeviceCreateResponse {
                    device_io_reply: DeviceIoResponse::new(req.device_io_request, io_status),
                    file_id,
                    information,
                })
            }
            ServerDriveIoRequest::DeviceCloseRequest(DeviceCloseRequest { device_io_request }) => {
                let io_status = self.close(&device_io_request);
                RdpdrPdu::DeviceCloseResponse(DeviceCloseResponse {
                    device_io_response: DeviceIoResponse::new(device_io_request, io_status),
                })
            }
            ServerDriveIoRequest::DeviceReadRequest(req) => {
                let (read_data, io_status) = match self.read(&req) {
                    Ok(data) => (data, NtStatus::SUCCESS),
                    Err(status) => (Vec::new(), status),
                };
                RdpdrPdu::DeviceReadResponse(DeviceReadResponse {
                    device_io_reply: DeviceIoResponse::new(req.device_io_request, io_status),
                    read_data,
                })
            }
            ServerDriveIoRequest::DeviceWriteRequest(req) => {
                let (length, io_status) = match self.write(&req) {
                    Ok(length) => (length, NtStatus::SUCCESS),
                    Err(status) => (0, status),
                };
                RdpdrPdu::DeviceWriteResponse(DeviceWriteResponse {
                    device_io_reply: DeviceIoResponse::new(req.device_io_request, io_status),
                    length,
                })
            }
            ServerDriveIoRequest::ServerDriveQueryInformationRequest(req) => {
                let (buffer, io_status) = answer(self.query_information(&req));
                RdpdrPdu::ClientDriveQueryInformationResponse(ClientDriveQueryInformationResponse {
                    device_io_response: DeviceIoResponse::new(req.device_io_request, io_status),
                    buffer,
                })
            }
            ServerDriveIoRequest::ServerDriveQueryVolumeInformationRequest(req) => {
                let (buffer, io_status) = answer(self.query_volume(&req));
                RdpdrPdu::ClientDriveQueryVolumeInformationResponse(
                    ClientDriveQueryVolumeInformationResponse::new(
                        req.device_io_request,
                        io_status,
                        buffer,
                    ),
                )
            }
            ServerDriveIoRequest::ServerDriveSetInformationRequest(req) => {
                let io_status = self.set_information(&req);
                let reply = match ClientDriveSetInformationResponse::new(&req, io_status) {
                    Ok(reply) => reply,
                    Err(error) => {
                        warn!(%error, "set-information reply could not be built");
                        return Ok(Vec::new());
                    }
                };
                RdpdrPdu::ClientDriveSetInformationResponse(reply)
            }
            ServerDriveIoRequest::ServerDriveQueryDirectoryRequest(req) => {
                let (buffer, io_status) = answer(self.query_directory(&req));
                RdpdrPdu::ClientDriveQueryDirectoryResponse(ClientDriveQueryDirectoryResponse {
                    device_io_reply: DeviceIoResponse::new(req.device_io_request, io_status),
                    buffer,
                })
            }
            // Left pending: completing it is what a change would do, and
            // answering "not supported" makes some servers ask again at once.
            ServerDriveIoRequest::ServerDriveNotifyChangeDirectoryRequest(_) => {
                return Ok(Vec::new());
            }
            ServerDriveIoRequest::DeviceControlRequest(req) => RdpdrPdu::DeviceControlResponse(
                DeviceControlResponse::new(req, NtStatus::SUCCESS, None),
            ),
            // Byte-range locks are accepted and not enforced, like FreeRDP.
            ServerDriveIoRequest::ServerDriveLockControlRequest(req) => {
                RdpdrPdu::DeviceCloseResponse(DeviceCloseResponse {
                    device_io_response: DeviceIoResponse::new(
                        req.device_io_request,
                        NtStatus::SUCCESS,
                    ),
                })
            }
        };
        Ok(vec![SvcMessage::from(pdu)])
    }
}

fn answer<T>(result: Result<T, NtStatus>) -> (Option<T>, NtStatus) {
    match result {
        Ok(value) => (Some(value), NtStatus::SUCCESS),
        Err(status) => (None, status),
    }
}

fn invalid() -> NtStatus {
    status(STATUS_INVALID_PARAMETER)
}

fn wants_write(access: &DesiredAccess) -> bool {
    access.intersects(
        DesiredAccess::FILE_WRITE_DATA_OR_FILE_ADD_FILE
            | DesiredAccess::FILE_APPEND_DATA_OR_FILE_ADD_SUBDIRECTORY
            | DesiredAccess::GENERIC_WRITE
            | DesiredAccess::GENERIC_ALL
            | DesiredAccess::MAXIMUM_ALLOWED,
    )
}

fn io_status(error: io::Error) -> NtStatus {
    match error.kind() {
        io::ErrorKind::NotFound => status(STATUS_OBJECT_NAME_NOT_FOUND),
        io::ErrorKind::PermissionDenied => NtStatus::ACCESS_DENIED,
        io::ErrorKind::AlreadyExists => NtStatus::OBJECT_NAME_COLLISION,
        io::ErrorKind::DirectoryNotEmpty => NtStatus::DIRECTORY_NOT_EMPTY,
        io::ErrorKind::NotADirectory => NtStatus::NOT_A_DIRECTORY,
        io::ErrorKind::IsADirectory => status(STATUS_FILE_IS_A_DIRECTORY),
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidFilename => {
            status(STATUS_OBJECT_NAME_INVALID)
        }
        _ => NtStatus::UNSUCCESSFUL,
    }
}

/// A missing file whose folder exists is "not found"; a missing folder on
/// the way is "path not found", as Windows tells them apart.
fn missing_status(path: &Path) -> NtStatus {
    match path.parent() {
        Some(parent) if parent.is_dir() => status(STATUS_OBJECT_NAME_NOT_FOUND),
        _ => status(STATUS_OBJECT_PATH_NOT_FOUND),
    }
}

/// The local path for a server path under `root`, or why there is none.
///
/// The server path is relative to the share (`\dir\file.txt`, or empty for
/// the share itself). Components that could leave the share, or that this
/// system would read as more than a name, are refused outright; what is
/// left is checked again on disk against links (see [`inside`]).
pub(crate) fn resolve(root: &Path, server_path: &str) -> Result<PathBuf, NtStatus> {
    if server_path.len() > 32 * 1024 {
        return Err(status(STATUS_OBJECT_NAME_INVALID));
    }
    let mut path = root.to_path_buf();
    for part in server_path.split('\\') {
        match part {
            "" | "." => continue,
            ".." => return Err(NtStatus::ACCESS_DENIED),
            _ => {}
        }
        if !plain_component(part) {
            return Err(status(STATUS_OBJECT_NAME_INVALID));
        }
        path.push(part);
    }
    inside(root, &path)?;
    Ok(path)
}

/// One name, nothing more: no separator of this system, no drive or stream
/// (`:`), no NUL or control character, no wildcard, not too long.
fn plain_component(part: &str) -> bool {
    part.len() <= 255
        && !part.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        && {
            let mut components = Path::new(part).components();
            matches!(
                (components.next(), components.next()),
                (Some(Component::Normal(_)), None)
            )
        }
}

/// Checks that `path` — or, when it doesn't exist yet, the nearest folder
/// above it that does — is under `root` once links are followed.
fn inside(root: &Path, path: &Path) -> Result<PathBuf, NtStatus> {
    let mut probe = path;
    loop {
        match fs::canonicalize(probe) {
            Ok(real) if real.starts_with(root) => return Ok(path.to_path_buf()),
            Ok(_) => return Err(NtStatus::ACCESS_DENIED),
            Err(_) => {
                // A link whose target is missing is not followed into nowhere.
                if fs::symlink_metadata(probe).is_ok() {
                    return Err(NtStatus::ACCESS_DENIED);
                }
                probe = match probe.parent() {
                    Some(parent) if parent.starts_with(root) => parent,
                    _ => return Err(NtStatus::ACCESS_DENIED),
                };
            }
        }
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// The names in `dir` that match the last part of `pattern` (`*`, `*.txt`,
/// or one name), sorted. A pattern without wildcards names one entry.
fn list(root: &Path, dir: &Path, pattern: &str) -> Result<VecDeque<String>, NtStatus> {
    let mask = pattern.rsplit('\\').next().unwrap_or_default();
    if mask.is_empty() {
        return Err(NtStatus::NO_SUCH_FILE);
    }
    if !mask.contains(['*', '?']) {
        // One name, in the folder that was opened.
        let path = resolve(root, pattern)?;
        return Ok(
            if path.parent() == Some(dir) && fs::metadata(&path).is_ok() {
                VecDeque::from([mask.to_string()])
            } else {
                VecDeque::new()
            },
        );
    }
    let mut names: Vec<String> = fs::read_dir(dir)
        .map_err(io_status)?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| matches_mask(mask, name))
        .take(MAX_LISTING)
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    Ok(names.into())
}

/// `*` and `?` as Windows matches them, without regard to case.
fn matches_mask(mask: &str, name: &str) -> bool {
    if mask == "*" || mask == "*.*" {
        return true;
    }
    let mask: Vec<char> = mask.to_lowercase().chars().collect();
    let name: Vec<char> = name.to_lowercase().chars().collect();
    let (mut m, mut n) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while n < name.len() {
        match mask.get(m) {
            Some('?') => {
                m += 1;
                n += 1;
            }
            Some('*') => {
                star = Some(m);
                m += 1;
                mark = n;
            }
            Some(c) if Some(c) == name.get(n) => {
                m += 1;
                n += 1;
            }
            _ => match star {
                Some(s) => {
                    m = s + 1;
                    mark += 1;
                    n = mark;
                }
                None => return false,
            },
        }
    }
    mask.get(m..)
        .is_some_and(|rest| rest.iter().all(|c| *c == '*'))
}

fn entry(
    level: FileInformationClassLevel,
    name: String,
    path: &Path,
    meta: &Metadata,
) -> Result<FileInformationClass, NtStatus> {
    let attributes = attributes(meta, path);
    let (created, accessed, written) = (
        created(meta),
        filetime(meta.accessed()),
        filetime(meta.modified()),
    );
    let size = size(meta);
    Ok(
        if level == FileInformationClassLevel::FILE_BOTH_DIRECTORY_INFORMATION {
            FileInformationClass::BothDirectory(FileBothDirectoryInformation::new(
                created, accessed, written, written, size, attributes, name,
            ))
        } else if level == FileInformationClassLevel::FILE_FULL_DIRECTORY_INFORMATION {
            FileInformationClass::FullDirectory(FileFullDirectoryInformation::new(
                created, accessed, written, written, size, attributes, name,
            ))
        } else if level == FileInformationClassLevel::FILE_DIRECTORY_INFORMATION {
            FileInformationClass::Directory(FileDirectoryInformation::new(
                created, accessed, written, written, size, attributes, name,
            ))
        } else if level == FileInformationClassLevel::FILE_NAMES_INFORMATION {
            FileInformationClass::Names(FileNamesInformation::new(name))
        } else {
            debug!(?level, "directory information class not supported");
            return Err(NtStatus::NOT_SUPPORTED);
        },
    )
}

fn attributes(meta: &Metadata, path: &Path) -> FileAttributes {
    let mut attributes = if meta.is_dir() {
        FileAttributes::FILE_ATTRIBUTE_DIRECTORY
    } else {
        FileAttributes::FILE_ATTRIBUTE_ARCHIVE
    };
    if meta.permissions().readonly() {
        attributes |= FileAttributes::FILE_ATTRIBUTE_READONLY;
    }
    let hidden = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.len() > 1 && n.starts_with('.'));
    if hidden || windows_hidden(meta) {
        attributes |= FileAttributes::FILE_ATTRIBUTE_HIDDEN;
    }
    attributes
}

#[cfg(windows)]
fn windows_hidden(meta: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x2 != 0
}

#[cfg(not(windows))]
fn windows_hidden(_meta: &Metadata) -> bool {
    false
}

fn size(meta: &Metadata) -> i64 {
    if meta.is_dir() {
        0
    } else {
        i64::try_from(meta.len()).unwrap_or(i64::MAX)
    }
}

fn boolean(value: bool) -> Boolean {
    if value {
        Boolean::True
    } else {
        Boolean::False
    }
}

fn created(meta: &Metadata) -> i64 {
    filetime(meta.created().or_else(|_| meta.modified()))
}

/// A time as Windows counts it: 100 ns since 1601. 0 when unknown.
fn filetime(time: io::Result<SystemTime>) -> i64 {
    let Ok(time) = time else { return 0 };
    let ticks = match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_nanos() / 100).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_nanos() / 100).unwrap_or(i64::MAX),
    };
    ticks.saturating_add(EPOCH_DIFFERENCE).max(0)
}

/// The other way round; `None` for "don't change" (0 and -1 in MS-FSCC).
fn from_filetime(ticks: i64) -> Option<SystemTime> {
    if ticks <= 0 {
        return None;
    }
    let since_1970 = ticks.checked_sub(EPOCH_DIFFERENCE)?;
    let duration =
        std::time::Duration::from_nanos(u64::try_from(since_1970).ok()?.checked_mul(100)?);
    UNIX_EPOCH.checked_add(duration)
}

/// A stable number per share name, for the volume's serial number.
fn serial(name: &str) -> u32 {
    name.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}

#[cfg(test)]
mod tests;
