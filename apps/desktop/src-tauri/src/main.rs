// Keep the console window away on Windows release builds — a terminal app that
// opens a second stray terminal on launch looks broken.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    uwurdp_desktop_lib::run()
}
