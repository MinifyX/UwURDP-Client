//! Folders shared with a server, inside the Mac App Store's sandbox.
//!
//! A sandboxed app may open what the user handed it — through the open panel,
//! or by dropping it on the window — and only for as long as it runs. A folder
//! picked for drive redirection today is a path the app may no longer read
//! tomorrow. What survives a restart is a *security-scoped bookmark*: an opaque
//! blob the system makes for a folder the app has access to right now, and
//! which, resolved on a later run, gives that access back.
//!
//! So every folder picked in "Ordner freigeben" gets a bookmark
//! ([`remember`], in `bookmarks.json` next to the host list), and connecting
//! resolves the bookmark of every folder the host shares and switches its
//! access on ([`open`]) for as long as that session lasts: dropping the
//! [`Access`] switches it off again. A folder inside one that already has a
//! bookmark needs none of its own.
//!
//! What is *not* here, on purpose: "all drives" (it has no folder to bookmark,
//! and is refused in the store build, see `hosts.rs`), a folder that came from
//! another device through sync or from an imported `.rdp` file (no bookmark
//! until it is picked again on this Mac; the session goes on without it, as
//! for any folder that is not on this computer), and files dropped on a
//! desktop for the server's clipboard: the system grants a dropped file to the
//! app for the rest of its run, which outlasts the clipboard offer.
//!
//! Everywhere else — Windows, Linux, the Mac build from GitHub, which is not
//! sandboxed — every function here does nothing at all.

use std::path::{Path, PathBuf};

/// Where the bookmarks are kept: the folder of the host list. Called once from
/// `setup`, before the page can pick or connect anything.
pub(crate) fn configure(data_directory: &Path) {
    #[cfg(all(target_os = "macos", feature = "mas"))]
    scoped::configure(data_directory);
    #[cfg(not(all(target_os = "macos", feature = "mas")))]
    let _ = data_directory;
}

/// Keeps access to the folder `path` across restarts, if it does not have
/// that already. Called right after the user picked it, while the open panel's
/// grant is fresh. Never fails: a folder without a bookmark is a folder the
/// next run shares nothing from, and says so in the log.
pub(crate) fn remember(path: &Path) {
    #[cfg(all(target_os = "macos", feature = "mas"))]
    scoped::remember(path);
    #[cfg(not(all(target_os = "macos", feature = "mas")))]
    let _ = path;
}

/// Switches on access to `path` through its bookmark (or the bookmark of a
/// folder it is in), for as long as the returned [`Access`] lives. `None` when
/// there is nothing to switch on: no bookmark, one that no longer resolves, or
/// a build without a sandbox.
#[cfg(all(target_os = "macos", feature = "mas"))]
pub(crate) fn open(path: &Path) -> Option<Access> {
    scoped::open(path)
}

#[cfg(not(all(target_os = "macos", feature = "mas")))]
pub(crate) fn open(_path: &Path) -> Option<Access> {
    None
}

/// Access to one shared folder, switched off when dropped.
#[cfg_attr(not(all(target_os = "macos", feature = "mas")), allow(dead_code))]
pub(crate) struct Access {
    /// Where the folder is now. A folder moved in Finder keeps its bookmark,
    /// and this is its new place.
    path: PathBuf,
    #[cfg(all(target_os = "macos", feature = "mas"))]
    url: objc2::rc::Retained<objc2_foundation::NSURL>,
}

impl Access {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(all(target_os = "macos", feature = "mas"))]
impl Drop for Access {
    fn drop(&mut self) {
        // SAFETY: balances the `startAccessingSecurityScopedResource` that
        // made this `Access`, on the same URL object.
        unsafe { self.url.stopAccessingSecurityScopedResource() };
    }
}

/// The bookkeeping, apart from the system calls, so it can be tested anywhere.
#[cfg(any(test, all(target_os = "macos", feature = "mas")))]
mod store {
    use std::path::{Path, PathBuf};

    use serde::{Deserialize, Serialize};

    /// More shared folders than anybody sets up, and a ceiling on a file that
    /// is read at every start.
    pub(super) const LIMIT: usize = 256;

    #[derive(Debug, Default, Serialize, Deserialize)]
    pub(super) struct Bookmarks {
        /// Oldest first, so the ceiling drops what was picked longest ago.
        pub entries: Vec<Entry>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub(super) struct Entry {
        pub path: PathBuf,
        /// The bookmark, hex-encoded so the file stays plain JSON.
        pub bookmark: String,
    }

    impl Bookmarks {
        pub fn parse(text: &str) -> Self {
            serde_json::from_str(text).unwrap_or_default()
        }

        /// The bookmark that reaches `path`: its own, else the one of the
        /// nearest folder it is in.
        pub fn find(&self, path: &Path) -> Option<&Entry> {
            self.entries
                .iter()
                .filter(|entry| path.starts_with(&entry.path))
                .max_by_key(|entry| entry.path.components().count())
        }

        /// Adds a bookmark for `path`, replacing one it had and every one
        /// inside it, which the new one makes redundant.
        pub fn add(&mut self, path: PathBuf, bookmark: &[u8]) {
            self.entries.retain(|entry| !entry.path.starts_with(&path));
            self.entries.push(Entry {
                path,
                bookmark: hex(bookmark),
            });
            let excess = self.entries.len().saturating_sub(LIMIT);
            self.entries.drain(..excess);
        }
    }

    pub(super) fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    pub(super) fn unhex(text: &str) -> Option<Vec<u8>> {
        if !text.len().is_multiple_of(2) {
            return None;
        }
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
            .collect()
    }
}

#[cfg(all(target_os = "macos", feature = "mas"))]
mod scoped {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use objc2::rc::Retained;
    use objc2::runtime::Bool;
    use objc2_foundation::{
        NSData, NSURLBookmarkCreationOptions, NSURLBookmarkResolutionOptions, NSURL,
    };

    use super::store::{unhex, Bookmarks};
    use super::Access;

    const FILE: &str = "bookmarks.json";

    /// The bookmarks and the file they live in. `None` until [`configure`] ran.
    static STATE: Mutex<Option<(PathBuf, Bookmarks)>> = Mutex::new(None);

    fn state() -> std::sync::MutexGuard<'static, Option<(PathBuf, Bookmarks)>> {
        STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn configure(data_directory: &Path) {
        let file = data_directory.join(FILE);
        let bookmarks = std::fs::read_to_string(&file)
            .map(|text| Bookmarks::parse(&text))
            .unwrap_or_default();
        tracing::info!(
            count = bookmarks.entries.len(),
            "security-scoped bookmarks for shared folders"
        );
        *state() = Some((file, bookmarks));
    }

    pub(super) fn remember(path: &Path) {
        let mut state = state();
        let Some((file, bookmarks)) = state.as_mut() else {
            return;
        };
        // Picked again: a fresh bookmark, even if an older one covers it, so a
        // stale or broken one is replaced by what the user just chose.
        let Some(bookmark) = create(path) else {
            tracing::info!(path = %path.display(), "no bookmark for this folder");
            return;
        };
        bookmarks.add(path.to_path_buf(), &bookmark);
        save(file, bookmarks);
    }

    pub(super) fn open(path: &Path) -> Option<Access> {
        let mut state = state();
        let (file, bookmarks) = state.as_mut()?;
        let entry = bookmarks.find(path)?.clone();
        let bytes = unhex(&entry.bookmark)?;
        let Some((url, now, stale)) = resolve(&bytes) else {
            tracing::info!(path = %entry.path.display(), "bookmark no longer resolves");
            return None;
        };
        // A stale bookmark still works this once; a fresh one is made now,
        // while the access it gave is switched on.
        if stale {
            if let Some(fresh) = create(&now) {
                bookmarks.add(entry.path.clone(), &fresh);
                save(file, bookmarks);
            }
        }
        // The bookmark is the folder itself: wherever it went. One of a
        // folder it is in: the same place below that folder.
        let path = match path.strip_prefix(&entry.path) {
            Ok(rest) if !rest.as_os_str().is_empty() => now.join(rest),
            _ => now,
        };
        Some(Access { path, url })
    }

    /// A security-scoped bookmark for a folder this process can open right now.
    fn create(path: &Path) -> Option<Vec<u8>> {
        let url = NSURL::from_directory_path(path)?;
        url.bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
            NSURLBookmarkCreationOptions::WithSecurityScope,
            None,
            None,
        )
        .ok()
        .map(|data| data.to_vec())
    }

    /// Resolves a bookmark and switches its access on, until the URL's
    /// [`Access`] is dropped. The URL, the path it points at now, and whether
    /// the bookmark should be made again.
    fn resolve(bytes: &[u8]) -> Option<(Retained<NSURL>, PathBuf, bool)> {
        let data = NSData::with_bytes(bytes);
        let mut stale = Bool::NO;
        // Never a dialog and never a network volume mounted while connecting:
        // a share that is not there costs that one folder, not a hung connect.
        let options = NSURLBookmarkResolutionOptions::WithSecurityScope
            | NSURLBookmarkResolutionOptions::WithoutUI
            | NSURLBookmarkResolutionOptions::WithoutMounting;
        // SAFETY: `stale` is a valid, writable `Bool` for the whole call.
        let url = unsafe {
            NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
                &data, options, None, &mut stale,
            )
        }
        .ok()?;
        let path = url.to_file_path()?;
        // SAFETY: a plain message to a valid file URL, balanced by
        // `stopAccessingSecurityScopedResource` when the `Access` is dropped.
        if !unsafe { url.startAccessingSecurityScopedResource() } {
            return None;
        }
        Some((url, path, stale.as_bool()))
    }

    /// Written through a temporary file so a crash mid-write never leaves half
    /// a JSON document behind.
    fn save(file: &Path, bookmarks: &Bookmarks) {
        let Ok(text) = serde_json::to_string(bookmarks) else {
            return;
        };
        let temporary = file.with_extension("json.tmp");
        if let Some(folder) = file.parent() {
            let _ = std::fs::create_dir_all(folder);
        }
        if let Err(error) =
            std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, file))
        {
            tracing::warn!(%error, "bookmarks could not be saved");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::store::{hex, unhex, Bookmarks, LIMIT};

    #[test]
    fn a_folder_inside_a_bookmarked_one_uses_the_nearest_bookmark() {
        let mut bookmarks = Bookmarks::default();
        bookmarks.add(PathBuf::from("/Users/nyu/Projects"), b"outer");
        bookmarks.add(PathBuf::from("/Users/nyu/Projects/rdp/share"), b"inner");
        let find = |path: &str| bookmarks.find(Path::new(path)).map(|e| e.path.clone());
        assert_eq!(
            find("/Users/nyu/Projects/rdp/share/sub"),
            Some(PathBuf::from("/Users/nyu/Projects/rdp/share"))
        );
        assert_eq!(
            find("/Users/nyu/Projects/other"),
            Some(PathBuf::from("/Users/nyu/Projects"))
        );
        // A shared prefix of characters is not a shared folder.
        assert_eq!(find("/Users/nyu/Projects-old"), None);
    }

    #[test]
    fn a_folder_replaces_the_bookmarks_inside_it() {
        let mut bookmarks = Bookmarks::default();
        bookmarks.add(PathBuf::from("/Users/nyu/share/a"), b"a");
        bookmarks.add(PathBuf::from("/Users/nyu/elsewhere"), b"b");
        bookmarks.add(PathBuf::from("/Users/nyu/share"), b"c");
        let paths: Vec<_> = bookmarks.entries.iter().map(|e| e.path.clone()).collect();
        assert_eq!(
            paths,
            [
                PathBuf::from("/Users/nyu/elsewhere"),
                PathBuf::from("/Users/nyu/share")
            ]
        );
    }

    #[test]
    fn the_oldest_bookmarks_go_first_past_the_ceiling() {
        let mut bookmarks = Bookmarks::default();
        for i in 0..LIMIT + 3 {
            bookmarks.add(PathBuf::from(format!("/f/{i}")), &[1]);
        }
        assert_eq!(bookmarks.entries.len(), LIMIT);
        assert_eq!(bookmarks.entries[0].path, PathBuf::from("/f/3"));
    }

    #[test]
    fn bookmarks_survive_the_round_trip_through_their_file() {
        let mut bookmarks = Bookmarks::default();
        bookmarks.add(PathBuf::from("/Users/nyu/share"), &[0, 1, 0xab, 0xff]);
        let text = serde_json::to_string(&bookmarks).unwrap();
        let back = Bookmarks::parse(&text);
        assert_eq!(back.entries, bookmarks.entries);
        assert_eq!(
            unhex(&back.entries[0].bookmark).unwrap(),
            [0, 1, 0xab, 0xff]
        );
        // A damaged file is an empty list, not a failed start.
        assert!(Bookmarks::parse("{ not json").entries.is_empty());
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex("zz"), None);
        assert_eq!(hex(&[]), "");
    }

    #[test]
    fn without_a_sandbox_there_is_nothing_to_open() {
        if !cfg!(all(target_os = "macos", feature = "mas")) {
            assert!(super::open(Path::new("/Users/nyu/share")).is_none());
        }
    }
}
