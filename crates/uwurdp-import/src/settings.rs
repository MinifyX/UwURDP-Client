//! RDCMan's own settings file.
//!
//! `%LOCALAPPDATA%\Microsoft\Remote Desktop Connection Manager\RDCMan.settings`
//! records the `.rdg` files RDCMan had open (so the app can offer them) and the
//! credential profiles the user saved at *Local* scope (so `scope="Local"`
//! references inside a `.rdg` resolve). Everywhere but Windows there is no such
//! file, so this returns empty.

use std::path::PathBuf;

use crate::NamedCredential;

/// What [`rdcman_settings`] found.
#[derive(Debug, Default)]
pub struct RdcManSettings {
    /// The `.rdg` files RDCMan last had open, in the order it listed them.
    pub files: Vec<PathBuf>,
    /// The user's Local-scope credential profiles.
    pub profiles: Vec<NamedCredential>,
}

/// Read RDCMan's settings for the current user. Empty off Windows, or when the
/// file is missing or unreadable.
pub fn rdcman_settings() -> RdcManSettings {
    #[cfg(windows)]
    {
        windows::read()
    }
    #[cfg(not(windows))]
    {
        RdcManSettings::default()
    }
}

#[cfg(windows)]
mod windows {
    use super::RdcManSettings;
    use crate::dpapi::Dpapi;
    use crate::rdg::parse_credentials_node;
    use std::path::PathBuf;

    /// `%LOCALAPPDATA%\Microsoft\Remote Desktop Connection Manager\RDCMan.settings`.
    fn settings_path() -> Option<PathBuf> {
        let local = std::env::var_os("LOCALAPPDATA")?;
        Some(
            PathBuf::from(local)
                .join("Microsoft")
                .join("Remote Desktop Connection Manager")
                .join("RDCMan.settings"),
        )
    }

    pub(super) fn read() -> RdcManSettings {
        let mut out = RdcManSettings::default();
        let Some(path) = settings_path() else {
            return out;
        };
        let Ok(xml) = std::fs::read_to_string(&path) else {
            return out;
        };
        let Ok(doc) = roxmltree::Document::parse(&xml) else {
            return out;
        };

        // <FilesToOpen> holds one <item> per recently opened .rdg.
        if let Some(files) = doc.descendants().find(|n| n.has_tag_name("FilesToOpen")) {
            for item in files.children().filter(|n| n.is_element()) {
                if let Some(text) = item.text().map(str::trim).filter(|t| !t.is_empty()) {
                    out.files.push(PathBuf::from(text));
                }
            }
        }

        // Each <credentialsProfile> is a saved Local-scope login. The file lives
        // on the user's own machine, so DPAPI can read the passwords directly.
        let dpapi = Dpapi;
        for node in doc
            .descendants()
            .filter(|n| n.has_tag_name("credentialsProfile"))
        {
            let (cred, _failed) = parse_credentials_node(node, &dpapi, false);
            if let Some(name) = cred.label.clone() {
                out.profiles.push(crate::NamedCredential {
                    name,
                    credential: cred,
                });
            }
        }
        out
    }
}
