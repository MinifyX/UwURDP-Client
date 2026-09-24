//! End-to-end tests over anonymised fixtures.
//!
//! Passwords in the fixtures are sealed with a stand-in for DPAPI: the UTF-16LE
//! plaintext XORed with 0x5A, base64 in `.rdg` files and hex in `.rdp` files.
//! [`XorDecrypt`] reverses it, so the tests exercise the real decode path
//! (base64/hex → `Decrypt` → UTF-16LE → trim NULs) without needing Windows.

use uwurdp_import::{
    parse_rdg, parse_rdp_file, Decrypt, ImportedAudio, ImportedCredential, ImportedDisplay,
    ImportedGateway, NamedCredential, PasswordOrigin, PasswordRecipient, RecipientKind, Secret,
    Source,
};

/// The fake cipher that mirrors how the fixtures were sealed.
struct XorDecrypt;

impl Decrypt for XorDecrypt {
    fn decrypt(&self, blob: &[u8]) -> Option<Secret> {
        let raw: Vec<u8> = blob.iter().map(|b| b ^ 0x5A).collect();
        let (pairs, _) = raw.as_chunks::<2>();
        let units: Vec<u16> = pairs
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .take_while(|u| *u != 0)
            .collect();
        Some(Secret::new(String::from_utf16_lossy(&units)))
    }
}

/// A decryptor that never succeeds, to check the drop-password path.
struct NoDecrypt;
impl Decrypt for NoDecrypt {
    fn decrypt(&self, _blob: &[u8]) -> Option<Secret> {
        None
    }
}

fn pw(cred: &ImportedCredential) -> Option<&str> {
    cred.password.as_deref().map(|s| s.as_str())
}

fn local_profiles() -> Vec<NamedCredential> {
    vec![NamedCredential {
        name: "corp-admin".into(),
        credential: ImportedCredential {
            label: Some("corp-admin".into()),
            username: Some("administrator".into()),
            domain: Some("CORP".into()),
            password: Some(Secret::new("L0calProf".into())),
            ..Default::default()
        },
    }]
}

// -- RDCMan 2.7 -------------------------------------------------------------

#[test]
fn rdg_27_full_tree() {
    let xml = include_str!("fixtures/rdcman_2.7.rdg");
    let profiles = local_profiles();
    let bundle = parse_rdg(xml, &profiles, &XorDecrypt).expect("parse");

    assert_eq!(bundle.source, Some(Source::RdcMan));

    // Groups, nested and flattened.
    let group_paths: Vec<&str> = bundle.groups.iter().map(|g| g.path.as_str()).collect();
    assert_eq!(group_paths, ["Datacenter", "Datacenter / Rack A"]);

    // Datacenter has its own creds; Rack A inherits them (same interned index).
    let dc = &bundle.groups[0];
    let rack = &bundle.groups[1];
    assert!(dc.credential.is_some());
    assert_eq!(dc.credential, rack.credential);
    assert_eq!(
        bundle.credentials[dc.credential.unwrap()]
            .username
            .as_deref(),
        Some("dcadmin")
    );
    assert_eq!(
        pw(&bundle.credentials[dc.credential.unwrap()]),
        Some("Gr0up#Pass")
    );

    // Smart group is skipped, with a note.
    assert!(bundle
        .skipped
        .iter()
        .any(|(what, _)| what.contains("smart group")));
    assert!(bundle
        .skipped
        .iter()
        .all(|(what, _)| !what.contains("decrypt")));

    let host = |name: &str| bundle.hosts.iter().find(|h| h.name == name).unwrap();

    // edge: directly under the file, inherits -> file creds attached to it, and
    // the port comes from the ":3390" in the address.
    let edge = host("edge");
    assert_eq!(edge.address, "10.0.0.10");
    assert_eq!(edge.port, 3390);
    assert_eq!(edge.group_path, None);
    let edge_cred = edge.credential.expect("edge inherits file creds directly");
    assert_eq!(
        bundle.credentials[edge_cred].username.as_deref(),
        Some("fileadmin")
    );
    assert_eq!(pw(&bundle.credentials[edge_cred]), Some("S3cr3t-File!"));

    // db1: references the File-scope "svc" profile.
    let db1 = host("db1");
    assert_eq!(db1.group_path.as_deref(), Some("Datacenter"));
    let svc = &bundle.credentials[db1.credential.unwrap()];
    assert_eq!(svc.username.as_deref(), Some("svc-deploy"));
    assert_eq!(pw(svc), Some("Prof#27pw"));

    // app1: inherits inside a group, so it carries no credential of its own
    // (it will inherit the group's at connect time). Settings fully resolved.
    let app1 = host("app1");
    assert_eq!(app1.group_path.as_deref(), Some("Datacenter / Rack A"));
    assert_eq!(app1.credential, None);
    assert_eq!(
        app1.settings.display,
        Some(ImportedDisplay::Fixed {
            width: 1920,
            height: 1080
        })
    );
    assert_eq!(app1.settings.color_depth, Some(24));
    assert_eq!(app1.settings.audio, Some(ImportedAudio::Off));
    assert_eq!(app1.settings.clipboard, Some(false));
    let gw = app1.settings.gateway.as_ref().expect("gateway");
    assert_eq!(
        gw,
        &ImportedGateway {
            address: "gw.example.com".into(),
            port: Some(443),
            credential: gw.credential,
            use_host_credentials: true,
            bypass_local: true,
        }
    );
    let gwcred = &bundle.credentials[gw.credential.unwrap()];
    assert_eq!(gwcred.username.as_deref(), Some("gwuser"));
    assert_eq!(pw(gwcred), Some("Gw@Secret9"));

    // app2: own inline creds; connectionSettings port overrides the address.
    let app2 = host("app2");
    assert_eq!(app2.address, "10.0.0.22");
    assert_eq!(app2.port, 3390);
    let app2_cred = &bundle.credentials[app2.credential.unwrap()];
    assert_eq!(app2_cred.username.as_deref(), Some("appadmin"));
    assert_eq!(pw(app2_cred), Some("H0st!Over"));

    // app3: resolves a Local-scope profile passed in by the app.
    let app3 = host("app3-local-profile");
    let app3_cred = &bundle.credentials[app3.credential.unwrap()];
    assert_eq!(app3_cred.username.as_deref(), Some("administrator"));
    assert_eq!(pw(app3_cred), Some("L0calProf"));

    // Unmapped-but-recognised settings land in extras, not lost.
    assert!(
        app1.extras
            .iter()
            .any(|(k, _)| k == "redirectDrives" || k == "authentication")
            || edge.extras.iter().any(|(k, _)| k == "loadBalanceInfo")
    );
}

#[test]
fn rdg_27_inherited_display_from_file() {
    // A host that overrides nothing takes the file's "same size as client area".
    let xml = include_str!("fixtures/rdcman_2.7.rdg");
    let profiles = local_profiles();
    let bundle = parse_rdg(xml, &profiles, &XorDecrypt).unwrap();
    let edge = bundle.hosts.iter().find(|h| h.name == "edge").unwrap();
    assert_eq!(edge.settings.display, Some(ImportedDisplay::FitWindow));
    assert_eq!(edge.settings.color_depth, Some(32));
    assert_eq!(edge.settings.audio, Some(ImportedAudio::Local));
}

// -- RDCMan 2.2 -------------------------------------------------------------

#[test]
fn rdg_22_flat_layout_and_cleartext() {
    let xml = include_str!("fixtures/rdcman_2.2.rdg");
    let bundle = parse_rdg(xml, &[], &XorDecrypt).unwrap();

    assert_eq!(bundle.groups.len(), 1);
    assert_eq!(bundle.groups[0].path, "Servers");

    // Server directly under the file: name/displayName/comment are direct
    // children in 2.2, and it inherits the file's clear-text credentials.
    let direct = bundle
        .hosts
        .iter()
        .find(|h| h.name == "legacy-direct")
        .unwrap();
    assert_eq!(direct.address, "10.0.0.5");
    assert_eq!(direct.group_path, None);
    let dcred = &bundle.credentials[direct.credential.unwrap()];
    assert_eq!(dcred.username.as_deref(), Some("rootuser"));
    assert_eq!(pw(dcred), Some("plaintextpw"));

    // Server in the group with its own DPAPI password and full-screen display,
    // all with settings sitting directly under <server>.
    let host7 = bundle.hosts.iter().find(|h| h.name == "host7").unwrap();
    assert_eq!(host7.group_path.as_deref(), Some("Servers"));
    assert_eq!(host7.address, "host7.example.com");
    assert_eq!(host7.settings.display, Some(ImportedDisplay::FullScreen));
    assert_eq!(host7.settings.admin, Some(true));
    assert_eq!(host7.settings.audio, Some(ImportedAudio::Remote));
    let hcred = &bundle.credentials[host7.credential.unwrap()];
    assert_eq!(pw(hcred), Some("H0st!Over"));
}

// -- certificate encryption -------------------------------------------------

#[test]
fn rdg_certificate_encrypted_drops_passwords() {
    let xml = include_str!("fixtures/rdcman_cert.rdg");
    // Even a working decryptor must not be used: the blob is not a DPAPI blob.
    let bundle = parse_rdg(xml, &[], &XorDecrypt).unwrap();

    let secure = &bundle.hosts[0];
    let cred = &bundle.credentials[secure.credential.unwrap()];
    assert_eq!(cred.username.as_deref(), Some("certuser"));
    assert_eq!(cred.domain.as_deref(), Some("CORP"));
    assert_eq!(pw(cred), None, "password must be dropped");
    assert!(bundle
        .skipped
        .iter()
        .any(|(what, why)| what.contains("passwords") && why.contains("certificate")));
}

#[test]
fn rdg_failed_decrypt_keeps_username() {
    let xml = include_str!("fixtures/rdcman_2.7.rdg");
    let profiles = local_profiles();
    let bundle = parse_rdg(xml, &profiles, &NoDecrypt).unwrap();
    // Usernames survive, DPAPI passwords are gone, one note explains it.
    let edge = bundle.hosts.iter().find(|h| h.name == "edge").unwrap();
    let cred = &bundle.credentials[edge.credential.unwrap()];
    assert_eq!(cred.username.as_deref(), Some("fileadmin"));
    assert_eq!(pw(cred), None);
    assert!(bundle
        .skipped
        .iter()
        .any(|(what, _)| what.contains("passwords could not be decrypted")));
}

// -- .rdp files -------------------------------------------------------------

#[test]
fn rdp_utf8() {
    let bytes = include_bytes!("fixtures/sample_utf8.rdp");
    let bundle = parse_rdp_file(bytes, "prod-web", &XorDecrypt).unwrap();
    assert_eq!(bundle.source, Some(Source::RdpFile));
    assert_eq!(bundle.hosts.len(), 1);

    let host = &bundle.hosts[0];
    assert_eq!(host.name, "prod-web");
    assert_eq!(host.address, "10.0.0.50");
    assert_eq!(host.port, 3391);
    assert_eq!(host.settings.display, Some(ImportedDisplay::FullScreen));
    assert_eq!(host.settings.color_depth, Some(24));
    assert_eq!(host.settings.admin, Some(true));
    assert_eq!(host.settings.audio, Some(ImportedAudio::Remote));
    assert_eq!(host.settings.clipboard, Some(true));

    // DOMAIN\user split.
    let cred = &bundle.credentials[host.credential.unwrap()];
    assert_eq!(cred.username.as_deref(), Some("alice"));
    assert_eq!(cred.domain.as_deref(), Some("CORP"));
    assert_eq!(pw(cred), Some("Rdp#File1"));

    // Gateway with local bypass (usage method 2).
    let gw = host.settings.gateway.as_ref().unwrap();
    assert_eq!(gw.address, "gw2.example.com");
    assert!(gw.bypass_local);

    // Noise dropped, unknown keys kept.
    assert!(host.extras.iter().all(|(k, _)| k != "winposstr"));
    assert!(host
        .extras
        .iter()
        .any(|(k, _)| k == "autoreconnection enabled"));
}

#[test]
fn rdp_utf16le_bom() {
    let bytes = include_bytes!("fixtures/sample_utf16.rdp");
    let bundle = parse_rdp_file(bytes, "host-uni", &XorDecrypt).unwrap();

    let host = &bundle.hosts[0];
    assert_eq!(host.name, "host-uni");
    assert_eq!(host.address, "host-uni.example.com");
    assert_eq!(host.port, 3389);
    // smart sizing / dynamic resolution -> fit the window.
    assert_eq!(host.settings.display, Some(ImportedDisplay::FitWindow));
    assert_eq!(host.settings.audio, Some(ImportedAudio::Local));
    assert_eq!(host.settings.clipboard, Some(false));

    // user@domain passes through as the username (a UPN).
    let cred = &bundle.credentials[host.credential.unwrap()];
    assert_eq!(cred.username.as_deref(), Some("bob@corp.example.com"));
    assert_eq!(pw(cred), Some("Rdp#File1"));
}

// -- passwords this Windows account opened ------------------------------------

/// Seal `text` the way the fixtures are sealed, as base64 for an `.rdg`.
fn seal(text: &str) -> String {
    use base64::Engine;
    let raw: Vec<u8> = text
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .map(|b| b ^ 0x5A)
        .collect();
    base64::engine::general_purpose::STANDARD.encode(raw)
}

/// A file that names the user's own Local profile "Admin" for one server, has
/// a group with a sealed password, and a server with a clear-text one.
fn borrowing_rdg() -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<RDCMan programVersion="2.7" schemaVersion="3">
  <file>
    <properties><name>Shared</name></properties>
    <server>
      <properties><name>192.0.2.10</name><displayName>borrower</displayName></properties>
      <logonCredentials inherit="None">
        <profileName scope="Local">Admin</profileName>
      </logonCredentials>
    </server>
    <server>
      <properties><name>192.0.2.30</name><displayName>plain</displayName></properties>
      <logonCredentials inherit="None">
        <profileName scope="Local">Custom</profileName>
        <userName>plainuser</userName>
        <password storeAsClearText="True">in-the-file</password>
      </logonCredentials>
    </server>
    <group>
      <properties>
        <name>Sealed</name>
        <logonCredentials inherit="None">
          <profileName scope="Local">Custom</profileName>
          <userName>groupuser</userName>
          <password>{}</password>
        </logonCredentials>
      </properties>
      <server>
        <properties><name>192.0.2.20:3390</name><displayName>member</displayName></properties>
        <logonCredentials inherit="FromParent" />
      </server>
    </group>
  </file>
</RDCMan>"#,
        seal("Gr0upSealed")
    )
}

fn admin_profile() -> Vec<NamedCredential> {
    vec![NamedCredential {
        name: "Admin".into(),
        credential: ImportedCredential {
            label: Some("Admin".into()),
            username: Some("admin".into()),
            password: Some(Secret::new("MyOwnSecret".into())),
            ..Default::default()
        },
    }]
}

#[test]
fn rdg_marks_where_each_password_came_from() {
    let bundle = parse_rdg(&borrowing_rdg(), &admin_profile(), &XorDecrypt).unwrap();
    let origin = |name: &str| {
        let host = bundle.hosts.iter().find(|h| h.name == name).unwrap();
        bundle.credentials[host.credential.unwrap()].origin
    };
    assert_eq!(origin("borrower"), PasswordOrigin::LocalProfile);
    assert_eq!(origin("plain"), PasswordOrigin::InFile);
    let group = &bundle.credentials[bundle.groups[0].credential.unwrap()];
    assert_eq!(group.origin, PasswordOrigin::Unsealed);
    assert_eq!(pw(group), Some("Gr0upSealed"));
}

#[test]
fn rdg_lists_every_address_that_would_get_the_users_passwords() {
    let bundle = parse_rdg(&borrowing_rdg(), &admin_profile(), &XorDecrypt).unwrap();
    let recipients = bundle.password_recipients();

    // The host that borrows the user's own profile, by address and profile.
    assert!(recipients.contains(&PasswordRecipient {
        kind: RecipientKind::Host,
        name: "borrower".into(),
        address: Some("192.0.2.10".into()),
        port: Some(3389),
        profile: Some("Admin".into()),
    }));
    // The group with a sealed password, and the host that inherits it.
    assert!(recipients.contains(&PasswordRecipient {
        kind: RecipientKind::Group,
        name: "Sealed".into(),
        address: None,
        port: None,
        profile: None,
    }));
    assert!(recipients.contains(&PasswordRecipient {
        kind: RecipientKind::Host,
        name: "member".into(),
        address: Some("192.0.2.20".into()),
        port: Some(3390),
        profile: None,
    }));
    // A clear-text password was the file's own to give.
    assert!(recipients.iter().all(|r| r.name != "plain"));
    assert_eq!(recipients.len(), 3);
}

#[test]
fn rdg_without_readable_passwords_lists_nobody() {
    // No profile to borrow and nothing DPAPI opens: usernames only.
    let bundle = parse_rdg(&borrowing_rdg(), &[], &NoDecrypt).unwrap();
    assert!(bundle.password_recipients().is_empty());
}

#[test]
fn rdp_password_51_is_the_users_own() {
    let bytes = include_bytes!("fixtures/sample_utf8.rdp");
    let bundle = parse_rdp_file(bytes, "prod-web", &XorDecrypt).unwrap();
    assert_eq!(bundle.credentials[0].origin, PasswordOrigin::Unsealed);
    let recipients = bundle.password_recipients();
    // The host, and the gateway that would be handed the host's login.
    assert_eq!(
        recipients
            .iter()
            .map(|r| (r.kind, r.address.as_deref()))
            .collect::<Vec<_>>(),
        [
            (RecipientKind::Host, Some("10.0.0.50")),
            (RecipientKind::Gateway, Some("gw2.example.com")),
        ]
    );
}
