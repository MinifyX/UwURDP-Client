// No console window next to the setup in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
#[cfg_attr(windows, path = "install.rs")]
#[cfg_attr(unix, path = "install_unix.rs")]
mod install;
#[cfg_attr(windows, path = "system.rs")]
#[cfg_attr(unix, path = "system_unix.rs")]
mod system;
mod versions;

fn main() {
    // On Windows, DLLs loaded later at runtime come from System32 only; this
    // must happen before anything else loads one. Nothing to do elsewhere.
    system::restrict_dll_search();
    // For CI: unpack what is packed inside into a folder and check it,
    // without a window. The one way to see a macOS or Linux payload work on
    // a machine that has no desktop.
    #[cfg(unix)]
    if let Some(at) = std::env::args().position(|arg| arg == "--check-payload") {
        let dir = std::env::args()
            .nth(at + 1)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("uwurdp-setup-check"));
        match install::check_payload(&dir) {
            Ok(report) => {
                println!("{report}");
                std::process::exit(0);
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    app::run();
}
