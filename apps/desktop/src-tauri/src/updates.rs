//! Automatic updates, the same way UwUMail does them.
//!
//! UwURDP looks for a new version shortly after starting and every six hours,
//! downloads the signed setup (`UwURDP-Setup-<version>.exe` on Windows) quietly
//! and tells the page. "Restart now" hands over to that setup in `--update`
//! mode; otherwise the update is applied the next time UwURDP starts.
//!
//! A copy dpkg or rpm installed from the release's `.deb` / `.rpm` gets the
//! next package instead and installs it on "Restart now" with `pkexec dpkg -i`
//! or `pkexec rpm -U` — never on its own at start, since that asks for the
//! administrator password. Only when the package manager really owns the
//! running program, though: an Arch package repacked from the `.deb` updates
//! through pacman, the portable folder not at all.
//!
//! Two channels, both feeds on the `updates` branch of the public repository:
//! `stable.json` carries plain versions only, `beta.json` carries betas too.
//! Only the setup is signed, not the feed, so the signature is checked when the
//! update is downloaded and again right before the setup runs, and the setup
//! itself refuses to install anything older than what is there.

use std::path::{Path, PathBuf};
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

/// The `updates` branch of the public repo, written by `pnpm release`.
const FEED: &str = "https://raw.githubusercontent.com/MinifyX/UwURDP-Client/updates";
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(20);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const PENDING: &str = "pending.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Beta,
}

/// Until the page says otherwise: a beta build stays on Beta, everything else on Stable.
impl Default for Channel {
    fn default() -> Self {
        if env!("CARGO_PKG_VERSION").contains('-') {
            Channel::Beta
        } else {
            Channel::Stable
        }
    }
}

/// A downloaded update waiting to be installed, as `pending.json` keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadyUpdate {
    version: String,
    /// Release notes; JSON with `de` and `en` when written for UwURDP.
    notes: Option<String>,
    /// The downloaded setup.
    file: PathBuf,
    /// The release signature, checked again right before the setup runs.
    signature: String,
}

/// What the page learns about a waiting update. No file paths.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub notes: Option<String>,
}

impl From<&ReadyUpdate> for UpdateInfo {
    fn from(update: &ReadyUpdate) -> Self {
        Self {
            version: update.version.clone(),
            notes: update.notes.clone(),
        }
    }
}

#[derive(Default)]
pub struct Updates {
    channel: Mutex<Channel>,
    ready: Mutex<Option<ReadyUpdate>>,
    checking: tokio::sync::Mutex<()>,
}

fn updates_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_local_data_dir()
        .ok()
        .map(|dir| dir.join("updates"))
}

fn is_newer(app: &AppHandle, version: &str) -> bool {
    semver::Version::parse(version).is_ok_and(|v| v > app.package_info().version)
}

fn setup_file(dir: &Path, kind: Install, version: &str) -> PathBuf {
    dir.join(setup_name(kind, version))
}

/// Where the setup keeps what it installed (macOS and Linux; on Windows it is
/// the registry). The same path as in `apps/setup/src-tauri/src/install_unix.rs`.
#[cfg(not(windows))]
fn setup_record() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let config = if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .unwrap_or_else(|| home.join(".config"))
    };
    Some(config.join("app.uwurdp.setup").join("install.json"))
}

/// Whether this copy of UwURDP is one the setup put there, and so one the
/// setup may replace. On macOS and Linux an app from somewhere else — a
/// `.deb`, a copy out of a disk image — updates the way it came.
fn installed_by_setup() -> bool {
    #[cfg(windows)]
    {
        true
    }
    #[cfg(not(windows))]
    {
        #[derive(Deserialize)]
        struct Record {
            dir: PathBuf,
        }
        let (Some(path), Ok(me)) = (setup_record(), std::env::current_exe()) else {
            return false;
        };
        std::fs::read(path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<Record>(&raw).ok())
            .is_some_and(|record| record.dir.is_absolute() && me.starts_with(&record.dir))
    }
}

/// The Linux package name of the `.deb` and `.rpm` (`scripts/build-setup.mjs`).
#[cfg(target_os = "linux")]
const PACKAGE: &str = "uwurdp";

/// How this copy of UwURDP was installed, as far as updating it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum Install {
    /// By UwURDP's own setup, which the updater runs again.
    Setup,
    /// By dpkg, from the release's `.deb`.
    Deb,
    /// By rpm, from the release's `.rpm`.
    Rpm,
}

/// How this copy updates itself, or `None` when it doesn't: a copy out of a
/// disk image, the portable folder, a package some other tool put together.
/// Asked once; the answer doesn't change while UwURDP runs.
fn install_kind() -> Option<Install> {
    static KIND: std::sync::OnceLock<Option<Install>> = std::sync::OnceLock::new();
    *KIND.get_or_init(|| {
        if installed_by_setup() {
            return Some(Install::Setup);
        }
        #[cfg(target_os = "linux")]
        {
            use tauri::utils::{config::BundleType, platform::bundle_type};
            let me = std::env::current_exe().ok()?;
            // The bundler marks the program with the package it went into.
            // The AUR package repacks the `.deb`, so the mark alone isn't
            // enough: the package manager has to know the file as its own.
            match bundle_type() {
                Some(BundleType::Deb) if dpkg_owns(&me) => return Some(Install::Deb),
                Some(BundleType::Rpm) if rpm_owns(&me) => return Some(Install::Rpm),
                _ => {}
            }
        }
        None
    })
}

/// Whether dpkg installed `exe` as part of UwURDP's package.
#[cfg(target_os = "linux")]
fn dpkg_owns(exe: &Path) -> bool {
    let list = format!("/var/lib/dpkg/info/{PACKAGE}.list");
    std::fs::read_to_string(list)
        .is_ok_and(|files| files.lines().any(|line| Path::new(line.trim()) == exe))
}

/// Whether rpm installed `exe` as part of UwURDP's package.
#[cfg(target_os = "linux")]
fn rpm_owns(exe: &Path) -> bool {
    std::process::Command::new("rpm")
        .args(["-qf", "--queryformat", "%{NAME}"])
        .arg(exe)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .is_ok_and(|out| out.status.success() && out.stdout == PACKAGE.as_bytes())
}

/// The feed's platform key for this build, as `pnpm release` writes it:
/// `windows-x86_64`, `darwin-aarch64`, `linux-x86_64` for the setup, with
/// `-deb` / `-rpm` added for a copy the package manager installed. Always
/// named exactly, so a package never falls back to the setup's entry.
fn feed_target(kind: Install) -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let base = format!("{os}-{}", std::env::consts::ARCH);
    match kind {
        Install::Setup => base,
        Install::Deb => format!("{base}-deb"),
        Install::Rpm => format!("{base}-rpm"),
    }
}

/// A waiting update, opened and checked, with the file held so that nobody can
/// change or replace it until the handle is dropped.
struct Pending {
    update: ReadyUpdate,
    /// The bytes the signature was checked on (a package is installed from these).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    bytes: Vec<u8>,
    _locked: std::fs::File,
}

/// A waiting update, if it is where UwURDP put it and still carries a valid
/// release signature for exactly that file name. `pending.json` lives in a
/// folder any program of the user can write to, so neither its path nor the
/// file is trusted blindly — and the file stays locked against writing and
/// deleting from the check until the setup has started.
fn open_pending(app: &AppHandle, kind: Install) -> Option<Pending> {
    let dir = updates_dir(app)?;
    let raw = std::fs::read(dir.join(PENDING)).ok()?;
    let update = serde_json::from_slice::<ReadyUpdate>(&raw).ok()?;
    semver::Version::parse(&update.version).ok()?;
    let expected = setup_file(&dir, kind, &update.version);
    if update.file != expected {
        return None;
    }
    let mut file = open_locked(&expected).ok()?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).ok()?;
    let name = setup_name(kind, &update.version);
    if !verify(app, &bytes, &update.signature, &name) {
        return None;
    }
    Some(Pending {
        update,
        bytes,
        _locked: file,
    })
}

/// Opens a file for reading while refusing everyone else write and delete
/// access. Starting it as a program still works: that only needs reading.
fn open_locked(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        options.share_mode(FILE_SHARE_READ);
    }
    options.open(path)
}

/// The name the signature of this platform's update has to carry. The release
/// publishes the files under names without a version
/// (`UwURDP-windows-x64-setup.exe`, `UwURDP-update-macos-universal`, …) but
/// signs each under this one, which binds the bytes to the version the feed
/// claims. These are the names installed versions look for: never change them,
/// only add. On macOS the update is the bare setup program from the disk
/// image, one universal build signed once under each name.
fn setup_name(kind: Install, version: &str) -> String {
    let package = |ext: &str| format!("UwURDP-{version}-linux-{}.{ext}", std::env::consts::ARCH);
    match kind {
        Install::Deb => return package("deb"),
        Install::Rpm => return package("rpm"),
        Install::Setup => {}
    }
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => format!("UwURDP-Setup-{version}-macos-arm64-update"),
        ("macos", _) => format!("UwURDP-Setup-{version}-macos-x64-update"),
        ("linux", "aarch64") => format!("UwURDP-Setup-{version}-linux-arm64.AppImage"),
        ("linux", _) => format!("UwURDP-Setup-{version}-linux-x64.AppImage"),
        (_, "aarch64") => format!("UwURDP-Setup-{version}-windows-arm64.exe"),
        _ => format!("UwURDP-Setup-{version}.exe"),
    }
}

fn pubkey(app: &AppHandle) -> Option<String> {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|updater| updater.get("pubkey"))
        .and_then(|key| key.as_str())
        .map(str::to_string)
}

/// Checks a setup against the release key from `tauri.conf.json`.
fn verify(app: &AppHandle, bytes: &[u8], signature: &str, file_name: &str) -> bool {
    pubkey(app).is_some_and(|key| verify_with(&key, bytes, signature, file_name))
}

/// Both the key and the signature come base64-wrapped, the way Tauri's signer
/// writes them. The feed is not signed, only the setup is; the signature's
/// trusted comment names the file that was signed, so checking it ties the
/// version the feed claims to the version that was actually signed.
fn verify_with(pubkey: &str, bytes: &[u8], signature: &str, file_name: &str) -> bool {
    use base64::Engine as _;
    let decode = |text: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(text.trim())
            .ok()
            .and_then(|raw| String::from_utf8(raw).ok())
    };
    let (Some(pubkey), Some(signature)) = (decode(pubkey), decode(signature)) else {
        return false;
    };
    let (Ok(key), Ok(signature)) = (
        minisign_verify::PublicKey::decode(&pubkey),
        minisign_verify::Signature::decode(&signature),
    ) else {
        return false;
    };
    key.verify(bytes, &signature, false).is_ok()
        && signs_file(signature.trusted_comment(), file_name)
}

/// Tauri's signer writes `timestamp:<secs>\tfile:<name>` as the trusted comment.
fn signs_file(trusted_comment: &str, file_name: &str) -> bool {
    trusted_comment
        .split('\t')
        .filter_map(|part| part.trim().strip_prefix("file:"))
        .any(|name| name == file_name)
}

/// Whether another UwURDP window is running from the same program file. Its
/// sessions would die with the update, so nothing hands over while it runs.
#[cfg(windows)]
fn other_instances_running() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let Ok(me) = std::env::current_exe() else {
        return false;
    };
    let wanted = me.to_string_lossy().to_lowercase();
    let own_pid = std::process::id();
    let path_of = |pid: u32| -> Option<String> {
        // SAFETY: plain Win32 calls with a buffer we own; the handle is closed.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return None;
            }
            let mut buffer = [0u16; 1024];
            let mut size = buffer.len() as u32;
            let ok = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                buffer.as_mut_ptr(),
                &mut size,
            );
            CloseHandle(process);
            (ok != 0).then(|| String::from_utf16_lossy(&buffer[..size as usize]).to_lowercase())
        }
    };

    // SAFETY: the snapshot handle is checked and closed; the entry's size is set.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more && !found {
            if entry.th32ProcessID != own_pid
                && path_of(entry.th32ProcessID).as_deref() == Some(&*wanted)
            {
                found = true;
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        found
    }
}

#[cfg(not(windows))]
fn other_instances_running() -> bool {
    false
}

/// Starts the checked setup to replace this UwURDP, which then quits. The file
/// stays locked until the setup process exists.
fn hand_over(pending: Pending, relaunch: bool) -> Result<(), String> {
    let update = &pending.update;
    // Each download gets one attempt. If the setup refuses (e.g. an older
    // version), the next start must not hand over again and again.
    if let Some(dir) = update.file.parent() {
        let _ = std::fs::remove_file(dir.join(PENDING));
    }
    let pid = std::process::id().to_string();
    let mut args = vec!["--update", "--wait-pid", pid.as_str()];
    if relaunch {
        args.push("--relaunch");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&update.file, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("Couldn't start the update: {e}"))?;
    }
    let mut command = std::process::Command::new(&update.file);
    command.args(&args);
    // The Linux setup is an AppImage. Unpacked and run, it needs no FUSE,
    // which many systems no longer have.
    #[cfg(target_os = "linux")]
    command.env("APPIMAGE_EXTRACT_AND_RUN", "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group, so it lives on when UwURDP quits.
        command.process_group(0);
    }
    let started = command
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Couldn't start the update: {e}"));
    drop(pending);
    started
}

/// Installs a checked `.deb` or `.rpm` through the package manager, asking for
/// the administrator password with pkexec, and starts the new version. Blocks
/// until the package manager is done.
#[cfg(target_os = "linux")]
fn install_package(pending: Pending, kind: Install) -> Result<(), String> {
    let failed = |e: std::io::Error| format!("Couldn't install the update: {e}");
    // The program's path before the package replaces it: afterwards the
    // kernel reports the old file as deleted.
    let me = std::env::current_exe().map_err(failed)?;
    let (ext, command): (&str, &[&str]) = match kind {
        Install::Deb => ("deb", &["dpkg", "-i"]),
        // rpm sorts `0.2.0-beta.1` after `0.2.0`; UwURDP has already checked
        // that the version is newer and that the package is signed for it.
        Install::Rpm => ("rpm", &["rpm", "-U", "--oldpackage"]),
        Install::Setup => return Err("Not a package.".into()),
    };
    // Each download gets one attempt, as with the setup.
    if let Some(dir) = pending.update.file.parent() {
        let _ = std::fs::remove_file(dir.join(PENDING));
    }
    // The bytes whose signature was checked go to a fresh folder only this
    // user can open, and root installs that copy, not the file in the
    // updates folder.
    let staging = tempfile::Builder::new()
        .prefix("uwurdp-update-")
        .tempdir()
        .map_err(failed)?;
    let file = staging.path().join(format!("{PACKAGE}.{ext}"));
    std::fs::write(&file, &pending.bytes).map_err(failed)?;
    drop(pending);
    let status = std::process::Command::new("pkexec")
        .args(command)
        .arg(&file)
        .stdin(std::process::Stdio::null())
        .status()
        .map_err(|e| {
            format!(
                "Couldn't ask for the administrator password (pkexec): {e}. \
                 Install the update with your package manager."
            )
        })?;
    if !status.success() {
        return Err(format!(
            "The package manager didn't install the update ({status}). \
             Install it with your package manager."
        ));
    }
    let mut again = std::process::Command::new(&me);
    {
        use std::os::unix::process::CommandExt;
        // Its own process group, so it lives on when this UwURDP quits.
        again.process_group(0);
    }
    again
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("The update is installed, but UwURDP couldn't start again: {e}"))
}

#[cfg(not(target_os = "linux"))]
fn install_package(_pending: Pending, _kind: Install) -> Result<(), String> {
    Err("Only Linux packages update this way.".into())
}

/// Called first thing on start: installs a waiting update, or cleans up after
/// one. Returns true when UwURDP must quit right away because the setup takes
/// over.
pub fn apply_pending_on_start(app: &AppHandle) -> bool {
    if cfg!(debug_assertions) {
        return false;
    }
    let Some(kind) = install_kind() else {
        return false;
    };
    match open_pending(app, kind) {
        Some(pending) if is_newer(app, &pending.update.version) => {
            // Another window still has sessions open; the update waits. So
            // does a package, for "Restart now": nobody wants a password
            // prompt just for starting UwURDP.
            kind == Install::Setup && !other_instances_running() && hand_over(pending, true).is_ok()
        }
        _ => {
            if let Some(dir) = updates_dir(app) {
                let _ = std::fs::remove_dir_all(dir);
            }
            false
        }
    }
}

pub fn set_channel(app: &AppHandle, channel: Channel) {
    let state = app.state::<Updates>();
    let mut current = state.channel.lock();
    if *current != channel {
        *current = channel;
        tracing::info!(?channel, "update channel");
    }
}

pub fn ready(app: &AppHandle) -> Option<UpdateInfo> {
    app.state::<Updates>()
        .ready
        .lock()
        .as_ref()
        .map(UpdateInfo::from)
}

/// Looks for a newer version and downloads it. Returns the waiting update, if any.
pub async fn check(app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    let state = app.state::<Updates>();
    let _one_at_a_time = state.checking.lock().await;
    if let Some(update) = ready(app) {
        return Ok(Some(update));
    }
    let Some(kind) = install_kind() else {
        return Ok(None);
    };
    let channel = *state.channel.lock();
    let feed = match channel {
        Channel::Stable => format!("{FEED}/stable.json"),
        Channel::Beta => format!("{FEED}/beta.json"),
    };
    let fail = |e: tauri_plugin_updater::Error| format!("Update check failed: {e}");
    let updater = app
        .updater_builder()
        .endpoints(vec![feed
            .parse()
            .map_err(|_| "Bad update address".to_string())?])
        .map_err(fail)?
        // The Linux setup brings its own WebKit and is large; a slow line
        // needs its time.
        .timeout(Duration::from_secs(15 * 60))
        .target(feed_target(kind))
        .build()
        .map_err(fail)?;
    let Some(found) = updater.check().await.map_err(fail)? else {
        return Ok(None);
    };
    if semver::Version::parse(&found.version).is_err() {
        return Err(format!(
            "The update feed names a bad version: {}",
            found.version
        ));
    }

    // The plugin checks the signature against the public key before handing
    // out the bytes; the file name in it is checked here, against the version
    // the feed claims.
    let bytes = found.download(|_, _| {}, || {}).await.map_err(fail)?;
    let name = setup_name(kind, &found.version);
    if !verify(app, &bytes, &found.signature, &name) {
        return Err(format!(
            "The update's signature doesn't belong to {name}, so it was not saved."
        ));
    }
    let dir = updates_dir(app).ok_or("No folder for updates")?;
    let save = |e: std::io::Error| format!("Couldn't save the update: {e}");
    std::fs::create_dir_all(&dir).map_err(save)?;
    let file = setup_file(&dir, kind, &found.version);
    std::fs::write(&file, &bytes).map_err(save)?;
    let update = ReadyUpdate {
        version: found.version.clone(),
        notes: found.body.clone(),
        file,
        signature: found.signature.clone(),
    };
    let json = serde_json::to_vec(&update).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(PENDING), json).map_err(save)?;

    let info = UpdateInfo::from(&update);
    *state.ready.lock() = Some(update);
    let _ = app.emit("update:ready", &info);
    Ok(Some(info))
}

/// "Restart now".
pub async fn install_now(app: &AppHandle) -> Result<(), String> {
    if cfg!(debug_assertions) {
        return Err("Development builds don't install updates.".into());
    }
    if ready(app).is_none() {
        return Err("There's no update waiting.".into());
    }
    let kind = install_kind().ok_or("This copy of UwURDP doesn't update itself.")?;
    if other_instances_running() {
        return Err(
            "Another UwURDP window is still open. Close it first, so its connections aren't cut off."
                .into(),
        );
    }
    // Read it back from disk and keep it locked, so the signature is checked
    // on the file that runs.
    let pending = open_pending(app, kind).ok_or("The downloaded update is damaged.")?;
    if kind == Install::Setup {
        hand_over(pending, true)?;
    } else {
        // The password prompt and the package manager take their time; the
        // window keeps drawing meanwhile.
        let installed =
            tauri::async_runtime::spawn_blocking(move || install_package(pending, kind))
                .await
                .map_err(|e| format!("Couldn't install the update: {e}"))?;
        // Tried once either way, like a setup: pending.json is gone.
        *app.state::<Updates>().ready.lock() = None;
        installed?;
    }
    app.exit(0);
    Ok(())
}

/// Checks in the background for as long as UwURDP runs.
pub fn start(app: &AppHandle) {
    app.manage(Updates::default());
    if let Some(pending) = install_kind()
        .and_then(|kind| open_pending(app, kind))
        .filter(|p| is_newer(app, &p.update.version))
    {
        *app.state::<Updates>().ready.lock() = Some(pending.update);
    }
    if cfg!(debug_assertions) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_AFTER).await;
        loop {
            if let Err(error) = check(&app).await {
                tracing::info!("{error}");
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_beta_build_starts_on_the_beta_channel() {
        let expected = if env!("CARGO_PKG_VERSION").contains('-') {
            Channel::Beta
        } else {
            Channel::Stable
        };
        assert_eq!(Channel::default(), expected);
        assert_eq!(serde_json::to_string(&Channel::Beta).unwrap(), "\"beta\"");
    }

    #[test]
    fn garbage_never_verifies() {
        assert!(!verify_with("", b"setup", "", "UwURDP-Setup-1.0.0.exe"));
        assert!(!verify_with(
            "bm90IGEga2V5",
            b"setup",
            "bm90IGEgc2lnbmF0dXJl",
            "UwURDP-Setup-1.0.0.exe"
        ));
    }

    #[test]
    fn a_setup_signed_with_another_key_is_refused() {
        // The shipped key, and a signature that is well-formed but belongs to
        // nothing this key signed.
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let pubkey = conf["plugins"]["updater"]["pubkey"].as_str().unwrap();
        assert!(!pubkey.is_empty());
        assert!(!verify_with(
            pubkey,
            b"a setup nobody signed",
            "AAAA",
            "UwURDP-Setup-1.0.0.exe"
        ));
    }

    #[test]
    fn the_signature_must_name_the_setup_the_feed_promised() {
        let comment = "timestamp:1789000000\tfile:UwURDP-Setup-0.1.0-beta.2.exe";
        assert!(signs_file(comment, "UwURDP-Setup-0.1.0-beta.2.exe"));
        // An older, validly signed setup offered as a newer version.
        assert!(!signs_file(comment, "UwURDP-Setup-0.2.0.exe"));
        assert!(!signs_file(
            "timestamp:1789000000",
            "UwURDP-Setup-0.2.0.exe"
        ));
    }

    #[test]
    fn only_the_file_uwurdp_names_counts_as_a_setup() {
        let dir = Path::new("updates");
        let file = setup_file(dir, Install::Setup, "0.1.0-beta.2");
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("UwURDP-Setup-0.1.0-beta.2"), "{name}");
        #[cfg(all(windows, target_arch = "x86_64"))]
        assert_eq!(name, "UwURDP-Setup-0.1.0-beta.2.exe");
        #[cfg(all(windows, target_arch = "aarch64"))]
        assert_eq!(name, "UwURDP-Setup-0.1.0-beta.2-windows-arm64.exe");
        #[cfg(target_os = "linux")]
        assert!(name.ends_with(".AppImage"));
        #[cfg(target_os = "macos")]
        assert!(name.ends_with("-update"));
    }

    #[test]
    fn packages_are_signed_under_their_own_versioned_names() {
        // What `pnpm release` signs the `.deb` / `.rpm` as (scripts/release.mjs).
        let arch = std::env::consts::ARCH;
        assert_eq!(
            setup_name(Install::Deb, "0.2.0"),
            format!("UwURDP-0.2.0-linux-{arch}.deb")
        );
        assert_eq!(
            setup_name(Install::Rpm, "0.2.0-beta.1"),
            format!("UwURDP-0.2.0-beta.1-linux-{arch}.rpm")
        );
        assert_eq!(
            feed_target(Install::Deb),
            format!("{}-deb", feed_target(Install::Setup))
        );
        assert_eq!(
            feed_target(Install::Rpm),
            format!("{}-rpm", feed_target(Install::Setup))
        );
    }

    #[test]
    fn the_names_installed_versions_look_for_stay() {
        // The release signs under exactly these; older installs know no others.
        #[cfg(all(windows, target_arch = "x86_64"))]
        assert_eq!(
            setup_name(Install::Setup, "1.2.3"),
            "UwURDP-Setup-1.2.3.exe"
        );
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        assert_eq!(
            setup_name(Install::Setup, "1.2.3"),
            "UwURDP-Setup-1.2.3-macos-arm64-update"
        );
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        assert_eq!(
            setup_name(Install::Setup, "1.2.3"),
            "UwURDP-Setup-1.2.3-macos-x64-update"
        );
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        assert_eq!(
            setup_name(Install::Setup, "1.2.3"),
            "UwURDP-Setup-1.2.3-linux-x64.AppImage"
        );
        assert!(setup_name(Install::Setup, "1.2.3").starts_with("UwURDP-Setup-1.2.3"));
    }

    #[test]
    fn the_feed_key_is_the_one_the_release_writes() {
        let target = feed_target(Install::Setup);
        assert!(
            ["windows-", "darwin-", "linux-"]
                .iter()
                .any(|os| target.starts_with(os)),
            "{target}"
        );
        #[cfg(all(windows, target_arch = "x86_64"))]
        assert_eq!(target, "windows-x86_64");
        #[cfg(all(windows, target_arch = "aarch64"))]
        assert_eq!(target, "windows-aarch64");
    }
}
