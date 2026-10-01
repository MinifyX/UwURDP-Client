//! This computer's clipboard, as the worker needs it: behind a trait, so the
//! worker's logic is tested against a fake one.
//!
//! `arboard` does the work on every platform (text, HTML, pictures, file
//! lists: `CF_HDROP` on Windows, file URLs on macOS, `text/uri-list` on
//! Linux). What it lacks is a cheap way to see that anything changed; Windows
//! and macOS keep a counter for exactly that, read here without opening the
//! clipboard. Linux has none, so there the worker compares contents.

use super::formats::Rgba;
use std::path::PathBuf;
use tracing::warn;

pub(crate) trait LocalClipboard {
    /// A number that changes whenever anything puts something on the
    /// clipboard, if the platform has one (`GetClipboardSequenceNumber`,
    /// `NSPasteboard.changeCount`). Must be cheap: it's read four times a
    /// second.
    fn change_token(&mut self) -> Option<u64> {
        None
    }
    /// The files and folders on the clipboard, if that is what it holds.
    fn files(&mut self) -> Option<Vec<PathBuf>>;
    fn text(&mut self) -> Option<String>;
    fn html(&mut self) -> Option<String>;
    fn image(&mut self) -> Option<Rgba>;
    /// Text, with an HTML version of it when there is one.
    fn set_text(&mut self, text: &str, html: Option<&str>) -> Result<(), String>;
    fn set_image(&mut self, image: Rgba) -> Result<(), String>;
    fn set_files(&mut self, files: &[PathBuf]) -> Result<(), String>;
}

pub(crate) struct Arboard(arboard::Clipboard);

pub(crate) fn open_system_clipboard() -> Option<Box<dyn LocalClipboard>> {
    match arboard::Clipboard::new() {
        Ok(clipboard) => Some(Box::new(Arboard(clipboard))),
        Err(e) => {
            warn!(error = %e, "local clipboard unavailable; clipboard sharing is off");
            None
        }
    }
}

impl LocalClipboard for Arboard {
    #[cfg(windows)]
    fn change_token(&mut self) -> Option<u64> {
        // SAFETY: no arguments, no preconditions; it never opens the clipboard.
        let n = unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() };
        // Zero means the caller has no access (another desktop); fall back
        // to comparing contents then.
        (n != 0).then_some(u64::from(n))
    }

    #[cfg(target_os = "macos")]
    fn change_token(&mut self) -> Option<u64> {
        let count = objc2_app_kit::NSPasteboard::generalPasteboard().changeCount();
        Some(count as u64)
    }

    fn files(&mut self) -> Option<Vec<PathBuf>> {
        let files = self.0.get().file_list().ok()?;
        let files: Vec<PathBuf> = files
            .into_iter()
            // arboard splits `text/uri-list` at LF only; its lines end in CRLF.
            .map(|p| match p.to_str() {
                Some(s) if s.ends_with('\r') => PathBuf::from(s.trim_end_matches('\r')),
                _ => p,
            })
            .filter(|p| p.exists())
            .collect();
        (!files.is_empty()).then_some(files)
    }

    fn text(&mut self) -> Option<String> {
        self.0.get_text().ok().filter(|t| !t.is_empty())
    }

    fn html(&mut self) -> Option<String> {
        self.0.get().html().ok().filter(|t| !t.is_empty())
    }

    fn image(&mut self) -> Option<Rgba> {
        let image = self.0.get_image().ok()?;
        Some(Rgba {
            width: image.width,
            height: image.height,
            bytes: image.bytes.into_owned(),
        })
    }

    fn set_text(&mut self, text: &str, html: Option<&str>) -> Result<(), String> {
        match html {
            Some(html) => self.0.set_html(html, Some(text)),
            None => self.0.set_text(text),
        }
        .map_err(|e| e.to_string())
    }

    fn set_image(&mut self, image: Rgba) -> Result<(), String> {
        self.0
            .set_image(arboard::ImageData {
                width: image.width,
                height: image.height,
                bytes: image.bytes.into(),
            })
            .map_err(|e| e.to_string())
    }

    fn set_files(&mut self, files: &[PathBuf]) -> Result<(), String> {
        self.0.set().file_list(files).map_err(|e| e.to_string())
    }
}
