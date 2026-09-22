//! The macOS and Linux parts: processes, and starting programs apart from the
//! setup.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Only Windows searches the program's own folder for libraries.
pub fn restrict_dll_search() {}

/// Every process except this one whose executable lies inside `dir`.
pub fn processes_under(dir: &Path) -> Vec<i32> {
    let me = std::process::id() as i32;
    running()
        .into_iter()
        .filter(|(pid, exe)| *pid != me && exe.starts_with(dir))
        .map(|(pid, _)| pid)
        .collect()
}

/// Process ids and their executables. Linux has them in /proc; macOS gives
/// the full path of the executable as `comm` in `ps`.
#[cfg(target_os = "linux")]
fn running() -> Vec<(i32, PathBuf)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid: i32 = entry.file_name().to_str()?.parse().ok()?;
            let exe = std::fs::read_link(entry.path().join("exe")).ok()?;
            Some((pid, exe))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn running() -> Vec<(i32, PathBuf)> {
    let Ok(output) = std::process::Command::new("/bin/ps")
        .args(["-axww", "-o", "pid=,comm="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, exe) = line.split_once(' ')?;
            Some((pid.parse().ok()?, PathBuf::from(exe.trim())))
        })
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn running() -> Vec<(i32, PathBuf)> {
    Vec::new()
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only asks whether the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Waits for a process to end; true if it did in time.
pub fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let pid = pid as i32;
    while alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

/// Asks every process running from `dir` to end, and makes it after a while.
pub fn stop_processes_under(dir: &Path, name: &str) -> Result<(), String> {
    let pids = processes_under(dir);
    if pids.is_empty() {
        return Ok(());
    }
    for &pid in &pids {
        // SAFETY: plain signals to processes we just found.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while pids.iter().any(|&pid| alive(pid)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    for &pid in pids.iter().filter(|&&pid| alive(pid)) {
        // SAFETY: as above.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    if processes_under(dir).is_empty() {
        Ok(())
    } else {
        Err(format!("{name} is still running and couldn't be closed."))
    }
}

/// Starts a program in a process group of its own, so it outlives the setup.
pub fn spawn_detached(program: &Path, args: &[&str]) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    std::process::Command::new(program)
        .args(args)
        .current_dir(program.parent().unwrap_or(Path::new("/")))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Couldn't start {}: {e}", program.display()))
}

/// Runs a helper and ignores how it went — for the things that make an
/// install nicer but not work (refreshing a menu cache, a trust flag).
pub fn best_effort(program: &str, args: &[&str]) {
    let _ = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// The user's home folder.
pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// An XDG base directory from the environment, or its default under home.
#[cfg(not(target_os = "macos"))]
pub fn xdg(variable: &str, default: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home().join(default))
}

/// The desktop folder as `xdg-user-dirs` names it, `~/Desktop` otherwise.
#[cfg(not(target_os = "macos"))]
pub fn desktop_dir() -> PathBuf {
    let home = home();
    let config = xdg("XDG_CONFIG_HOME", ".config").join("user-dirs.dirs");
    std::fs::read_to_string(config)
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let value = line.trim().strip_prefix("XDG_DESKTOP_DIR=")?;
                let value = value.trim().trim_matches('"');
                let path = match value.strip_prefix("$HOME") {
                    Some(rest) => home.join(rest.trim_start_matches('/')),
                    None => PathBuf::from(value),
                };
                path.is_absolute().then_some(path)
            })
        })
        .unwrap_or_else(|| home.join("Desktop"))
}

/// Whether this user may create files in `dir`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn writable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: a NUL-terminated path that lives for the call.
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}
