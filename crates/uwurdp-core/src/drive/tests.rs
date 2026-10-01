//! The file system backend on a temporary folder: what the server may do,
//! and what it may not.

use super::*;
use ironrdp_rdpdr::pdu::efs::{
    FileDispositionInformation, FileEndOfFileInformation, FileRenameInformation, MajorFunction,
    MinorFunction, SharedAccess,
};

/// A folder of its own per test, removed afterwards.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("uwurdp-drive-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("share")).unwrap();
        Self(fs::canonicalize(dir).unwrap())
    }

    fn share(&self) -> PathBuf {
        self.0.join("share")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const DEVICE: u32 = 1;

fn backend(scratch: &Scratch) -> FsBackend {
    FsBackend::new(vec![(
        DEVICE,
        Share {
            name: "Share".into(),
            root: scratch.share(),
        },
    )])
}

fn io(major: MajorFunction, file_id: u32) -> DeviceIoRequest {
    DeviceIoRequest {
        device_id: DEVICE,
        file_id,
        completion_id: 7,
        major_function: major,
        minor_function: MinorFunction::from(0),
    }
}

fn create_request(
    path: &str,
    disposition: CreateDisposition,
    options: CreateOptions,
) -> DeviceCreateRequest {
    DeviceCreateRequest {
        device_io_request: io(MajorFunction::Create, 0),
        desired_access: DesiredAccess::GENERIC_READ | DesiredAccess::GENERIC_WRITE,
        allocation_size: 0,
        file_attributes: FileAttributes::empty(),
        shared_access: SharedAccess::empty(),
        create_disposition: disposition,
        create_options: options,
        path: path.into(),
    }
}

fn open(fs: &mut FsBackend, path: &str, disposition: CreateDisposition) -> Result<u32, NtStatus> {
    fs.create(&create_request(path, disposition, CreateOptions::empty()))
        .map(|(id, _)| id)
}

fn write(fs: &mut FsBackend, id: u32, offset: u64, data: &[u8]) -> Result<u32, NtStatus> {
    fs.write(&DeviceWriteRequest {
        device_io_request: io(MajorFunction::Write, id),
        offset,
        write_data: data.to_vec(),
    })
}

fn read(fs: &mut FsBackend, id: u32, offset: u64, length: u32) -> Result<Vec<u8>, NtStatus> {
    fs.read(&DeviceReadRequest {
        device_io_request: io(MajorFunction::Read, id),
        length,
        offset,
    })
}

fn close(fs: &mut FsBackend, id: u32) -> NtStatus {
    fs.close(&io(MajorFunction::Close, id))
}

fn set(fs: &mut FsBackend, id: u32, buffer: FileInformationClass) -> NtStatus {
    fs.set_information(&ServerDriveSetInformationRequest {
        device_io_request: io(MajorFunction::SetInformation, id),
        set_buffer: buffer,
    })
}

fn list_all(fs: &mut FsBackend, id: u32, pattern: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut initial = 1;
    loop {
        let request = ServerDriveQueryDirectoryRequest {
            device_io_request: io(MajorFunction::DirectoryControl, id),
            file_info_class_lvl: FileInformationClassLevel::FILE_BOTH_DIRECTORY_INFORMATION,
            initial_query: initial,
            path: pattern.into(),
        };
        initial = 0;
        match fs.query_directory(&request) {
            Ok(FileInformationClass::BothDirectory(info)) => names.push(info.file_name),
            Ok(other) => panic!("unexpected {other:?}"),
            Err(status)
                if status == NtStatus::NO_MORE_FILES || status == NtStatus::NO_SUCH_FILE =>
            {
                return names
            }
            Err(status) => panic!("listing failed: {status:?}"),
        }
    }
}

#[test]
fn files_are_created_written_read_and_listed() {
    let scratch = Scratch::new();
    let mut fs = backend(&scratch);

    let id = open(&mut fs, "\\notes.txt", CreateDisposition::FILE_CREATE).unwrap();
    assert_eq!(write(&mut fs, id, 0, b"hello world"), Ok(11));
    assert_eq!(write(&mut fs, id, 6, b"there"), Ok(5));
    assert_eq!(read(&mut fs, id, 0, 100).unwrap(), b"hello there");
    assert_eq!(read(&mut fs, id, 6, 3).unwrap(), b"the");
    assert_eq!(
        read(&mut fs, id, 100, 3).unwrap(),
        b"",
        "past the end is empty"
    );
    assert_eq!(close(&mut fs, id), NtStatus::SUCCESS);
    assert_eq!(
        fs::read(scratch.share().join("notes.txt")).unwrap(),
        b"hello there"
    );

    // Creating it again collides; opening it truncating starts over.
    assert_eq!(
        open(&mut fs, "\\notes.txt", CreateDisposition::FILE_CREATE),
        Err(NtStatus::OBJECT_NAME_COLLISION)
    );
    let id = open(&mut fs, "\\notes.txt", CreateDisposition::FILE_OVERWRITE_IF).unwrap();
    assert_eq!(read(&mut fs, id, 0, 100).unwrap(), b"");
    close(&mut fs, id);

    // A folder, a file in it, and the listing.
    fs.create(&create_request(
        "\\Docs",
        CreateDisposition::FILE_CREATE,
        CreateOptions::FILE_DIRECTORY_FILE,
    ))
    .unwrap();
    let id = open(&mut fs, "\\Docs\\a.TXT", CreateDisposition::FILE_OPEN_IF).unwrap();
    close(&mut fs, id);
    let root = open(&mut fs, "", CreateDisposition::FILE_OPEN).unwrap();
    assert_eq!(list_all(&mut fs, root, "\\*"), ["Docs", "notes.txt"]);
    let docs = open(&mut fs, "\\Docs", CreateDisposition::FILE_OPEN).unwrap();
    assert_eq!(
        list_all(&mut fs, docs, "\\Docs\\*.txt"),
        ["a.TXT"],
        "masks ignore case"
    );
    assert_eq!(list_all(&mut fs, docs, "\\Docs\\a.TXT"), ["a.TXT"]);
    assert_eq!(
        list_all(&mut fs, docs, "\\Docs\\*.pdf"),
        Vec::<String>::new()
    );

    // Information about what is open.
    let info = fs
        .query_information(&ServerDriveQueryInformationRequest {
            device_io_request: io(MajorFunction::QueryInformation, docs),
            file_info_class_lvl: FileInformationClassLevel::FILE_STANDARD_INFORMATION,
        })
        .unwrap();
    assert!(matches!(
        info,
        FileInformationClass::Standard(FileStandardInformation {
            directory: Boolean::True,
            ..
        })
    ));
    let volume = fs
        .query_volume(&ServerDriveQueryVolumeInformationRequest {
            device_io_request: io(MajorFunction::QueryVolumeInformation, root),
            fs_info_class_lvl: FileSystemInformationClassLevel::FILE_FS_VOLUME_INFORMATION,
        })
        .unwrap();
    assert!(matches!(
        volume,
        FileSystemInformationClass::FileFsVolumeInformation(FileFsVolumeInformation { ref volume_label, .. })
            if volume_label == "Share"
    ));
}

#[test]
fn opening_what_is_not_there_says_so() {
    let scratch = Scratch::new();
    let mut fs = backend(&scratch);
    assert_eq!(
        open(&mut fs, "\\missing.txt", CreateDisposition::FILE_OPEN),
        Err(status(STATUS_OBJECT_NAME_NOT_FOUND))
    );
    assert_eq!(
        open(
            &mut fs,
            "\\nowhere\\missing.txt",
            CreateDisposition::FILE_OPEN
        ),
        Err(status(STATUS_OBJECT_PATH_NOT_FOUND))
    );
    assert_eq!(read(&mut fs, 99, 0, 1), Err(invalid()), "an unknown handle");
    assert_eq!(close(&mut fs, 99), invalid());
}

#[test]
fn files_are_renamed_resized_and_deleted() {
    let scratch = Scratch::new();
    let share = scratch.share();
    let mut fs = backend(&scratch);

    let id = open(&mut fs, "\\old.txt", CreateDisposition::FILE_CREATE).unwrap();
    write(&mut fs, id, 0, b"0123456789").unwrap();
    let rename = |name: &str, replace: bool| {
        FileInformationClass::Rename(FileRenameInformation {
            replace_if_exists: if replace {
                Boolean::True
            } else {
                Boolean::False
            },
            file_name: name.into(),
        })
    };
    assert_eq!(
        set(&mut fs, id, rename("\\new.txt", false)),
        NtStatus::SUCCESS
    );
    assert!(!share.join("old.txt").exists());
    // The handle still works after the rename.
    assert_eq!(read(&mut fs, id, 0, 4).unwrap(), b"0123");
    assert_eq!(
        set(
            &mut fs,
            id,
            FileInformationClass::EndOfFile(FileEndOfFileInformation { end_of_file: 4 })
        ),
        NtStatus::SUCCESS
    );
    close(&mut fs, id);
    assert_eq!(fs::read(share.join("new.txt")).unwrap(), b"0123");

    // Renaming onto something that exists needs "replace".
    fs::write(share.join("other.txt"), b"x").unwrap();
    let id = open(&mut fs, "\\new.txt", CreateDisposition::FILE_OPEN).unwrap();
    assert_eq!(
        set(&mut fs, id, rename("\\other.txt", false)),
        NtStatus::OBJECT_NAME_COLLISION
    );
    assert_eq!(
        set(&mut fs, id, rename("\\other.txt", true)),
        NtStatus::SUCCESS
    );
    close(&mut fs, id);
    assert_eq!(fs::read(share.join("other.txt")).unwrap(), b"0123");

    // Deleting: marked, then gone at close.
    let id = open(&mut fs, "\\other.txt", CreateDisposition::FILE_OPEN).unwrap();
    let delete =
        FileInformationClass::Disposition(FileDispositionInformation { delete_pending: 1 });
    assert_eq!(set(&mut fs, id, delete.clone()), NtStatus::SUCCESS);
    assert!(share.join("other.txt").exists());
    close(&mut fs, id);
    assert!(!share.join("other.txt").exists());

    // A folder with something in it is not deleted; an empty one is.
    fs::create_dir(share.join("full")).unwrap();
    fs::write(share.join("full").join("f"), b"").unwrap();
    let id = open(&mut fs, "\\full", CreateDisposition::FILE_OPEN).unwrap();
    assert_eq!(
        set(&mut fs, id, delete.clone()),
        NtStatus::DIRECTORY_NOT_EMPTY
    );
    close(&mut fs, id);
    fs::remove_file(share.join("full").join("f")).unwrap();
    let id = open(&mut fs, "\\full", CreateDisposition::FILE_OPEN).unwrap();
    assert_eq!(set(&mut fs, id, delete.clone()), NtStatus::SUCCESS);
    close(&mut fs, id);
    assert!(!share.join("full").exists());

    // The share itself stays.
    let root = open(&mut fs, "\\", CreateDisposition::FILE_OPEN).unwrap();
    assert_eq!(set(&mut fs, root, delete), NtStatus::ACCESS_DENIED);
    assert_eq!(
        set(&mut fs, root, rename("\\gone", true)),
        NtStatus::ACCESS_DENIED
    );
    close(&mut fs, root);
    assert!(share.is_dir());
}

#[test]
fn nothing_outside_the_share_can_be_reached() {
    let scratch = Scratch::new();
    fs::write(scratch.0.join("secret.txt"), b"secret").unwrap();
    let mut fs = backend(&scratch);

    for path in [
        "\\..\\secret.txt",
        "\\sub\\..\\..\\secret.txt",
        "..",
        "\\C:\\Windows",
        "\\a:stream",
        "\\a/../../secret.txt",
        "\\nul\0byte",
        "\\star*",
    ] {
        assert!(
            open(&mut fs, path, CreateDisposition::FILE_OPEN_IF).is_err(),
            "{path:?} must be refused"
        );
    }
    let absolute = format!("\\{}", scratch.0.join("secret.txt").display());
    assert!(open(&mut fs, &absolute, CreateDisposition::FILE_OPEN).is_err());

    // Renaming out of the share is refused as well.
    let id = open(&mut fs, "\\in.txt", CreateDisposition::FILE_CREATE).unwrap();
    let out = FileInformationClass::Rename(FileRenameInformation {
        replace_if_exists: Boolean::True,
        file_name: "\\..\\escaped.txt".into(),
    });
    assert_eq!(set(&mut fs, id, out), NtStatus::ACCESS_DENIED);
    close(&mut fs, id);
    assert!(!scratch.0.join("escaped.txt").exists());

    // A request for a device that was never announced gets nowhere.
    let mut request = create_request(
        "\\x",
        CreateDisposition::FILE_OPEN_IF,
        CreateOptions::empty(),
    );
    request.device_io_request.device_id = 42;
    assert!(fs.create(&request).is_err());
    assert_eq!(fs::read(scratch.0.join("secret.txt")).unwrap(), b"secret");
}

#[cfg(unix)]
#[test]
fn a_link_out_of_the_share_is_not_followed() {
    let scratch = Scratch::new();
    let share = scratch.share();
    fs::create_dir(scratch.0.join("outside")).unwrap();
    fs::write(scratch.0.join("outside").join("secret.txt"), b"secret").unwrap();
    std::os::unix::fs::symlink(scratch.0.join("outside"), share.join("door")).unwrap();
    std::os::unix::fs::symlink(scratch.0.join("missing"), share.join("dangling")).unwrap();
    fs::write(share.join("ok.txt"), b"ok").unwrap();
    std::os::unix::fs::symlink(share.join("ok.txt"), share.join("inner")).unwrap();
    let mut fs = backend(&scratch);

    assert_eq!(
        open(&mut fs, "\\door\\secret.txt", CreateDisposition::FILE_OPEN),
        Err(NtStatus::ACCESS_DENIED)
    );
    assert_eq!(
        open(&mut fs, "\\door\\new.txt", CreateDisposition::FILE_CREATE),
        Err(NtStatus::ACCESS_DENIED)
    );
    assert_eq!(
        open(&mut fs, "\\dangling", CreateDisposition::FILE_OPEN_IF),
        Err(NtStatus::ACCESS_DENIED)
    );
    assert!(!scratch.0.join("missing").exists());
    // A link that stays inside works, and the one outside isn't listed.
    let id = open(&mut fs, "\\inner", CreateDisposition::FILE_OPEN).unwrap();
    assert_eq!(read(&mut fs, id, 0, 10).unwrap(), b"ok");
    let root = open(&mut fs, "", CreateDisposition::FILE_OPEN).unwrap();
    assert_eq!(list_all(&mut fs, root, "\\*"), ["inner", "ok.txt"]);
}

#[test]
fn sizes_and_handles_are_capped() {
    let scratch = Scratch::new();
    fs::write(
        scratch.share().join("big.bin"),
        vec![1u8; MAX_READ as usize + 10],
    )
    .unwrap();
    let mut fs = backend(&scratch);
    let id = open(&mut fs, "\\big.bin", CreateDisposition::FILE_OPEN).unwrap();
    assert_eq!(
        read(&mut fs, id, 0, u32::MAX).unwrap().len(),
        MAX_READ as usize
    );

    fs.max_open = 16;
    for _ in 1..fs.max_open {
        open(&mut fs, "\\big.bin", CreateDisposition::FILE_OPEN).unwrap();
    }
    assert_eq!(
        open(&mut fs, "\\big.bin", CreateDisposition::FILE_OPEN),
        Err(status(STATUS_INSUFFICIENT_RESOURCES))
    );
}

#[test]
fn masks_match_like_windows() {
    assert!(matches_mask("*", "anything"));
    assert!(matches_mask("*.txt", "A.TXT"));
    assert!(matches_mask("a?c", "abc"));
    assert!(matches_mask("a*c*", "abxcyz"));
    assert!(!matches_mask("*.txt", "a.txt.bak"));
    assert!(!matches_mask("a?c", "ac"));
}

#[test]
fn times_survive_the_trip_to_windows_and_back() {
    let now = SystemTime::now();
    let ticks = filetime(Ok(now));
    let back = from_filetime(ticks).unwrap();
    let drift = now.duration_since(back).unwrap_or_default();
    assert!(drift < std::time::Duration::from_micros(1));
    assert_eq!(from_filetime(0), None);
    assert_eq!(from_filetime(-1), None);
    assert_eq!(filetime(Ok(UNIX_EPOCH)), EPOCH_DIFFERENCE);
}

#[test]
fn missing_folders_are_skipped_and_names_are_cleaned() {
    let scratch = Scratch::new();
    let shares = resolve_shares(&[
        DriveShare {
            name: "Pro:jects".into(),
            path: scratch.share(),
        },
        DriveShare {
            name: "Gone".into(),
            path: scratch.0.join("not-here"),
        },
        DriveShare {
            name: "projects".into(),
            path: scratch.0.clone(),
        },
    ]);
    assert_eq!(
        shares.len(),
        1,
        "the missing one and the duplicate name are left out"
    );
    assert_eq!(shares[0].0, 1);
    assert_eq!(shares[0].1.name, "Projects");
}

#[test]
fn a_request_ironrdp_cannot_read_is_answered_not_fatal() {
    let scratch = Scratch::new();
    let shares = resolve_shares(&[DriveShare {
        name: "Share".into(),
        path: scratch.share(),
    }]);
    let rdpdr = Rdpdr::new(Box::new(FsBackend::new(shares)), "test".into())
        .with_drives(Some(vec![(1, "Share".into())]));
    let mut channel = SafeRdpdr(rdpdr);

    // A set-information request with a class no one knows.
    let mut pdu = Vec::new();
    pdu.extend_from_slice(&0x4472u16.to_le_bytes());
    pdu.extend_from_slice(&0x4952u16.to_le_bytes());
    for word in [1u32, 5, 77, 0x6 /* IRP_MJ_SET_INFORMATION */, 0] {
        pdu.extend_from_slice(&word.to_le_bytes());
    }
    pdu.extend_from_slice(&0x99u32.to_le_bytes());
    pdu.extend_from_slice(&0u32.to_le_bytes());
    pdu.extend_from_slice(&[0; 24]);

    let replies = channel.process(&pdu).expect("not an error for the session");
    assert_eq!(replies.len(), 1, "the server hears back");
    // Garbage is dropped quietly.
    assert!(channel.process(&[1, 2, 3]).unwrap().is_empty());
}

#[test]
fn a_whole_request_round_trips_through_the_backend() {
    let scratch = Scratch::new();
    let mut fs = backend(&scratch);
    let replies = fs
        .handle_drive_io_request(ServerDriveIoRequest::ServerCreateDriveRequest(
            create_request(
                "\\x.txt",
                CreateDisposition::FILE_CREATE,
                CreateOptions::FILE_NON_DIRECTORY_FILE,
            ),
        ))
        .unwrap();
    assert_eq!(replies.len(), 1);
    assert!(scratch.share().join("x.txt").exists());
    // A change notification is left pending: no reply.
    let replies = fs
        .handle_drive_io_request(
            ServerDriveIoRequest::ServerDriveNotifyChangeDirectoryRequest(
                ironrdp_rdpdr::pdu::efs::ServerDriveNotifyChangeDirectoryRequest {
                    device_io_request: io(MajorFunction::DirectoryControl, 1),
                    watch_tree: 0,
                    completion_filter: 0,
                },
            ),
        )
        .unwrap();
    assert!(replies.is_empty());
}

#[test]
fn a_read_only_file_opens_for_reading_when_more_was_asked() {
    let scratch = Scratch::new();
    let path = scratch.share().join("ro.txt");
    fs::write(&path, b"read me").unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions.clone()).unwrap();
    // As root (or an administrator) nothing is read-only; then there's nothing to see.
    if OpenOptions::new().write(true).open(&path).is_err() {
        let mut fs = backend(&scratch);
        let id = open(&mut fs, "\\ro.txt", CreateDisposition::FILE_OPEN).unwrap();
        assert_eq!(read(&mut fs, id, 0, 100).unwrap(), b"read me");
        assert_eq!(write(&mut fs, id, 0, b"x"), Err(NtStatus::ACCESS_DENIED));
        close(&mut fs, id);
    }
    // Windows won't remove a read-only file with the folder.
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&path, permissions).unwrap();
}
