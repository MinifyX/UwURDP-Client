//! Remote Desktop Connection Manager `.rdg` files.
//!
//! An `.rdg` is `<RDCMan>` wrapping one `<file>` that holds a tree of `<group>`
//! and `<server>` nodes. Credentials and six kinds of settings hang off the
//! file, every group and every server, each with an `inherit` attribute that
//! says whether the node overrides its parent or takes the parent's value. The
//! app wants none of that machinery: it wants each host with its settings fully
//! resolved and each group with the credentials it would actually use. So this
//! module walks the tree once, resolving inheritance as it goes.
//!
//! Two schema generations are in the wild and both are handled. RDCMan 2.2
//! (schemaVersion 1) puts a server's `name`/`displayName`/`comment` and its
//! settings directly under `<server>`; 2.7+ (schemaVersion 3) moves the
//! identity into a `<properties>` child and keeps the settings as siblings of
//! it. [`find_child`] looks in both places so the rest of the code does not care.

use base64::Engine;
use roxmltree::{Document, Node};

use crate::{
    Decrypt, ImportBundle, ImportError, ImportedAudio, ImportedCredential, ImportedDisplay,
    ImportedGateway, ImportedGroup, ImportedHost, ImportedSettings, NamedCredential, Source,
};

/// The default RDP port, used when neither the address nor the connection
/// settings named one.
const DEFAULT_PORT: u16 = 3389;

/// Parse an `.rdg` document into the neutral bundle.
///
/// `local_profiles` are the app's own credential profiles, used to resolve
/// `scope="Local"` references (RDCMan keeps those outside the file, in
/// `RDCMan.settings`). `decrypt` turns a DPAPI password blob into plaintext;
/// when it cannot, the username and domain are kept and the password dropped,
/// with a single note in [`ImportBundle::skipped`].
pub fn parse_rdg(
    xml: &str,
    local_profiles: &[NamedCredential],
    decrypt: &dyn Decrypt,
) -> Result<ImportBundle, ImportError> {
    let doc = Document::parse(xml).map_err(|e| ImportError::Read(e.to_string()))?;

    let file = doc
        .descendants()
        .find(|n| n.has_tag_name("file"))
        .ok_or(ImportError::Empty)?;

    // A certificate-encrypted file seals passwords to a private key we do not
    // have, so decryption is hopeless for the whole file — recognise it up front.
    let cert_encrypted = doc.descendants().any(|n| {
        n.has_tag_name("encryptionSettings")
            && n.descendants()
                .any(|c| c.is_element() && c.tag_name().name().to_lowercase().contains("cert"))
    });

    let mut ctx = Ctx {
        bundle: ImportBundle {
            source: Some(Source::RdcMan),
            ..Default::default()
        },
        local_profiles,
        decrypt,
        cert_encrypted,
        pw_failed: false,
    };

    // File-scope credential profiles, resolvable by name for scope="File".
    let file_profiles = collect_file_profiles(file, &mut ctx);

    // The file node is the root of inheritance: its own effective credentials
    // and settings are what unqualified children fall back to.
    let file_creds = ctx.resolve_creds(file, None, &file_profiles);
    let root_settings = EffNodes::at(file, &EffNodes::default());

    walk(
        file,
        None,
        file_creds,
        &root_settings,
        &file_profiles,
        &mut ctx,
    );

    if ctx.pw_failed {
        let reason = if cert_encrypted {
            "the file is certificate-encrypted; usernames were kept"
        } else {
            "DPAPI could not read them for this user; usernames were kept"
        };
        ctx.bundle
            .skipped
            .push(("passwords could not be decrypted".into(), reason.into()));
    }

    if ctx.bundle.hosts.is_empty() && ctx.bundle.groups.is_empty() {
        return Err(ImportError::Empty);
    }
    Ok(ctx.bundle)
}

/// State threaded through the walk.
struct Ctx<'a> {
    bundle: ImportBundle,
    local_profiles: &'a [NamedCredential],
    decrypt: &'a dyn Decrypt,
    cert_encrypted: bool,
    pw_failed: bool,
}

/// A resolved credential specification for a `logonCredentials` element.
enum CredSpec {
    /// Missing element or `inherit="FromParent"`: take the parent's.
    Inherit,
    /// `inherit="None"` with nothing usable: explicitly no credentials.
    Empty,
    /// Inline user/domain/password.
    Inline(ImportedCredential, bool),
    /// A reference to a named profile.
    Profile { name: String, local: bool },
}

impl<'a> Ctx<'a> {
    /// The effective credential index for a node: its own when it overrides,
    /// otherwise the parent's. Interns into the bundle so equal credentials
    /// collapse to one entry.
    fn resolve_creds(
        &mut self,
        node: Node,
        parent: Option<usize>,
        file_profiles: &[(String, ImportedCredential, bool)],
    ) -> Option<usize> {
        match self.logon_spec(find_child(node, "logonCredentials")) {
            CredSpec::Inherit => parent,
            CredSpec::Empty => None,
            CredSpec::Inline(cred, failed) => {
                self.pw_failed |= failed;
                self.bundle.intern_credential(cred)
            }
            CredSpec::Profile { name, local } => self.resolve_profile(&name, local, file_profiles),
        }
    }

    /// The credential a server records for *itself* (ignoring inheritance),
    /// used for the special host rule in [`walk`].
    fn own_creds(
        &mut self,
        node: Node,
        file_profiles: &[(String, ImportedCredential, bool)],
    ) -> Option<usize> {
        match self.logon_spec(find_child(node, "logonCredentials")) {
            CredSpec::Inherit | CredSpec::Empty => None,
            CredSpec::Inline(cred, failed) => {
                self.pw_failed |= failed;
                self.bundle.intern_credential(cred)
            }
            CredSpec::Profile { name, local } => self.resolve_profile(&name, local, file_profiles),
        }
    }

    fn resolve_profile(
        &mut self,
        name: &str,
        local: bool,
        file_profiles: &[(String, ImportedCredential, bool)],
    ) -> Option<usize> {
        if local {
            if let Some(p) = self.local_profiles.iter().find(|p| p.name == name) {
                let cred = clone_cred(&p.credential);
                return self.bundle.intern_credential(cred);
            }
        } else if let Some((_, cred, failed)) = file_profiles.iter().find(|(n, _, _)| n == name) {
            self.pw_failed |= *failed;
            let cred = clone_cred(cred);
            return self.bundle.intern_credential(cred);
        }
        self.bundle.skipped.push((
            format!("credential profile '{name}'"),
            "referenced but not found; the host will ask at connect time".into(),
        ));
        None
    }

    /// Classify a `logonCredentials` element.
    fn logon_spec(&self, node: Option<Node>) -> CredSpec {
        let Some(node) = node else {
            return CredSpec::Inherit;
        };
        if inherits_from_parent(node) {
            return CredSpec::Inherit;
        }
        let profile = find_text(node, "profileName");
        let has_user = find_child(node, "userName").is_some();
        match profile.as_deref() {
            // "Custom" or an absent profile name means the values are inline.
            Some("Custom") | None => {
                let (cred, failed) = self.parse_inline(node, None);
                if cred.is_empty() {
                    CredSpec::Empty
                } else {
                    CredSpec::Inline(cred, failed)
                }
            }
            Some(name) if has_user => {
                // A named node that still carries its own values: inline, keep name.
                let (cred, failed) = self.parse_inline(node, Some(name.to_string()));
                CredSpec::Inline(cred, failed)
            }
            Some(name) => CredSpec::Profile {
                name: name.to_string(),
                local: profile_is_local(node),
            },
        }
    }

    /// Read user/domain/password directly off a credentials element.
    fn parse_inline(&self, node: Node, label: Option<String>) -> (ImportedCredential, bool) {
        let (password, failed) = self.read_password(node);
        let cred = ImportedCredential {
            label,
            username: find_text(node, "userName"),
            domain: find_text(node, "domain"),
            password,
        };
        (cred, failed)
    }

    /// Decode a `<password>` child. Clear-text passwords come through as-is; a
    /// base64 DPAPI blob goes through [`Decrypt`]. Returns whether a password
    /// was present but could not be recovered.
    fn read_password(&self, node: Node) -> (Option<crate::Secret>, bool) {
        let Some(pw) = find_child(node, "password") else {
            return (None, false);
        };
        let Some(text) = pw.text().map(str::trim).filter(|t| !t.is_empty()) else {
            return (None, false);
        };
        if pw.attribute("storeAsClearText") == Some("True") {
            return (Some(crate::Secret::new(text.to_string())), false);
        }
        if self.cert_encrypted {
            return (None, true);
        }
        match base64::engine::general_purpose::STANDARD.decode(text) {
            Ok(blob) => match self.decrypt.decrypt(&blob) {
                Some(secret) => (Some(secret), false),
                None => (None, true),
            },
            Err(_) => (None, true),
        }
    }
}

/// Depth-first walk over groups and servers, emitting the bundle as it goes.
fn walk(
    node: Node,
    group_path: Option<&str>,
    creds: Option<usize>,
    settings: &EffNodes,
    file_profiles: &[(String, ImportedCredential, bool)],
    ctx: &mut Ctx,
) {
    for child in node.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "group" => {
                let name = find_text(child, "name").unwrap_or_else(|| "(unnamed group)".into());
                let path = match group_path {
                    Some(parent) => format!("{parent} / {name}"),
                    None => name,
                };
                let child_creds = ctx.resolve_creds(child, creds, file_profiles);
                let child_settings = EffNodes::at(child, settings);
                ctx.bundle.groups.push(ImportedGroup {
                    path: path.clone(),
                    credential: child_creds,
                    comment: find_text(child, "comment"),
                });
                walk(
                    child,
                    Some(&path),
                    child_creds,
                    &child_settings,
                    file_profiles,
                    ctx,
                );
            }
            "server" => {
                let host = build_host(child, group_path, creds, settings, file_profiles, ctx);
                ctx.bundle.hosts.push(host);
            }
            "smartGroup" => {
                let name = find_text(child, "name").unwrap_or_else(|| "(unnamed)".into());
                ctx.bundle.skipped.push((
                    format!("smart group '{name}'"),
                    "dynamic groups are rule-based and are not imported".into(),
                ));
            }
            _ => {}
        }
    }
}

/// Turn one `<server>` into a fully resolved host.
fn build_host(
    node: Node,
    group_path: Option<&str>,
    parent_creds: Option<usize>,
    parent_settings: &EffNodes,
    file_profiles: &[(String, ImportedCredential, bool)],
    ctx: &mut Ctx,
) -> ImportedHost {
    let raw_name = find_text(node, "name").unwrap_or_default();
    let (address, addr_port) = split_host_port(&raw_name);
    let display = find_text(node, "displayName").unwrap_or_else(|| raw_name.clone());

    let settings_nodes = EffNodes::at(node, parent_settings);

    // Port: connectionSettings wins over a ":port" in the address.
    let mut port = addr_port.unwrap_or(DEFAULT_PORT);
    if let Some(cs) = settings_nodes.connection {
        if let Some(p) = find_text(cs, "port").and_then(|t| t.parse().ok()) {
            port = p;
        }
    }

    // Host credential: its own when it overrides; when it inherits, None inside
    // a group (it will inherit from the group at connect time) but the file's
    // credentials attached directly when it is a top-level server.
    let credential = match ctx.logon_spec(find_child(node, "logonCredentials")) {
        CredSpec::Inherit => {
            if group_path.is_some() {
                None
            } else {
                parent_creds
            }
        }
        _ => ctx.own_creds(node, file_profiles),
    };

    let (settings, extras) = settings_nodes.build(ctx);

    ImportedHost {
        name: display,
        address,
        port,
        group_path: group_path.map(str::to_string),
        credential,
        comment: find_text(node, "comment"),
        settings,
        extras,
    }
}

/// The nearest node that defines each settings category, resolved by
/// inheritance. Parsing (both the mapped fields and the extras) happens once,
/// at the host, from whichever node won.
#[derive(Default, Clone, Copy)]
struct EffNodes<'a, 'input> {
    remote_desktop: Option<Node<'a, 'input>>,
    connection: Option<Node<'a, 'input>>,
    local_resources: Option<Node<'a, 'input>>,
    gateway: Option<Node<'a, 'input>>,
    security: Option<Node<'a, 'input>>,
    display_settings: Option<Node<'a, 'input>>,
}

impl<'a, 'input> EffNodes<'a, 'input> {
    /// Fold one node's settings over the inherited ones.
    fn at(node: Node<'a, 'input>, parent: &EffNodes<'a, 'input>) -> Self {
        EffNodes {
            remote_desktop: effective(node, "remoteDesktop", parent.remote_desktop),
            connection: effective(node, "connectionSettings", parent.connection),
            local_resources: effective(node, "localResources", parent.local_resources),
            gateway: effective(node, "gatewaySettings", parent.gateway),
            security: effective(node, "securitySettings", parent.security),
            display_settings: effective(node, "displaySettings", parent.display_settings),
        }
    }

    /// Build the mapped settings and the leftover extras for a host.
    fn build(&self, ctx: &mut Ctx) -> (ImportedSettings, Vec<(String, String)>) {
        let mut extras = Vec::new();
        let mut settings = ImportedSettings::default();

        if let Some(rd) = self.remote_desktop {
            settings.display = parse_display(rd);
            settings.color_depth = find_text(rd, "colorDepth").and_then(|t| t.parse().ok());
            dump_extras(
                rd,
                &["colorDepth", "size", "fullScreen", "sameSizeAsClientArea"],
                &mut extras,
            );
        }
        if let Some(cs) = self.connection {
            settings.admin = find_bool(cs, "connectToConsole");
            dump_extras(cs, &["connectToConsole", "port"], &mut extras);
        }
        if let Some(lr) = self.local_resources {
            settings.audio = parse_audio(lr);
            settings.clipboard = find_bool(lr, "redirectClipboard");
            dump_extras(lr, &["audioRedirection", "redirectClipboard"], &mut extras);
        }
        if let Some(gw) = self.gateway {
            settings.gateway = parse_gateway(gw, ctx);
        }
        if let Some(sec) = self.security {
            dump_extras(sec, &[], &mut extras);
        }
        if let Some(ds) = self.display_settings {
            dump_extras(ds, &[], &mut extras);
        }
        (settings, extras)
    }
}

/// Whether `node`'s `tag` child is effective here or inherited from `parent`.
fn effective<'a, 'input>(
    node: Node<'a, 'input>,
    tag: &str,
    parent: Option<Node<'a, 'input>>,
) -> Option<Node<'a, 'input>> {
    match find_child(node, tag) {
        None => parent,
        Some(e) if inherits_from_parent(e) => parent,
        Some(e) => Some(e),
    }
}

fn parse_display(rd: Node) -> Option<ImportedDisplay> {
    if find_bool(rd, "fullScreen") == Some(true) {
        return Some(ImportedDisplay::FullScreen);
    }
    if find_bool(rd, "sameSizeAsClientArea") == Some(true) {
        return Some(ImportedDisplay::FitWindow);
    }
    let size = find_text(rd, "size")?;
    let (w, h) = size.split_once('x').or_else(|| size.split_once('X'))?;
    Some(ImportedDisplay::Fixed {
        width: w.trim().parse().ok()?,
        height: h.trim().parse().ok()?,
    })
}

fn parse_audio(lr: Node) -> Option<ImportedAudio> {
    match find_text(lr, "audioRedirection")?.as_str() {
        "0" | "Client" => Some(ImportedAudio::Local),
        "1" | "Remote" => Some(ImportedAudio::Remote),
        "2" | "NoSound" | "None" => Some(ImportedAudio::Off),
        _ => None,
    }
}

fn parse_gateway(gw: Node, ctx: &mut Ctx) -> Option<ImportedGateway> {
    if find_bool(gw, "enabled") != Some(true) {
        return None;
    }
    let address = find_text(gw, "hostName")?;
    let (cred, failed) = ctx.parse_inline(gw, None);
    ctx.pw_failed |= failed;
    let credential = ctx.bundle.intern_credential(cred);
    Some(ImportedGateway {
        address,
        port: find_text(gw, "port").and_then(|t| t.parse().ok()),
        credential,
        use_host_credentials: find_bool(gw, "credSharing").unwrap_or(false),
        bypass_local: find_bool(gw, "localBypass").unwrap_or(false),
    })
}

/// Parse the file's `<credentialsProfiles>` into `(name, credential, pw_failed)`.
fn collect_file_profiles(file: Node, ctx: &mut Ctx) -> Vec<(String, ImportedCredential, bool)> {
    let mut out = Vec::new();
    let Some(profiles) = find_child(file, "credentialsProfiles") else {
        return out;
    };
    for node in profiles
        .children()
        .filter(|n| n.has_tag_name("credentialsProfile"))
    {
        let (cred, failed) = parse_credentials_node(node, ctx.decrypt, ctx.cert_encrypted);
        if let Some(name) = cred.label.clone() {
            out.push((name, cred, failed));
        }
    }
    out
}

/// Parse a standalone credentials element (`credentialsProfile` in a file or in
/// `RDCMan.settings`). Shared with [`crate::settings`].
pub(crate) fn parse_credentials_node(
    node: Node,
    decrypt: &dyn Decrypt,
    cert_encrypted: bool,
) -> (ImportedCredential, bool) {
    let ctx_pw = read_password_static(node, decrypt, cert_encrypted);
    let label = find_text(node, "profileName").filter(|n| n != "Custom");
    let cred = ImportedCredential {
        label,
        username: find_text(node, "userName"),
        domain: find_text(node, "domain"),
        password: ctx_pw.0,
    };
    (cred, ctx_pw.1)
}

/// Password decode without a [`Ctx`], for [`parse_credentials_node`].
fn read_password_static(
    node: Node,
    decrypt: &dyn Decrypt,
    cert_encrypted: bool,
) -> (Option<crate::Secret>, bool) {
    let Some(pw) = find_child(node, "password") else {
        return (None, false);
    };
    let Some(text) = pw.text().map(str::trim).filter(|t| !t.is_empty()) else {
        return (None, false);
    };
    if pw.attribute("storeAsClearText") == Some("True") {
        return (Some(crate::Secret::new(text.to_string())), false);
    }
    if cert_encrypted {
        return (None, true);
    }
    match base64::engine::general_purpose::STANDARD.decode(text) {
        Ok(blob) => match decrypt.decrypt(&blob) {
            Some(secret) => (Some(secret), false),
            None => (None, true),
        },
        Err(_) => (None, true),
    }
}

// -- small XML and value helpers -------------------------------------------

/// Find a child element by tag, looking directly under `node` first and then
/// inside a `<properties>` child. RDCMan 2.2 and 2.7 disagree on which, so both
/// are accepted.
fn find_child<'a, 'input>(node: Node<'a, 'input>, tag: &str) -> Option<Node<'a, 'input>> {
    node.children()
        .find(|c| c.is_element() && c.has_tag_name(tag))
        .or_else(|| {
            node.children()
                .find(|c| c.is_element() && c.has_tag_name("properties"))
                .and_then(|p| p.children().find(|c| c.is_element() && c.has_tag_name(tag)))
        })
}

/// Trimmed, non-empty text of a child element.
fn find_text(node: Node, tag: &str) -> Option<String> {
    find_child(node, tag)
        .and_then(|c| c.text())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// A `<tag>True/False</tag>` child as a bool.
fn find_bool(node: Node, tag: &str) -> Option<bool> {
    match find_text(node, tag)?.as_str() {
        "True" | "true" | "1" => Some(true),
        "False" | "false" | "0" => Some(false),
        _ => None,
    }
}

/// Whether a settings/credentials element defers to its parent.
fn inherits_from_parent(node: Node) -> bool {
    node.attribute("inherit") == Some("FromParent")
}

/// A `<profileName scope="Local">` reference points at the app's own profiles.
fn profile_is_local(node: Node) -> bool {
    find_child(node, "profileName").and_then(|p| p.attribute("scope")) == Some("Local")
}

/// Split `"host:3389"` into `("host", Some(3389))`, leaving IPv6/other text as
/// an address with no port.
fn split_host_port(raw: &str) -> (String, Option<u16>) {
    if let Some((host, port)) = raw.rsplit_once(':') {
        if let Ok(p) = port.trim().parse::<u16>() {
            if !host.is_empty() && !host.contains(':') {
                return (host.trim().to_string(), Some(p));
            }
        }
    }
    (raw.trim().to_string(), None)
}

/// Push every child element not in `mapped` into `extras`, so a recognised but
/// unmapped setting stays visible in the preview.
fn dump_extras(node: Node, mapped: &[&str], extras: &mut Vec<(String, String)>) {
    for child in node.children().filter(Node::is_element) {
        let tag = child.tag_name().name();
        if mapped.contains(&tag) {
            continue;
        }
        if let Some(text) = child.text().map(str::trim).filter(|t| !t.is_empty()) {
            extras.push((tag.to_string(), text.to_string()));
        }
    }
}

/// Copy a credential, cloning the wiped-on-drop password wrapper.
fn clone_cred(cred: &ImportedCredential) -> ImportedCredential {
    ImportedCredential {
        label: cred.label.clone(),
        username: cred.username.clone(),
        domain: cred.domain.clone(),
        password: cred.password.clone(),
    }
}
