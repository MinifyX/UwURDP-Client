//! Installing, updating and removing UwURDP on macOS and Linux, for the
//! current user.
//!
//! - **macOS:** `UwURDP.app` into `/Applications`, or `~/Applications` when
//!   this user may not write there. No shortcuts: the app shows up in
//!   Launchpad and Spotlight by being there.
//! - **Linux:** the app as an AppDir — an AppImage, already unpacked — under
//!   `~/.local/share/uwurdp`, with menu entries in
//!   `~/.local/share/applications` and, if wanted, on the desktop. Unpacked,
//!   because an AppImage that runs as one needs FUSE, which many systems no
//!   longer ship; everything else it brings (WebKitGTK included) stays inside.
//!
//! No administrator rights either way. What was installed where is kept in a
//! small file (`install.json`) in the setup's own config folder, which
//! UwURDP's updater reads too: an app installed some other way — a `.deb`, a
//! copy dragged out of a disk image — is not one the setup updates.
//!
//! `UWURDP_SETUP_SANDBOX=<folder>` redirects all of it for testing.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::system;

#[cfg(target_os = "macos")]
mod names {
    pub const APP: &str = "UwURDP.app";
}
#[cfg(not(target_os = "macos"))]
mod names {
    pub const APP: &str = "UwURDP";
}
use names::APP;

pub const APP_ID: &str = "app.uwurdp.desktop";
const SETUP_ID: &str = "app.uwurdp.setup";

/// The app in a tar archive (see `build.rs`).
static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.zst"));
const PAYLOAD_SIZE: &str = env!("UWURDP_SETUP_PAYLOAD_SIZE");
/// The compressor's window, which the decompressor has to allow.
const WINDOW_LOG: u32 = 28;

pub fn has_payload() -> bool {
    !PAYLOAD.is_empty()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    pub dir: String,
    pub desktop_shortcut: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Installed {
    pub dir: String,
    pub version: Option<String>,
    /// Only Windows had an older installer; here always false.
    pub legacy: bool,
}

/// What `install.json` holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    dir: String,
    version: String,
    desktop_shortcut: bool,
}

/// Where things go on this machine.
pub struct Layout {
    pub default_dir: PathBuf,
    /// The setup's `install.json`.
    state: PathBuf,
    /// Linux: where menu entries go. Unused on macOS.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    applications: PathBuf,
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    desktop: PathBuf,
    /// Everything UwURDP keeps: removed on request.
    data: Vec<PathBuf>,
    pub sandbox: bool,
}

impl Layout {
    pub fn detect() -> Self {
        match std::env::var_os("UWURDP_SETUP_SANDBOX").filter(|dir| !dir.is_empty()) {
            Some(dir) => Self::sandbox(Path::new(&dir)),
            None => Self::real(),
        }
    }

    #[cfg(target_os = "macos")]
    fn real() -> Self {
        let home = system::home();
        let library = home.join("Library");
        let system_apps = PathBuf::from("/Applications");
        let default_dir = if system::writable(&system_apps) {
            system_apps
        } else {
            home.join("Applications")
        };
        let data = vec![
            library.join("Application Support").join(APP_ID),
            library.join("Caches").join(APP_ID),
            library.join("WebKit").join(APP_ID),
            library
                .join("Saved Application State")
                .join(format!("{APP_ID}.savedState")),
        ];
        Self {
            default_dir,
            state: library
                .join("Application Support")
                .join(SETUP_ID)
                .join("install.json"),
            applications: PathBuf::new(),
            desktop: PathBuf::new(),
            data,
            sandbox: false,
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn real() -> Self {
        let data_home = system::xdg("XDG_DATA_HOME", ".local/share");
        let config_home = system::xdg("XDG_CONFIG_HOME", ".config");
        let cache_home = system::xdg("XDG_CACHE_HOME", ".cache");
        let data = vec![
            data_home.join(APP_ID),
            config_home.join(APP_ID),
            cache_home.join(APP_ID),
        ];
        Self {
            default_dir: data_home.join("uwurdp"),
            state: config_home.join(SETUP_ID).join("install.json"),
            applications: data_home.join("applications"),
            desktop: system::desktop_dir(),
            data,
            sandbox: false,
        }
    }

    pub fn sandbox(root: &Path) -> Self {
        Self {
            default_dir: root.join("Programs"),
            state: root.join("Setup").join("install.json"),
            applications: root.join("Applications"),
            desktop: root.join("Desktop"),
            data: vec![root.join("Data").join(APP_ID)],
            sandbox: true,
        }
    }

    fn record(&self) -> Option<Record> {
        let text = std::fs::read_to_string(&self.state).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// What is installed right now, if anything.
    pub fn installed(&self) -> Option<Installed> {
        if let Some(record) = self.record() {
            if Path::new(&record.dir).join(APP).exists() {
                return Some(Installed {
                    dir: record.dir,
                    version: Some(record.version),
                    legacy: false,
                });
            }
        }
        // Put there by hand — out of a disk image, say. Its version is not
        // known, so an update will not touch it, but a reinstall can.
        self.default_dir.join(APP).exists().then(|| Installed {
            dir: self.default_dir.display().to_string(),
            version: None,
            legacy: false,
        })
    }

    /// Options from the last install, or the defaults.
    pub fn remembered_options(&self) -> Options {
        match self.record() {
            Some(record) => Options {
                dir: record.dir,
                desktop_shortcut: record.desktop_shortcut,
            },
            None => Options {
                dir: self.default_dir.display().to_string(),
                desktop_shortcut: !cfg!(target_os = "macos"),
            },
        }
    }
}

/// On macOS the app goes straight into the picked folder; elsewhere into an
/// `uwurdp` folder inside it, unless it is one already.
pub fn folder_for(picked: PathBuf) -> PathBuf {
    if cfg!(target_os = "macos")
        || picked
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("uwurdp"))
    {
        picked
    } else {
        picked.join("uwurdp")
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    Prepare,
    Copy,
    Shortcuts,
    Register,
    Cleanup,
    Done,
}

pub type Progress<'a> = &'a mut dyn FnMut(Step, f64);

/// Stops UwURDP if it runs from `dir`. Skipped in the sandbox, which must
/// never touch real processes.
pub fn stop_app(layout: &Layout, dir: &Path) -> Result<(), String> {
    if layout.sandbox {
        return Ok(());
    }
    system::stop_processes_under(&dir.join(APP), "UwURDP")
}

/// UwURDP is open. Closing it ends every open remote session, so the setup
/// asks first.
pub fn app_running(layout: &Layout, dir: &Path) -> bool {
    !layout.sandbox && !system::processes_under(&dir.join(APP)).is_empty()
}

/// Starts the installed UwURDP. Nothing in the sandbox.
pub fn launch(layout: &Layout, dir: &Path) -> Result<(), String> {
    if layout.sandbox {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        system::spawn_detached(
            Path::new("/usr/bin/open"),
            &[&dir.join(APP).to_string_lossy()],
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        system::spawn_detached(&dir.join(APP).join("AppRun"), &[])
    }
}

/// A reader that tells how far it got.
struct Counting<'a, R> {
    inner: R,
    read: u64,
    report: &'a mut dyn FnMut(u64),
}

impl<R: Read> Read for Counting<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        (self.report)(self.read);
        Ok(n)
    }
}

/// Unpack the app into `staging`, reporting progress over the copy step.
fn unpack(staging: &Path, progress: Progress) -> Result<(), String> {
    if PAYLOAD.is_empty() {
        return Err("This setup was built without UwURDP inside (a development build).".into());
    }
    let damaged = |e: std::io::Error| format!("The packed app is damaged: {e}");
    let total: u64 = PAYLOAD_SIZE.parse().unwrap_or(1).max(1);
    let mut decoder = zstd::Decoder::new(PAYLOAD).map_err(damaged)?;
    decoder.window_log_max(WINDOW_LOG).map_err(damaged)?;
    let mut report = |read: u64| progress(Step::Copy, read.min(total) as f64 / total as f64);
    let counting = Counting {
        inner: decoder,
        read: 0,
        report: &mut report,
    };
    let mut archive = tar::Archive::new(counting);
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(false);
    archive.set_overwrite(true);
    for entry in archive.entries().map_err(damaged)? {
        let mut entry = entry.map_err(damaged)?;
        let top = entry
            .path()
            .map_err(damaged)?
            .components()
            .next()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        if top != APP {
            return Err(format!("The packed app holds something unexpected: {top}"));
        }
        // `unpack_in` keeps every entry inside `staging`: absolute paths and
        // `..` in an archive are refused rather than followed.
        let placed = entry
            .unpack_in(staging)
            .map_err(|e| format!("Couldn't unpack {top}: {e}"))?;
        if !placed {
            return Err(format!("The packed app tried to write outside {top}."));
        }
    }
    if !staging.join(APP).is_dir() {
        return Err(format!("The packed app has no {APP} in it."));
    }
    Ok(())
}

/// Unpacks everything into `dir` (which must not exist yet) and checks that
/// the app arrived with an executable to start — `--check-payload`, for CI.
pub fn check_payload(dir: &Path) -> Result<String, String> {
    use std::os::unix::fs::PermissionsExt;
    if dir.exists() {
        return Err(format!("{} exists already.", dir.display()));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
    unpack(dir, &mut |_, _| {})?;
    // macOS: the one program in Contents/MacOS; Linux: the AppDir's AppRun.
    let path = if cfg!(target_os = "macos") {
        let programs = dir.join(APP).join("Contents/MacOS");
        std::fs::read_dir(&programs)
            .map_err(|e| format!("{} is missing: {e}", programs.display()))?
            .flatten()
            .map(|entry| entry.path())
            .next()
            .ok_or_else(|| format!("{} is empty.", programs.display()))?
    } else {
        dir.join(APP).join("AppRun")
    };
    let meta =
        std::fs::metadata(&path).map_err(|e| format!("{} is missing: {e}", path.display()))?;
    if meta.permissions().mode() & 0o111 == 0 {
        return Err(format!("{} is not executable.", path.display()));
    }
    Ok(format!("ok  {} ({} bytes)", path.display(), meta.len()))
}

/// Puts `incoming` where `target` is, and what was there before away. The old
/// version is moved aside first, so a failure leaves one of the two in place.
fn swap_in(incoming: &Path, target: &Path) -> Result<(), String> {
    let aside = target.with_extension(format!("old-{}", std::process::id()));
    let had_old = std::fs::symlink_metadata(target).is_ok();
    if had_old {
        std::fs::rename(target, &aside)
            .map_err(|e| format!("Couldn't move the old {} aside: {e}", target.display()))?;
    }
    if let Err(e) = std::fs::rename(incoming, target) {
        if had_old {
            let _ = std::fs::rename(&aside, target);
        }
        return Err(format!("Couldn't put {} in place: {e}", target.display()));
    }
    if had_old {
        remove_tree(&aside);
    }
    Ok(())
}

/// Removes a folder UwURDP owns. A symlink in its place is only unlinked,
/// never followed.
fn remove_tree(path: &Path) {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            let _ = std::fs::remove_dir_all(path);
        }
        Ok(_) => {
            let _ = std::fs::remove_file(path);
        }
        Err(_) => {}
    }
}

pub fn install(
    layout: &Layout,
    options: &Options,
    version: &str,
    progress: Progress,
) -> Result<(), String> {
    let dir = PathBuf::from(options.dir.trim());
    if !dir.is_absolute() {
        return Err("Please pick a full folder path.".into());
    }
    progress(Step::Prepare, 0.0);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
    stop_app(layout, &dir)?;
    // Unpacked next to where it goes, so the final move is a rename on the
    // same disk, never a copy.
    let staging = dir.join(format!(".uwurdp-setup-{}", std::process::id()));
    remove_tree(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| format!("Couldn't create {}: {e}", staging.display()))?;
    progress(Step::Prepare, 1.0);

    let result =
        unpack(&staging, progress).and_then(|()| swap_in(&staging.join(APP), &dir.join(APP)));
    remove_tree(&staging);
    result?;

    progress(Step::Shortcuts, 0.0);
    shortcuts(layout, &dir, options)?;
    progress(Step::Shortcuts, 1.0);

    progress(Step::Register, 0.0);
    let record = Record {
        dir: dir.display().to_string(),
        version: version.to_string(),
        desktop_shortcut: options.desktop_shortcut,
    };
    if let Some(parent) = layout.state.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Couldn't create {}: {e}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?;
    std::fs::write(&layout.state, json)
        .map_err(|e| format!("Couldn't write {}: {e}", layout.state.display()))?;
    progress(Step::Register, 1.0);
    progress(Step::Done, 1.0);
    Ok(())
}

/// macOS: tell Launch Services the app is there, so Spotlight and Launchpad
/// find it at once.
#[cfg(target_os = "macos")]
fn shortcuts(layout: &Layout, dir: &Path, _options: &Options) -> Result<(), String> {
    if layout.sandbox {
        return Ok(());
    }
    const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
    let app = dir.join(APP);
    if app.exists() {
        system::best_effort(LSREGISTER, &["-f", &app.to_string_lossy()]);
    }
    Ok(())
}

/// Linux: menu entries, and a desktop icon if wanted.
#[cfg(not(target_os = "macos"))]
fn shortcuts(layout: &Layout, dir: &Path, options: &Options) -> Result<(), String> {
    let app_dir = dir.join(APP);
    let desktop_file = format!("{APP_ID}.desktop");
    let menu_entry = layout.applications.join(&desktop_file);
    let on_desktop = layout.desktop.join(&desktop_file);
    if !app_dir.exists() {
        let _ = std::fs::remove_file(&menu_entry);
        let _ = std::fs::remove_file(&on_desktop);
        return Ok(());
    }
    let entry = desktop_entry(
        &app_dir,
        "UwURDP",
        "Remote desktop client with self-hosted encrypted sync",
        "uwurdp-desktop",
        "Network;RemoteAccess;",
    );
    write_entry(&menu_entry, &entry)?;
    if options.desktop_shortcut {
        if write_entry(&on_desktop, &entry).is_ok() && !layout.sandbox {
            // GNOME only starts a desktop file someone trusted.
            system::best_effort(
                "gio",
                &[
                    "set",
                    &on_desktop.to_string_lossy(),
                    "metadata::trusted",
                    "true",
                ],
            );
        }
    } else {
        let _ = std::fs::remove_file(&on_desktop);
    }
    if !layout.sandbox {
        system::best_effort(
            "update-desktop-database",
            &[&layout.applications.to_string_lossy()],
        );
    }
    Ok(())
}

/// A freedesktop entry for an AppDir. The icon is the AppDir's own PNG.
#[cfg(not(target_os = "macos"))]
fn desktop_entry(
    app_dir: &Path,
    name: &str,
    comment: &str,
    class: &str,
    categories: &str,
) -> String {
    let icon = std::fs::read_dir(app_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "png"))
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let exec = app_dir.join("AppRun");
    format!(
        "[Desktop Entry]\nType=Application\nName={name}\nComment={comment}\nExec={} %U\nIcon={icon}\nTerminal=false\nCategories={categories}\nStartupWMClass={class}\n",
        quote_exec(&exec.to_string_lossy())
    )
}

/// A path as the `Exec` key wants it: quoted, with the characters the spec
/// reserves escaped.
#[cfg(not(target_os = "macos"))]
fn quote_exec(path: &str) -> String {
    let mut out = String::from("\"");
    for c in path.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        if c == '%' {
            out.push('%');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(not(target_os = "macos"))]
fn write_entry(path: &Path, text: &str) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Couldn't create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("Couldn't write {}: {e}", path.display()))?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    Ok(())
}

pub fn uninstall(
    layout: &Layout,
    dir: &Path,
    keep_data: bool,
    progress: Progress,
) -> Result<(), String> {
    progress(Step::Prepare, 0.0);
    stop_app(layout, dir)?;
    progress(Step::Prepare, 1.0);

    progress(Step::Shortcuts, 0.0);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = std::fs::remove_file(layout.applications.join(format!("{APP_ID}.desktop")));
        let _ = std::fs::remove_file(layout.desktop.join(format!("{APP_ID}.desktop")));
    }
    progress(Step::Shortcuts, 1.0);

    progress(Step::Register, 0.0);
    let _ = std::fs::remove_file(&layout.state);
    if let Some(parent) = layout.state.parent() {
        let _ = std::fs::remove_dir(parent);
    }
    progress(Step::Register, 1.0);

    progress(Step::Copy, 0.0);
    remove_tree(&dir.join(APP));
    if dir.join(APP).exists() {
        return Err(format!(
            "Couldn't remove {}. Is UwURDP still open?",
            dir.join(APP).display()
        ));
    }
    // Only the folder the setup made for itself, and only when empty — never
    // /Applications or a folder the person chose that holds other things.
    if !cfg!(target_os = "macos") {
        let _ = std::fs::remove_dir(dir);
    }
    progress(Step::Copy, 1.0);

    if !keep_data {
        progress(Step::Cleanup, 0.0);
        for folder in &layout.data {
            if std::fs::symlink_metadata(folder).is_ok() {
                remove_tree(folder);
                if folder.exists() {
                    return Err(format!("Couldn't delete {}.", folder.display()));
                }
            }
        }
        if !layout.sandbox {
            forget_seal_key();
        }
        progress(Step::Cleanup, 1.0);
    }
    progress(Step::Done, 1.0);
    Ok(())
}

/// The key UwURDP sealed its remembered vault and its pairing with lives in
/// the Keychain or the Secret Service, outside the data folders.
fn forget_seal_key() {
    #[cfg(target_os = "macos")]
    system::best_effort(
        "/usr/bin/security",
        &[
            "delete-generic-password",
            "-s",
            APP_ID,
            "-a",
            "device-seal-key",
        ],
    );
    #[cfg(not(target_os = "macos"))]
    system::best_effort(
        "secret-tool",
        &["clear", "service", APP_ID, "username", "device-seal-key"],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sandbox {
        _dir: tempfile::TempDir,
        layout: Layout,
    }

    impl Sandbox {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let layout = Layout::sandbox(dir.path());
            Self { _dir: dir, layout }
        }
    }

    #[test]
    fn remembers_defaults_until_something_is_installed() {
        let sandbox = Sandbox::new();
        assert!(sandbox.layout.installed().is_none());
        let options = sandbox.layout.remembered_options();
        assert!(options.dir.ends_with("Programs"));
    }

    #[test]
    fn swapping_keeps_one_version_in_place() {
        let sandbox = Sandbox::new();
        let dir = sandbox.layout.default_dir.clone();
        let target = dir.join(APP);
        let incoming = dir.join("incoming");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("old"), b"old").unwrap();
        std::fs::create_dir_all(&incoming).unwrap();
        std::fs::write(incoming.join("new"), b"new").unwrap();
        swap_in(&incoming, &target).unwrap();
        assert!(target.join("new").exists());
        assert!(!target.join("old").exists());
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "nothing left aside"
        );
    }

    #[test]
    fn registers_and_unregisters_everything() {
        let sandbox = Sandbox::new();
        let layout = &sandbox.layout;
        let dir = layout.default_dir.clone();
        std::fs::create_dir_all(dir.join(APP)).unwrap();
        std::fs::create_dir_all(&layout.data[0]).unwrap();
        std::fs::write(layout.data[0].join("uwurdp.db"), b"hosts").unwrap();
        let options = Options {
            dir: dir.display().to_string(),
            desktop_shortcut: true,
        };
        shortcuts(layout, &dir, &options).unwrap();
        std::fs::create_dir_all(layout.state.parent().unwrap()).unwrap();
        std::fs::write(
            &layout.state,
            serde_json::to_vec(&Record {
                dir: options.dir.clone(),
                version: "0.1.0".into(),
                desktop_shortcut: true,
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            layout.installed().unwrap().version.as_deref(),
            Some("0.1.0")
        );

        uninstall(layout, &dir, true, &mut |_, _| {}).unwrap();
        assert!(!dir.join(APP).exists());
        assert!(layout.installed().is_none());
        assert!(
            layout.data[0].join("uwurdp.db").exists(),
            "hosts and vault are kept"
        );

        std::fs::create_dir_all(dir.join(APP)).unwrap();
        uninstall(layout, &dir, false, &mut |_, _| {}).unwrap();
        assert!(
            !layout.data[0].exists(),
            "hosts and vault are deleted on request"
        );
    }

    #[test]
    fn a_folder_the_person_picked_gets_a_folder_of_ours() {
        let picked = PathBuf::from("/opt/tools");
        if cfg!(target_os = "macos") {
            assert_eq!(folder_for(picked.clone()), picked);
        } else {
            assert_eq!(folder_for(picked), PathBuf::from("/opt/tools/uwurdp"));
            assert_eq!(
                folder_for(PathBuf::from("/opt/uwurdp")),
                PathBuf::from("/opt/uwurdp")
            );
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn exec_paths_are_quoted_for_the_desktop_file() {
        assert_eq!(
            quote_exec("/home/nyu/a b/AppRun"),
            "\"/home/nyu/a b/AppRun\""
        );
        assert_eq!(quote_exec("/x/$y/100%"), "\"/x/\\$y/100%%\"");
    }
}
