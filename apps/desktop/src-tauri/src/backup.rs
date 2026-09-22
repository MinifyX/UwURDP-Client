//! Exporting everything into an `.uwurdp` file, and reading one back.
//!
//! The file format and its sealing live in `uwurdp_store::backup`; this layer
//! adds the native dialogs and keeps a picked file in memory between "what's
//! in it?" and "import it", so the page never reads or names a path.

use crate::dialogs::{self, Filter};
use crate::import::ImportReport;
use crate::AppState;
use parking_lot::Mutex;
use serde::Serialize;
use std::path::Path;
use tauri::{AppHandle, State};
use uuid::Uuid;
use uwurdp_store::{BackupSummary, StoreError};
use zeroize::Zeroizing;

const FILTER: Filter<'static> = Filter {
    name: "UwURDP-Export",
    extensions: &["uwurdp"],
};

/// The file the user picked for import, until it is imported or replaced.
#[derive(Default)]
pub(crate) struct PickedExport(Mutex<Option<(String, Zeroizing<Vec<u8>>)>>);

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum BackupFailure {
    /// Secrets need the vault open — to read them out, or to seal them in.
    VaultLocked,
    PasswordRequired,
    PasswordWrong,
    Error {
        message: String,
    },
}

impl From<StoreError> for BackupFailure {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::VaultLocked => Self::VaultLocked,
            StoreError::ExportPasswordRequired => Self::PasswordRequired,
            StoreError::ExportPasswordWrong => Self::PasswordWrong,
            other => Self::Error {
                message: other.to_string(),
            },
        }
    }
}

fn failure(message: impl std::fmt::Display) -> BackupFailure {
    BackupFailure::Error {
        message: message.to_string(),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Exported {
    file_name: String,
    summary: BackupSummary,
}

/// Write every host, group, login and trusted certificate into a file the
/// user picks. With `secrets`, stored passwords come along and the whole
/// file is sealed with `password`. `None` when the dialog was cancelled.
#[tauri::command]
pub(crate) async fn export_hosts(
    app: AppHandle,
    state: State<'_, AppState>,
    secrets: bool,
    password: Option<String>,
) -> Result<Option<Exported>, BackupFailure> {
    let password = password.map(Zeroizing::new);
    if secrets && password.as_ref().is_none_or(|p| p.is_empty()) {
        return Err(BackupFailure::PasswordRequired);
    }
    let backup = state.store.export_backup(secrets)?;
    let summary = backup.summary();
    let bytes = uwurdp_store::encode_export(
        &backup,
        password.as_ref().map(|p| p.as_bytes()),
        env!("CARGO_PKG_VERSION"),
    )?;
    drop(backup);

    let name = format!("UwURDP-Export-{}.uwurdp", today());
    let Some(path) = dialogs::save(&app, "Hosts exportieren", &name, FILTER).await else {
        return Ok(None);
    };
    write_atomically(&path, &bytes).map_err(failure)?;
    Ok(Some(Exported {
        file_name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(name),
        summary,
    }))
}

/// Write next to the target and rename over it, so a crash never leaves half
/// an export where a good one was.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension(format!("uwurdp-{}.tmp", Uuid::now_v7().simple()));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Picked {
    token: String,
    file_name: String,
    sealed: bool,
    /// Known right away for a file without a password.
    summary: Option<BackupSummary>,
}

#[tauri::command]
pub(crate) async fn pick_export_file(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<Picked>, BackupFailure> {
    let Some(path) = dialogs::open(&app, "UwURDP-Export öffnen", FILTER).await else {
        return Ok(None);
    };
    let size = std::fs::metadata(&path).map_err(failure)?.len();
    if size > uwurdp_store::backup::MAX_EXPORT_BYTES as u64 {
        return Err(failure("the file is too large to be an export"));
    }
    let bytes = Zeroizing::new(std::fs::read(&path).map_err(failure)?);
    let sealed = uwurdp_store::export_is_sealed(&bytes)?;
    let summary = if sealed {
        None
    } else {
        Some(uwurdp_store::decode_export(&bytes, None)?.summary())
    };
    let token = Uuid::now_v7().to_string();
    *state.picked_export.0.lock() = Some((token.clone(), bytes));
    Ok(Some(Picked {
        token,
        file_name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        sealed,
        summary,
    }))
}

fn picked(state: &AppState, token: &str) -> Result<Zeroizing<Vec<u8>>, BackupFailure> {
    match &*state.picked_export.0.lock() {
        Some((picked, bytes)) if picked == token => Ok(bytes.clone()),
        _ => Err(failure("pick the file again")),
    }
}

/// Run `work` on a worker thread: opening a sealed export derives a key from
/// its password, which takes about a second on purpose, and the window should
/// keep drawing meanwhile.
async fn off_the_main_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, BackupFailure> + Send + 'static,
) -> Result<T, BackupFailure> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(failure)?
}

/// What a sealed file holds, once its password is known.
#[tauri::command]
pub(crate) async fn read_export_file(
    state: State<'_, AppState>,
    token: String,
    password: Option<String>,
) -> Result<BackupSummary, BackupFailure> {
    let bytes = picked(&state, &token)?;
    let password = password.map(Zeroizing::new);
    off_the_main_thread(move || {
        Ok(uwurdp_store::decode_export(&bytes, password.as_ref().map(|p| p.as_bytes()))?.summary())
    })
    .await
}

#[tauri::command]
pub(crate) async fn import_export_file(
    state: State<'_, AppState>,
    token: String,
    password: Option<String>,
) -> Result<ImportReport, BackupFailure> {
    let bytes = picked(&state, &token)?;
    let password = password.map(Zeroizing::new);
    let store = std::sync::Arc::clone(&state.store);
    let outcome = off_the_main_thread(move || {
        let mut backup =
            uwurdp_store::decode_export(&bytes, password.as_ref().map(|p| p.as_bytes()))?;
        // A file is data from anywhere. A certificate is only taken when its
        // fingerprint really is that certificate's.
        backup.known_hosts.retain(|known| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(known.public_key.trim())
                .is_ok_and(|der| uwurdp_core::certificate_fingerprint(&der) == known.fingerprint)
        });
        Ok(store.import_backup(backup)?)
    })
    .await?;
    *state.picked_export.0.lock() = None;
    Ok(ImportReport::of(outcome, Vec::new()))
}

/// Today as `YYYY-MM-DD`, in UTC — good enough for a file name.
fn today() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0) as i64;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Howard Hinnant's days-to-date, for the proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_come_out_right() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_713), (2026, 9, 17));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }
}
