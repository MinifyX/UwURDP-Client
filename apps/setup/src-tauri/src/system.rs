//! The Windows parts: known folders, shortcuts, processes, WebView2 and
//! message boxes.

use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::core::{Interface, GUID, HSTRING, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::Urlmon::URLDownloadToFileW;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemAlloc, CoTaskMemFree, IPersistFile,
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::LibraryLoader::{
    SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
    PROCESS_ACCESS_RIGHTS, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_LocalAppData, FOLDERID_Programs, FOLDERID_RoamingAppData,
    FOLDERID_System, FOLDERID_UserProgramFiles, IShellLinkW, SHGetKnownFolderPath, ShellLink,
    KF_FLAG_CREATE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONERROR, MB_ICONQUESTION, MB_OK, MB_YESNO,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;

/// DLLs loaded by name come from System32 only, not from the setup's folder or PATH.
pub fn restrict_dll_search() {
    unsafe {
        let _ = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let raw = SHGetKnownFolderPath(id, KF_FLAG_CREATE, None).ok()?;
        let path = raw.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(raw.0 as *const c_void));
        path
    }
}

pub struct Folders {
    /// `%LOCALAPPDATA%\Programs`
    pub user_programs: PathBuf,
    /// Start menu › Programs
    pub start_menu: PathBuf,
    pub desktop: PathBuf,
    pub roaming: PathBuf,
    pub local: PathBuf,
}

pub fn folders() -> Folders {
    let env = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .unwrap_or_default()
    };
    let local = known_folder(&FOLDERID_LocalAppData).unwrap_or_else(|| env("LOCALAPPDATA"));
    let roaming = known_folder(&FOLDERID_RoamingAppData).unwrap_or_else(|| env("APPDATA"));
    Folders {
        user_programs: known_folder(&FOLDERID_UserProgramFiles)
            .unwrap_or_else(|| local.join("Programs")),
        start_menu: known_folder(&FOLDERID_Programs)
            .unwrap_or_else(|| roaming.join("Microsoft\\Windows\\Start Menu\\Programs")),
        desktop: known_folder(&FOLDERID_Desktop)
            .unwrap_or_else(|| env("USERPROFILE").join("Desktop")),
        roaming,
        local,
    }
}

pub struct Shortcut<'a> {
    pub target: &'a Path,
    pub arguments: &'a str,
    pub description: &'a str,
    /// Windows groups the taskbar button and the Start menu entry by this.
    pub app_id: &'a str,
}

pub fn create_shortcut(path: &Path, shortcut: &Shortcut) -> Result<(), String> {
    let fail =
        |step: &str, e: windows::core::Error| format!("Couldn't create the shortcut ({step}): {e}");
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| fail("create", e))?;
        link.SetPath(&HSTRING::from(shortcut.target.as_os_str()))
            .map_err(|e| fail("target", e))?;
        link.SetArguments(&HSTRING::from(shortcut.arguments))
            .map_err(|e| fail("arguments", e))?;
        if let Some(dir) = shortcut.target.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))
                .map_err(|e| fail("folder", e))?;
        }
        link.SetDescription(&HSTRING::from(shortcut.description))
            .map_err(|e| fail("description", e))?;
        link.SetIconLocation(&HSTRING::from(shortcut.target.as_os_str()), 0)
            .map_err(|e| fail("icon", e))?;

        let store: IPropertyStore = link.cast().map_err(|e| fail("properties", e))?;
        let id = wide(shortcut.app_id);
        let memory = CoTaskMemAlloc(id.len() * 2) as *mut u16;
        if memory.is_null() {
            return Err("Couldn't create the shortcut (out of memory).".into());
        }
        std::ptr::copy_nonoverlapping(id.as_ptr(), memory, id.len());
        let mut value = PROPVARIANT::default();
        (*value.Anonymous.Anonymous).vt = VT_LPWSTR;
        (*value.Anonymous.Anonymous).Anonymous.pwszVal = PWSTR(memory);
        // Dropping the PROPVARIANT frees the string again.
        store
            .SetValue(&PKEY_AppUserModel_ID, &value)
            .map_err(|e| fail("app id", e))?;
        store.Commit().map_err(|e| fail("app id", e))?;

        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
        }
        let file: IPersistFile = link.cast().map_err(|e| fail("file", e))?;
        file.Save(&HSTRING::from(path.as_os_str()), true)
            .map_err(|e| fail("save", e))?;
    }
    Ok(())
}

fn process_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(process);
        result.ok()?;
        Some(PathBuf::from(String::from_utf16_lossy(
            &buffer[..size as usize],
        )))
    }
}

/// Process ids running this exact executable.
pub fn processes_of(exe: &Path) -> Vec<u32> {
    let wanted = exe.to_string_lossy().to_lowercase();
    let mut found = Vec::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return found;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snapshot, &mut entry).is_ok();
        while ok {
            let end = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(0);
            let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
            let candidate = exe
                .file_name()
                .is_some_and(|file| file.to_string_lossy().eq_ignore_ascii_case(&name));
            if candidate
                && entry.th32ProcessID != std::process::id()
                && process_path(entry.th32ProcessID)
                    .is_some_and(|path| path.to_string_lossy().to_lowercase() == wanted)
            {
                found.push(entry.th32ProcessID);
            }
            ok = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    found
}

fn open(pid: u32, rights: PROCESS_ACCESS_RIGHTS) -> Option<HANDLE> {
    unsafe { OpenProcess(rights, false, pid).ok() }
}

/// Waits for a process to end; true if it did in time.
pub fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let Some(process) = open(pid, PROCESS_SYNCHRONIZE) else {
        return true;
    };
    let result =
        unsafe { WaitForSingleObject(process, timeout.as_millis().min(u32::MAX as u128) as u32) };
    unsafe {
        let _ = CloseHandle(process);
    }
    result == WAIT_OBJECT_0
}

/// Ends every process of an executable, waiting a little for each.
pub fn stop_processes(exe: &Path) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    for pid in processes_of(exe) {
        if let Some(process) = open(pid, PROCESS_TERMINATE) {
            unsafe {
                let _ = TerminateProcess(process, 0);
                let _ = CloseHandle(process);
            }
        }
        wait_for_exit(pid, deadline.saturating_duration_since(Instant::now()));
    }
    if processes_of(exe).is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} is still running and couldn't be closed.",
            exe.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "UwURDP".into())
        ))
    }
}

/// Starts a program without waiting for it and without a console window.
pub fn spawn_detached(exe: &Path, args: &[&str]) -> Result<(), String> {
    std::process::Command::new(exe)
        .args(args)
        .current_dir(exe.parent().unwrap_or(Path::new(".")))
        .creation_flags(DETACHED_PROCESS)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Couldn't start {}: {e}", exe.display()))
}

/// Deletes a file a few seconds after this process has ended (for the
/// uninstaller's temporary copy).
pub fn delete_after_exit(file: &Path) {
    // Only ever this setup's own copy in the temp folder. The path goes in
    // through an environment variable: cmd expands %…% inside quotes too, so a
    // profile path with a percent sign in it must never become part of the
    // command line itself.
    //
    // cmd and ping by their full paths, started in System32: this copy runs
    // from the temp folder, and a bare `cmd` is looked up next to the running
    // program first — as is `ping` in cmd's working folder — so a cmd.exe or
    // ping.bat someone left in %TEMP% would run instead.
    let Some(system) = known_folder(&FOLDERID_System) else {
        return;
    };
    let _ = std::process::Command::new(system.join("cmd.exe"))
        .raw_arg(
            "/c \"\"%UWURDP_PING%\" 127.0.0.1 -n 4 > nul & del /f /q \"%UWURDP_SETUP_COPY%\"\"",
        )
        .env("UWURDP_PING", system.join("PING.EXE"))
        .env("UWURDP_SETUP_COPY", file)
        .current_dir(&system)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

pub fn ask(title: &str, text: &str) -> bool {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}

pub fn alert(title: &str, text: &str) {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MB_OK | MB_ICONERROR,
        );
    }
}

const WEBVIEW2_CLIENT: &str =
    r"Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

pub fn webview2_installed() -> bool {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;
    let version = |root, path: String| {
        RegKey::predef(root)
            .open_subkey(path)
            .and_then(|key| key.get_value::<String, _>("pv"))
            .is_ok_and(|v| !v.is_empty() && v != "0.0.0.0")
    };
    version(
        HKEY_LOCAL_MACHINE,
        format!(r"SOFTWARE\WOW6432Node\{WEBVIEW2_CLIENT}"),
    ) || version(HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\{WEBVIEW2_CLIENT}"))
        || version(HKEY_CURRENT_USER, format!(r"Software\{WEBVIEW2_CLIENT}"))
}

/// Downloads Microsoft's WebView2 bootstrapper and runs it.
pub fn install_webview2() -> Result<(), String> {
    // A fresh folder of our own, so nothing already lying in the temp folder
    // under that name gets run.
    let folder = std::env::temp_dir().join(format!("UwURDP-WebView2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).map_err(|e| format!("Couldn't download WebView2: {e}"))?;
    let target = folder.join("MicrosoftEdgeWebview2Setup.exe");
    unsafe {
        URLDownloadToFileW(
            None,
            &HSTRING::from("https://go.microsoft.com/fwlink/p/?LinkId=2124703"),
            &HSTRING::from(target.as_os_str()),
            0,
            None,
        )
        .map_err(|e| format!("Couldn't download WebView2: {e}"))?;
    }
    let status = std::process::Command::new(&target)
        .args(["/silent", "/install"])
        .status()
        .map_err(|e| format!("Couldn't start the WebView2 setup: {e}"));
    let _ = std::fs::remove_dir_all(&folder);
    if status?.success() && webview2_installed() {
        Ok(())
    } else {
        Err("WebView2 couldn't be installed.".into())
    }
}
