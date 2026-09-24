//! Single `.rdp` files saved by mstsc.
//!
//! An `.rdp` is a flat list of `key:type:value` lines, where `type` is `s`
//! (string), `i` (integer) or `b` (binary hex). One file is one host, so there
//! is no tree and no inheritance — the whole job is recognising keys, mapping
//! the ones we model and keeping the rest in `extras` so nothing a user set in
//! mstsc quietly vanishes. The file is UTF-8 or, more often, UTF-16LE with a
//! byte-order mark.

use std::collections::BTreeMap;

use crate::{
    split_host_port, Decrypt, ImportBundle, ImportError, ImportedAudio, ImportedCredential,
    ImportedDisplay, ImportedGateway, ImportedHost, ImportedSettings, PasswordOrigin, Source,
};

const DEFAULT_PORT: u16 = 3389;

/// Keys that are pure client-window bookkeeping, not settings worth showing.
const NOISE: &[&str] = &["winposstr"];

/// Parse one `.rdp` file. `file_stem` names the host, since the file itself has
/// no display name.
pub fn parse_rdp_file(
    bytes: &[u8],
    file_stem: &str,
    decrypt: &dyn Decrypt,
) -> Result<ImportBundle, ImportError> {
    let text = decode_text(bytes).ok_or_else(|| ImportError::Read("not UTF-8 or UTF-16".into()))?;
    let map = parse_lines(&text);
    if map.is_empty() {
        return Err(ImportError::Empty);
    }

    let mut bundle = ImportBundle {
        source: Some(Source::RdpFile),
        ..Default::default()
    };
    let mut extras: Vec<(String, String)> = Vec::new();

    // Address and port. "full address" may carry a ":port"; "server port" wins.
    let (address, addr_port) = map
        .get("full address")
        .map(|v| split_host_port(v))
        .unwrap_or_default();
    let port = map
        .get("server port")
        .and_then(|v| v.parse().ok())
        .or(addr_port)
        .unwrap_or(DEFAULT_PORT);

    // Credentials.
    let (username, domain) = split_username(
        map.get("username").map(String::as_str),
        map.get("domain").map(String::as_str),
    );
    let mut pw_failed = false;
    let password = match map.get("password 51") {
        Some(hex) => match hex_to_bytes(hex) {
            Some(blob) if !blob.is_empty() => match decrypt.decrypt(&blob) {
                Some(secret) => Some(secret),
                None => {
                    pw_failed = true;
                    None
                }
            },
            _ => None,
        },
        None => None,
    };
    let cred = ImportedCredential {
        label: None,
        username,
        domain,
        // `password 51` is only ever a DPAPI blob: one this account opened.
        origin: if password.is_some() {
            PasswordOrigin::Unsealed
        } else {
            PasswordOrigin::InFile
        },
        password,
    };
    let credential = bundle.intern_credential(cred);

    // Settings.
    let settings = ImportedSettings {
        display: parse_display(&map),
        color_depth: map.get("session bpp").and_then(|v| v.parse().ok()),
        admin: map.get("administrative session").map(|v| v == "1"),
        audio: map.get("audiomode").and_then(|v| match v.as_str() {
            "0" => Some(ImportedAudio::Local),
            "1" => Some(ImportedAudio::Remote),
            "2" => Some(ImportedAudio::Off),
            _ => None,
        }),
        clipboard: map.get("redirectclipboard").map(|v| v == "1"),
        gateway: parse_gateway(&map),
    };

    // Everything recognised as a line but not mapped becomes an extra, minus
    // the keys we consumed and the pure window-position noise.
    let consumed = [
        "full address",
        "server port",
        "username",
        "domain",
        "password 51",
        "screen mode id",
        "smart sizing",
        "dynamic resolution",
        "desktopwidth",
        "desktopheight",
        "session bpp",
        "administrative session",
        "audiomode",
        "redirectclipboard",
        "gatewayhostname",
        "gatewayusagemethod",
    ];
    for (key, value) in &map {
        if consumed.contains(&key.as_str()) || NOISE.contains(&key.as_str()) {
            continue;
        }
        if value.is_empty() {
            continue;
        }
        extras.push((key.clone(), value.clone()));
    }

    if pw_failed {
        bundle.skipped.push((
            "password could not be decrypted".into(),
            "DPAPI could not read it for this user; the username was kept".into(),
        ));
    }

    bundle.hosts.push(ImportedHost {
        name: file_stem.to_string(),
        address,
        port,
        group_path: None,
        credential,
        comment: None,
        settings,
        extras,
    });
    Ok(bundle)
}

fn parse_display(map: &BTreeMap<String, String>) -> Option<ImportedDisplay> {
    if map.get("screen mode id").map(String::as_str) == Some("2") {
        return Some(ImportedDisplay::FullScreen);
    }
    let fit = map.get("smart sizing").map(String::as_str) == Some("1")
        || map.get("dynamic resolution").map(String::as_str) == Some("1");
    if fit {
        return Some(ImportedDisplay::FitWindow);
    }
    let w = map.get("desktopwidth").and_then(|v| v.parse().ok())?;
    let h = map.get("desktopheight").and_then(|v| v.parse().ok())?;
    Some(ImportedDisplay::Fixed {
        width: w,
        height: h,
    })
}

fn parse_gateway(map: &BTreeMap<String, String>) -> Option<ImportedGateway> {
    let address = map
        .get("gatewayhostname")
        .filter(|h| !h.is_empty())?
        .clone();
    // 0 = do not use a gateway. Any other method means it is in play.
    if map.get("gatewayusagemethod").map(String::as_str) == Some("0") {
        return None;
    }
    Some(ImportedGateway {
        address,
        port: None,
        credential: None,
        use_host_credentials: false,
        // Method 2 is "bypass gateway for local addresses".
        bypass_local: map.get("gatewayusagemethod").map(String::as_str) == Some("2"),
    })
}

/// Decode the file to text, honouring a UTF-16LE/BE or UTF-8 BOM and otherwise
/// assuming UTF-8.
fn decode_text(bytes: &[u8]) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let (pairs, _) = rest.as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().map(|c| u16::from_le_bytes(*c)).collect();
        return Some(String::from_utf16_lossy(&units));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let (pairs, _) = rest.as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().map(|c| u16::from_be_bytes(*c)).collect();
        return Some(String::from_utf16_lossy(&units));
    }
    let rest = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    Some(String::from_utf8_lossy(rest).into_owned())
}

/// Collect `key:type:value` lines into a map, lower-casing keys. The value may
/// itself contain colons (a `full address` with a port), so only the first two
/// fields are split off.
fn parse_lines(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut it = line.splitn(3, ':');
        let (Some(key), Some(_ty), Some(value)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        map.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    map
}

/// Split `DOMAIN\user` into domain and user; pass `user@domain` through as the
/// username. An explicit `domain` line takes precedence.
fn split_username(
    username: Option<&str>,
    domain_line: Option<&str>,
) -> (Option<String>, Option<String>) {
    let mut domain = domain_line.filter(|d| !d.is_empty()).map(str::to_string);
    let username = username.filter(|u| !u.is_empty()).map(|u| {
        if let Some((dom, user)) = u.split_once('\\') {
            if domain.is_none() && !dom.is_empty() {
                domain = Some(dom.to_string());
            }
            user.to_string()
        } else {
            u.to_string()
        }
    });
    (username, domain)
}

/// Decode a hex string (as `password 51` stores the DPAPI blob) into bytes.
fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    let hex: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}
