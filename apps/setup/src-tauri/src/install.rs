//! Installing, updating and removing UwURDP for the current Windows user.
//!
//! Everything lives under the user's profile: the app in
//! `%LOCALAPPDATA%\Programs\UwURDP`, shortcuts in the Start menu (and on the
//! desktop if wanted), and registry entries under `HKEY_CURRENT_USER`. No
//! administrator rights are needed.
//!
//! `UWURDP_SETUP_SANDBOX=<folder>` redirects all of it (files, shortcuts,
//! registry under `HKCU\Software\UwURDP-Setup-Sandbox`) for testing.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
use winreg::RegKey;

use crate::system;

pub const APP_EXE: &str = "UwURDP.exe";
pub const UNINSTALL_EXE: &str = "uninstall.exe";
pub const APP_ID: &str = "app.uwurdp.desktop";
const SHORTCUT: &str = "UwURDP.lnk";
const HOMEPAGE: &str = "https://github.com/MinifyX/UwURDP-Client";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\UwURDP";
const SETUP_KEY: &str = r"Software\UwURDP\Setup";
/// What Tauri's standard NSIS installer (0.0.1) called the app and where it
/// kept its own registry entry.
const LEGACY_EXE: &str = "uwurdp-desktop.exe";
const LEGACY_PRODUCT_KEY: &str = r"Software\uwurdp\UwURDP";

static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.zst"));
const PAYLOAD_SIZE: &str = env!("UWURDP_SETUP_PAYLOAD_SIZE");

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
    /// Installed by the old standard installer.
    pub legacy: bool,
}

/// Where things go on this machine.
pub struct Layout {
    pub default_dir: PathBuf,
    pub start_menu: PathBuf,
    pub desktop: PathBuf,
    pub roaming_data: PathBuf,
    pub local_data: PathBuf,
    pub legacy_dir: PathBuf,
    registry_prefix: String,
    pub sandbox: bool,
}

impl Layout {
    pub fn detect() -> Self {
        match std::env::var_os("UWURDP_SETUP_SANDBOX").filter(|dir| !dir.is_empty()) {
            Some(dir) => Self::sandbox(Path::new(&dir)),
            None => {
                let folders = system::folders();
                Self {
                    default_dir: folders.user_programs.join("UwURDP"),
                    start_menu: folders.start_menu,
                    desktop: folders.desktop,
                    roaming_data: folders.roaming.join(APP_ID),
                    local_data: folders.local.join(APP_ID),
                    legacy_dir: folders.local.join("UwURDP"),
                    registry_prefix: String::new(),
                    sandbox: false,
                }
            }
        }
    }

    pub fn sandbox(root: &Path) -> Self {
        Self {
            default_dir: root.join(r"Programs\UwURDP"),
            start_menu: root.join("StartMenu"),
            desktop: root.join("Desktop"),
            roaming_data: root.join("Roaming").join(APP_ID),
            local_data: root.join("Local").join(APP_ID),
            legacy_dir: root.join(r"Local\UwURDP"),
            registry_prefix: format!(
                r"Software\UwURDP-Setup-Sandbox\{}\",
                root.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            sandbox: true,
        }
    }

    fn key(&self, path: &str) -> String {
        format!("{}{path}", self.registry_prefix)
    }

    fn open(&self, path: &str) -> Option<RegKey> {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(self.key(path), KEY_READ)
            .ok()
    }

    fn create(&self, path: &str) -> Result<RegKey, String> {
        RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(self.key(path))
            .map(|(key, _)| key)
            .map_err(|e| format!("Couldn't write to the registry ({path}): {e}"))
    }

    fn remove_tree(&self, path: &str) {
        let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(self.key(path));
    }

    fn remove_empty(&self, path: &str) {
        let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey(self.key(path));
    }

    /// What is installed right now, if anything.
    pub fn installed(&self) -> Option<Installed> {
        if let Some(key) = self.open(SETUP_KEY) {
            if let Ok(dir) = key.get_value::<String, _>("InstallDir") {
                if Path::new(&dir).join(APP_EXE).exists() {
                    return Some(Installed {
                        dir,
                        version: key.get_value("Version").ok(),
                        legacy: false,
                    });
                }
            }
        }
        let legacy = self.open(UNINSTALL_KEY);
        let dir = legacy
            .as_ref()
            .and_then(|key| key.get_value::<String, _>("InstallLocation").ok())
            .map(|dir| dir.trim_matches('"').to_string())
            .filter(|dir| Path::new(dir).join(LEGACY_EXE).exists())
            .or_else(|| {
                self.legacy_dir
                    .join(LEGACY_EXE)
                    .exists()
                    .then(|| self.legacy_dir.display().to_string())
            })?;
        let version = legacy.and_then(|key| key.get_value("DisplayVersion").ok());
        Some(Installed {
            dir,
            version,
            legacy: true,
        })
    }

    /// Options from the last install, or the defaults.
    pub fn remembered_options(&self) -> Options {
        let key = self.open(SETUP_KEY);
        let flag = |name: &str, default: bool| {
            key.as_ref()
                .and_then(|k| k.get_value::<u32, _>(name).ok())
                .map_or(default, |v| v != 0)
        };
        let dir = self
            .installed()
            .filter(|installed| !installed.legacy)
            .map_or_else(
                || self.default_dir.display().to_string(),
                |installed| installed.dir,
            );
        Options {
            dir,
            desktop_shortcut: flag("DesktopShortcut", true),
        }
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

/// Writes a file next to its destination first, then swaps it in. A running
/// program keeps its file locked for a moment after it ends, hence the retries.
fn replace_file(from: &Path, to: &Path) -> Result<(), String> {
    let mut attempt = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(_) if attempt < 19 => std::thread::sleep(Duration::from_millis(250)),
            Err(e) => return Err(format!("Couldn't replace {}: {e}", to.display())),
        }
        attempt += 1;
    }
}

/// Unpack the packed program to `target`, reporting progress through the copy
/// step.
fn extract(payload: &[u8], size: &str, target: &Path, progress: Progress) -> Result<(), String> {
    if payload.is_empty() {
        return Err("This setup was built without UwURDP inside (a development build).".into());
    }
    let total: u64 = size.parse().unwrap_or(1).max(1);
    let mut decoder =
        zstd::Decoder::new(payload).map_err(|e| format!("The packed app is damaged: {e}"))?;
    let mut file = std::fs::File::create(target)
        .map_err(|e| format!("Couldn't write {}: {e}", target.display()))?;
    let mut buffer = vec![0u8; 256 * 1024];
    let mut written = 0u64;
    loop {
        let read = decoder
            .read(&mut buffer)
            .map_err(|e| format!("The packed app is damaged: {e}"))?;
        if read == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buffer[..read])
            .map_err(|e| format!("Couldn't write {}: {e}", target.display()))?;
        written += read as u64;
        progress(Step::Copy, written as f64 / total as f64);
    }
    file.sync_all()
        .map_err(|e| format!("Couldn't write {}: {e}", target.display()))?;
    if written != total {
        return Err("The packed app is incomplete.".into());
    }
    Ok(())
}

fn quoted(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

fn dir_size_kb(dir: &Path) -> u32 {
    let bytes: u64 = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
        .sum();
    (bytes / 1024).min(u32::MAX as u64) as u32
}

/// Stops UwURDP if it runs from `dir`. Skipped in the sandbox, which must never
/// touch real processes.
pub fn stop_app(layout: &Layout, dir: &Path) -> Result<(), String> {
    if layout.sandbox {
        return Ok(());
    }
    system::stop_processes(&dir.join(APP_EXE))?;
    system::stop_processes(&dir.join(LEGACY_EXE))
}

/// UwURDP is open. Closing it ends every open remote session, so the setup
/// asks first.
pub fn app_running(layout: &Layout, dir: &Path) -> bool {
    !layout.sandbox
        && [APP_EXE, LEGACY_EXE]
            .iter()
            .any(|exe| !system::processes_of(&dir.join(exe)).is_empty())
}

/// Removes the files and the registry entry of the old standard installer.
/// Hosts, vault and settings stay: they live in the app data folders.
fn remove_legacy(layout: &Layout, new_dir: &Path) -> Result<(), String> {
    let Some(installed) = layout.installed().filter(|installed| installed.legacy) else {
        return Ok(());
    };
    let old = PathBuf::from(&installed.dir);
    stop_app(layout, &old)?;
    if old != new_dir {
        for file in [LEGACY_EXE, UNINSTALL_EXE] {
            let _ = std::fs::remove_file(old.join(file));
        }
        let _ = std::fs::remove_dir(&old);
    }
    layout.remove_tree(LEGACY_PRODUCT_KEY);
    layout.remove_empty(r"Software\uwurdp");
    Ok(())
}

/// The folder the setup installs into for a folder the person picked: a
/// "UwURDP" folder inside it, unless it is one already.
pub fn folder_for(mut picked: PathBuf) -> PathBuf {
    if !picked
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("UwURDP"))
    {
        picked.push("UwURDP");
    }
    picked
}

/// Starts the installed UwURDP. Nothing in the sandbox.
pub fn launch(layout: &Layout, dir: &Path) -> Result<(), String> {
    if layout.sandbox {
        return Ok(());
    }
    system::spawn_detached(&dir.join(APP_EXE), &[])
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
    remove_legacy(layout, &dir)?;
    progress(Step::Prepare, 1.0);

    let app = dir.join(APP_EXE);
    let incoming = dir.join("UwURDP.exe.new");
    extract(PAYLOAD, PAYLOAD_SIZE, &incoming, progress)?;
    replace_file(&incoming, &app)?;

    let uninstaller = dir.join(UNINSTALL_EXE);
    let me =
        std::env::current_exe().map_err(|e| format!("Couldn't find the setup program: {e}"))?;
    if !me.eq(&uninstaller) {
        let copy = dir.join("uninstall.exe.new");
        std::fs::copy(&me, &copy).map_err(|e| format!("Couldn't write {}: {e}", copy.display()))?;
        replace_file(&copy, &uninstaller)?;
    }

    progress(Step::Shortcuts, 0.0);
    let shortcut = system::Shortcut {
        target: &app,
        arguments: "",
        description: "UwURDP",
        app_id: APP_ID,
    };
    system::create_shortcut(&layout.start_menu.join(SHORTCUT), &shortcut)?;
    let on_desktop = layout.desktop.join(SHORTCUT);
    if options.desktop_shortcut {
        system::create_shortcut(&on_desktop, &shortcut)?;
    } else {
        let _ = std::fs::remove_file(&on_desktop);
    }
    progress(Step::Shortcuts, 1.0);

    progress(Step::Register, 0.0);
    register(layout, &dir, options, version)?;
    progress(Step::Register, 1.0);
    progress(Step::Done, 1.0);
    Ok(())
}

fn register(layout: &Layout, dir: &Path, options: &Options, version: &str) -> Result<(), String> {
    let app = dir.join(APP_EXE);
    let uninstaller = dir.join(UNINSTALL_EXE);
    let write = |key: &RegKey, name: &str, value: &str| {
        key.set_value(name, &value)
            .map_err(|e| format!("Couldn't write to the registry ({name}): {e}"))
    };
    let write_dword = |key: &RegKey, name: &str, value: u32| {
        key.set_value(name, &value)
            .map_err(|e| format!("Couldn't write to the registry ({name}): {e}"))
    };

    // The old installer wrote values this one doesn't; start from a clean entry.
    layout.remove_tree(UNINSTALL_KEY);
    let entry = layout.create(UNINSTALL_KEY)?;
    write(&entry, "DisplayName", "UwURDP")?;
    write(&entry, "DisplayVersion", version)?;
    write(&entry, "Publisher", "UwURDP")?;
    write(&entry, "DisplayIcon", &format!("{},0", app.display()))?;
    write(&entry, "InstallLocation", &dir.display().to_string())?;
    write(
        &entry,
        "UninstallString",
        &format!("{} --uninstall", quoted(&uninstaller)),
    )?;
    write(&entry, "URLInfoAbout", HOMEPAGE)?;
    write(&entry, "HelpLink", HOMEPAGE)?;
    write_dword(&entry, "EstimatedSize", dir_size_kb(dir))?;
    write_dword(&entry, "NoModify", 1)?;
    write_dword(&entry, "NoRepair", 1)?;

    let setup = layout.create(SETUP_KEY)?;
    write(&setup, "InstallDir", &dir.display().to_string())?;
    write(&setup, "Version", version)?;
    write_dword(&setup, "DesktopShortcut", options.desktop_shortcut.into())?;
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
    let _ = std::fs::remove_file(layout.start_menu.join(SHORTCUT));
    let _ = std::fs::remove_file(layout.desktop.join(SHORTCUT));
    progress(Step::Shortcuts, 1.0);

    progress(Step::Register, 0.0);
    layout.remove_tree(UNINSTALL_KEY);
    layout.remove_tree(r"Software\UwURDP");
    layout.remove_tree(LEGACY_PRODUCT_KEY);
    layout.remove_empty(r"Software\uwurdp");
    progress(Step::Register, 1.0);

    progress(Step::Copy, 0.0);
    for file in [
        APP_EXE,
        UNINSTALL_EXE,
        LEGACY_EXE,
        "UwURDP.exe.new",
        "uninstall.exe.new",
    ] {
        let path = dir.join(file);
        if path.exists() && std::fs::remove_file(&path).is_err() && file == APP_EXE {
            return Err(format!(
                "Couldn't remove {}. Is UwURDP still open?",
                path.display()
            ));
        }
    }
    let _ = std::fs::remove_dir(dir);
    progress(Step::Copy, 1.0);

    if !keep_data {
        progress(Step::Cleanup, 0.0);
        for folder in [&layout.roaming_data, &layout.local_data] {
            if folder.exists() {
                std::fs::remove_dir_all(folder)
                    .map_err(|e| format!("Couldn't delete {}: {e}", folder.display()))?;
            }
        }
        progress(Step::Cleanup, 1.0);
    }
    progress(Step::Done, 1.0);
    Ok(())
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

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let key = self
                .layout
                .registry_prefix
                .trim_end_matches('\\')
                .to_string();
            let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(key);
        }
    }

    #[test]
    fn remembers_defaults_until_something_is_installed() {
        let sandbox = Sandbox::new();
        assert!(sandbox.layout.installed().is_none());
        let options = sandbox.layout.remembered_options();
        assert!(options.dir.ends_with(r"Programs\UwURDP"));
        assert!(options.desktop_shortcut);
    }

    #[test]
    fn recognizes_the_old_standard_installer() {
        let sandbox = Sandbox::new();
        let layout = &sandbox.layout;
        std::fs::create_dir_all(&layout.legacy_dir).unwrap();
        std::fs::write(layout.legacy_dir.join(LEGACY_EXE), b"old").unwrap();
        std::fs::write(layout.legacy_dir.join(UNINSTALL_EXE), b"nsis").unwrap();
        layout
            .create(LEGACY_PRODUCT_KEY)
            .unwrap()
            .set_value("", &layout.legacy_dir.display().to_string())
            .unwrap();

        let installed = layout.installed().unwrap();
        assert!(installed.legacy);
        remove_legacy(layout, &layout.default_dir).unwrap();
        assert!(!layout.legacy_dir.exists());
        assert!(layout.open(LEGACY_PRODUCT_KEY).is_none());
        assert!(layout.installed().is_none());
    }

    #[test]
    fn registers_and_unregisters_everything() {
        let sandbox = Sandbox::new();
        let layout = &sandbox.layout;
        let dir = layout.default_dir.clone();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(APP_EXE), b"app").unwrap();
        std::fs::write(dir.join(UNINSTALL_EXE), b"setup").unwrap();
        let options = Options {
            dir: dir.display().to_string(),
            desktop_shortcut: false,
        };
        register(layout, &dir, &options, "0.1.0").unwrap();

        let installed = layout.installed().unwrap();
        assert_eq!(
            (installed.version.as_deref(), installed.legacy),
            (Some("0.1.0"), false)
        );
        assert!(!layout.remembered_options().desktop_shortcut);
        let command: String = layout
            .open(UNINSTALL_KEY)
            .unwrap()
            .get_value("UninstallString")
            .unwrap();
        assert!(command.ends_with("\" --uninstall"));

        std::fs::create_dir_all(&layout.roaming_data).unwrap();
        std::fs::write(layout.roaming_data.join("uwurdp.db"), b"hosts").unwrap();
        uninstall(layout, &dir, true, &mut |_, _| {}).unwrap();
        assert!(!dir.exists());
        assert!(
            layout.roaming_data.join("uwurdp.db").exists(),
            "hosts and vault are kept"
        );
        assert!(layout.open(UNINSTALL_KEY).is_none());
        assert!(layout.open(SETUP_KEY).is_none());

        std::fs::create_dir_all(&dir).unwrap();
        uninstall(layout, &dir, false, &mut |_, _| {}).unwrap();
        assert!(
            !layout.roaming_data.exists(),
            "hosts and vault are deleted on request"
        );
    }
}
