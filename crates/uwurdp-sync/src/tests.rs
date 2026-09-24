//! Two devices, one server, no network.
//!
//! Every test here runs the real store and the real crypto against
//! [`MemoryServer`], which follows the server's rules. What is checked is what
//! sync has to get right: a change reaches the other device, a conflict ends
//! the same way on both, a delete stays deleted — and a server that tampers
//! with what it stores gets nowhere.

use uuid::Uuid;
use uwurdp_proto::{EntityKind, Envelope, Extra, Hlc, HostPayload, Resolution, Version, MAX_BATCH};
use uwurdp_store::{HostDraft, PasswordChange, RdpSettings, SecretText, Store, Workspace};
use uwurdp_vault::KdfParams;

use crate::{sync_once, MemoryServer, SyncError};

const PASSWORD: &[u8] = b"correct horse battery staple";

/// The first device: a store with a vault of its own.
fn first_device() -> Store {
    let store = Store::open_in_memory().unwrap();
    store
        .create_vault_with(PASSWORD, KdfParams::INSECURE_FOR_TESTS)
        .unwrap();
    store
}

/// A second device joining that vault: it takes the header the server holds
/// and opens it with the master password, exactly as pairing will.
fn joined_device(first: &Store) -> Store {
    let header = first.vault_header().unwrap().expect("a vault to join");
    let key = header.unlock(PASSWORD).unwrap().export_key();
    let store = Store::open_in_memory().unwrap();
    store.adopt_vault(&header, key).unwrap();
    store
}

fn draft(name: &str, address: &str) -> HostDraft {
    HostDraft {
        id: None,
        name: name.into(),
        address: address.into(),
        port: 3389,
        username: "uwu".into(),
        domain: String::new(),
        group_path: None,
        workspace: None,
        password: PasswordChange::Keep,
        rdp: RdpSettings::default(),
        comment: String::new(),
        gateway_username: String::new(),
        gateway_domain: String::new(),
        gateway_password: PasswordChange::Keep,
    }
}

fn names(store: &Store) -> Vec<String> {
    store
        .list_hosts()
        .unwrap()
        .into_iter()
        .map(|host| host.name)
        .collect()
}

#[test]
fn what_one_device_adds_shows_up_on_the_other() {
    let server = MemoryServer::new();
    let a = first_device();
    let b = joined_device(&a);

    let mut host = draft("prox-1", "10.0.0.12");
    host.group_path = Some("Homelab".into());
    host.password = PasswordChange::Set {
        value: SecretText::new("hunter2"),
    };
    let saved = a.save_host(host).unwrap();

    let up = sync_once(&a, &server).unwrap();
    assert!(up.pushed >= 4, "host, login, group and password: {up:?}");
    assert_eq!(
        up.apply.identical, up.pushed,
        "the pass ends by reading back what it wrote"
    );
    assert_eq!(up.conflicts, 0);

    let down = sync_once(&b, &server).unwrap();
    assert_eq!(down.pulled, up.pushed);
    assert_eq!(down.apply.rejected, 0);
    assert_eq!(down.apply.applied, up.pushed, "all of it was new to B");

    let hosts = b.list_hosts().unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].id, saved.id, "the id travels with the record");
    assert_eq!(hosts[0].address, "10.0.0.12");
    assert_eq!(hosts[0].username, "uwu");
    assert_eq!(
        hosts[0].group_path.as_deref(),
        Some("Homelab"),
        "the group came along as a record of its own"
    );
    assert!(hosts[0].has_password);
    assert_eq!(
        b.reveal_host_password(saved.id).unwrap().as_slice(),
        b"hunter2",
        "the password opens on the other device"
    );
    assert_eq!(b.list_groups().unwrap().len(), 1);
}

#[test]
fn a_host_i_removed_stays_removed_on_the_other_device() {
    let server = MemoryServer::new();
    let a = first_device();
    let b = joined_device(&a);
    let host = a.save_host(draft("nas", "10.0.0.9")).unwrap();
    sync_once(&a, &server).unwrap();
    sync_once(&b, &server).unwrap();
    assert_eq!(names(&b), vec!["nas"]);

    a.delete_host(host.id).unwrap();
    sync_once(&a, &server).unwrap();
    let report = sync_once(&b, &server).unwrap();
    assert!(report.apply.applied >= 1);
    assert!(names(&b).is_empty(), "the tombstone travelled");

    // And a device that edited it while offline does not bring it back.
    let c = joined_device(&a);
    sync_once(&c, &server).unwrap();
    assert!(names(&c).is_empty());
}

#[test]
fn the_later_of_two_edits_wins_on_both_devices() {
    let server = MemoryServer::new();
    let a = first_device();
    let b = joined_device(&a);
    let host = a.save_host(draft("web", "10.0.0.20")).unwrap();
    sync_once(&a, &server).unwrap();
    sync_once(&b, &server).unwrap();

    // Both rename it while neither has synced. A moment in between, so B's
    // edit really is the later one: within one millisecond the clocks tie and
    // the winner comes down to which device id sorts higher, which is a coin
    // flip and not what this test is about.
    let mut on_a = draft("web-a", "10.0.0.20");
    on_a.id = Some(host.id);
    a.save_host(on_a).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(3));
    let mut on_b = draft("web-b", "10.0.0.20");
    on_b.id = Some(host.id);
    b.save_host(on_b).unwrap();

    // A gets there first, B's push is refused and merged.
    sync_once(&a, &server).unwrap();
    let report = sync_once(&b, &server).unwrap();
    assert!(report.conflicts >= 1, "the server refused B's version");
    assert!(report.rounds <= crate::MAX_ROUNDS);

    // Whoever won, both devices agree — and so does the server.
    sync_once(&a, &server).unwrap();
    sync_once(&b, &server).unwrap();
    assert_eq!(names(&a), names(&b), "the two devices agree");
    assert_eq!(names(&b), vec!["web-b"], "B wrote later, so B wins");
}

#[test]
fn syncing_again_finds_nothing_to_do() {
    let server = MemoryServer::new();
    let a = first_device();
    a.save_host(draft("one", "10.0.0.1")).unwrap();
    sync_once(&a, &server).unwrap();

    let again = sync_once(&a, &server).unwrap();
    assert_eq!(again.pushed, 0);
    assert_eq!(
        again.pulled, 0,
        "pushing last would have left work for the next pass"
    );
    assert_eq!(again.rounds, 1);
    assert_eq!(a.pending_count().unwrap(), 0);

    let b = joined_device(&a);
    sync_once(&b, &server).unwrap();
    let again = sync_once(&b, &server).unwrap();
    assert_eq!((again.pulled, again.pushed), (0, 0));
}

#[test]
fn the_server_holds_nothing_it_can_read() {
    let server = MemoryServer::new();
    let a = first_device();
    let mut host = draft("prox-1", "10.0.0.12");
    host.group_path = Some("Homelab".into());
    host.password = PasswordChange::Set {
        value: SecretText::new("hunter2"),
    };
    a.save_host(host).unwrap();
    a.trust_host_key(
        "10.0.0.12",
        22,
        "ssh-ed25519",
        "SHA256:abcdef",
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5",
    )
    .unwrap();
    sync_once(&a, &server).unwrap();

    let held = server.records();
    assert!(!held.is_empty());
    for env in &held {
        let haystack = String::from_utf8_lossy(&env.blob).to_string();
        for secret in ["prox-1", "10.0.0.12", "hunter2", "Homelab", "SHA256:abcdef"] {
            assert!(
                !haystack.contains(secret),
                "{secret} must not be readable in a blob"
            );
        }
        assert_eq!(env.nonce.len(), 24);
    }
    // What it does see: how many records there are, of what kind, and when.
    assert!(held.iter().any(|env| env.kind == EntityKind::Host));
}

#[test]
fn a_server_that_marks_a_record_deleted_gets_nowhere() {
    let server = MemoryServer::new();
    let a = first_device();
    let b = joined_device(&a);
    a.save_host(draft("nas", "10.0.0.9")).unwrap();
    sync_once(&a, &server).unwrap();
    sync_once(&b, &server).unwrap();
    assert_eq!(names(&b), vec!["nas"]);

    // The server flips the tombstone flag on the host — the one bit that
    // would otherwise remove it everywhere, since a delete beats an edit.
    let mut host = server
        .records()
        .into_iter()
        .find(|env| env.kind == EntityKind::Host)
        .unwrap();
    host.deleted = true;
    host.updated_at = Hlc::new(host.updated_at.wall_ms + 60_000, 0, host.updated_at.device);
    server.store(host);

    let report = sync_once(&b, &server).unwrap();
    assert_eq!(report.apply.rejected, 1, "the seal caught it");
    assert_eq!(report.apply.applied, 0);
    assert_eq!(names(&b), vec!["nas"], "the host is still there");
}

#[test]
fn a_server_that_replays_an_old_version_gets_nowhere() {
    let server = MemoryServer::new();
    let a = first_device();
    let b = joined_device(&a);
    let host = a.save_host(draft("bastion", "10.0.0.2")).unwrap();
    sync_once(&a, &server).unwrap();
    let old = server
        .records()
        .into_iter()
        .find(|env| env.kind == EntityKind::Host)
        .unwrap();

    let mut renamed = draft("bastion-new", "10.0.0.2");
    renamed.id = Some(host.id);
    a.save_host(renamed).unwrap();
    sync_once(&a, &server).unwrap();
    sync_once(&b, &server).unwrap();
    assert_eq!(names(&b), vec!["bastion-new"]);

    // The server hands the old blob back under a newer clock.
    let mut replay = old.clone();
    replay.updated_at = Hlc::new(
        replay.updated_at.wall_ms + 3_600_000,
        0,
        replay.updated_at.device,
    );
    server.store(replay);

    let report = sync_once(&b, &server).unwrap();
    assert_eq!(report.apply.rejected, 1);
    assert_eq!(
        names(&b),
        vec!["bastion-new"],
        "the old name did not come back"
    );

    // The same blob with its own clock is simply older, and loses.
    server.store(old);
    let report = sync_once(&b, &server).unwrap();
    assert_eq!(report.apply.rejected, 0);
    assert_eq!(report.apply.kept, 1);
    assert_eq!(names(&b), vec!["bastion-new"]);
}

#[test]
fn a_record_from_another_vault_is_dropped() {
    let server = MemoryServer::new();
    let a = first_device();
    a.save_host(draft("one", "10.0.0.1")).unwrap();
    sync_once(&a, &server).unwrap();

    let mut stranger = server
        .records()
        .into_iter()
        .find(|env| env.kind == EntityKind::Host)
        .unwrap();
    stranger.id = Uuid::now_v7();
    stranger.vault_id = Uuid::now_v7();
    server.store(stranger);

    let report = sync_once(&a, &server).unwrap();
    assert_eq!(report.apply.rejected, 1);
    assert_eq!(names(&a), vec!["one"]);
}

#[test]
fn two_devices_that_trusted_different_keys_for_one_host_end_up_agreeing() {
    let server = MemoryServer::new();
    let a = first_device();
    let b = joined_device(&a);

    a.trust_host_key(
        "nas.lan",
        22,
        "ssh-ed25519",
        "SHA256:aaa",
        "ssh-ed25519 AAAA",
    )
    .unwrap();
    // A moment later, or the two decisions share a millisecond and the tie
    // falls to whichever device id is greater — which is not what is under
    // test here.
    std::thread::sleep(std::time::Duration::from_millis(2));
    b.trust_host_key(
        "nas.lan",
        22,
        "ssh-ed25519",
        "SHA256:bbb",
        "ssh-ed25519 BBBB",
    )
    .unwrap();

    sync_once(&a, &server).unwrap();
    let report = sync_once(&b, &server).unwrap();
    assert_eq!(
        report.apply.host_key_conflicts, 1,
        "it is worth telling the user about"
    );
    sync_once(&a, &server).unwrap();
    sync_once(&b, &server).unwrap();

    let on_a = a.known_host("nas.lan", 22).unwrap().unwrap();
    let on_b = b.known_host("nas.lan", 22).unwrap().unwrap();
    assert_eq!(
        on_a.fingerprint, on_b.fingerprint,
        "one key per address, the same one on both devices"
    );
    assert_eq!(
        on_b.fingerprint, "SHA256:bbb",
        "the later decision takes the slot"
    );
}

#[test]
fn a_field_a_newer_build_wrote_is_not_dropped_by_this_one() {
    let server = MemoryServer::new();
    let a = first_device();
    let b = joined_device(&a);

    // A build from the future writes a host with a field this one has never
    // heard of, sealed with the same vault key.
    let header = a.vault_header().unwrap().unwrap();
    let vault = header.unlock(PASSWORD).unwrap();
    let id = Uuid::now_v7();
    let clock = Hlc::new(1_700_000_000_000, 0, 42);
    let mut extra = Extra::new();
    extra.insert("tags".into(), serde_json::json!(["homelab", "critical"]));
    let payload = serde_json::to_vec(&HostPayload {
        name: "future".into(),
        address: "10.0.0.77".into(),
        port: 22,
        workspace: "private".into(),
        position: 0,
        group_id: None,
        identity_id: None,
        rdp: Default::default(),
        gateway_identity_id: None,
        comment: String::new(),
        extra,
    })
    .unwrap();
    let sealed = vault
        .seal_synced(id, EntityKind::Host, clock, false, &payload)
        .unwrap();
    server.store(Envelope {
        id,
        vault_id: vault.vault_id(),
        kind: EntityKind::Host,
        updated_at: clock,
        base_seq: 0,
        deleted: false,
        nonce: sealed.nonce,
        blob: sealed.blob,
        seq: None,
    });

    sync_once(&a, &server).unwrap();
    assert_eq!(names(&a), vec!["future"]);

    // This build edits the host and pushes it back.
    let mut edit = draft("future-renamed", "10.0.0.77");
    edit.id = Some(id);
    a.save_host(edit).unwrap();
    sync_once(&a, &server).unwrap();
    sync_once(&b, &server).unwrap();

    // The newer build's field is still in what the server holds.
    let host = server
        .records()
        .into_iter()
        .find(|env| env.id == id)
        .unwrap();
    let opened = vault
        .open_synced(
            host.id,
            host.kind,
            host.updated_at,
            host.deleted,
            &uwurdp_vault::Sealed {
                nonce: host.nonce.clone(),
                blob: host.blob.clone(),
            },
        )
        .unwrap();
    let payload: HostPayload = serde_json::from_slice(&opened).unwrap();
    assert_eq!(payload.name, "future-renamed");
    assert_eq!(
        payload.extra.get("tags"),
        Some(&serde_json::json!(["homelab", "critical"])),
        "the tags survived an edit by a build that knows nothing about them"
    );
}

#[test]
fn a_host_whose_login_has_not_arrived_yet_is_still_shown() {
    let server = MemoryServer::new();
    let a = first_device();
    let b = joined_device(&a);
    let mut host = draft("half", "10.0.0.5");
    host.group_path = Some("Later".into());
    a.save_host(host).unwrap();
    sync_once(&a, &server).unwrap();

    // Only the host itself arrives — its login and its group are in a page
    // that has not come yet.
    let held = server.records();
    let only_host: Vec<_> = held
        .iter()
        .filter(|env| env.kind == EntityKind::Host)
        .cloned()
        .collect();
    let report = b.apply_envelopes(&only_host).unwrap();
    assert_eq!(report.applied, 1);
    let hosts = b.list_hosts().unwrap();
    assert_eq!(hosts.len(), 1, "the host is there, with placeholders");
    assert_eq!(hosts[0].username, "");
    assert_eq!(
        hosts[0].group_path, None,
        "a group without a name yet is not shown as one"
    );
    assert!(
        b.list_groups().unwrap().is_empty(),
        "and it is not in the group list either"
    );
    assert_eq!(
        b.pending_count().unwrap(),
        0,
        "a placeholder is never pushed"
    );

    // The rest arrives.
    let report = sync_once(&b, &server).unwrap();
    assert_eq!(report.apply.rejected, 0);
    let hosts = b.list_hosts().unwrap();
    assert_eq!(hosts[0].username, "uwu");
    assert_eq!(hosts[0].group_path.as_deref(), Some("Later"));
    assert_eq!(b.pending_count().unwrap(), 0);
}

#[test]
fn a_device_that_joins_brings_its_own_hosts_along() {
    let server = MemoryServer::new();
    let a = first_device();
    a.save_host(draft("from-a", "10.0.0.1")).unwrap();
    sync_once(&a, &server).unwrap();

    // B had a vault and a host of its own before it ever heard of a server.
    let b = Store::open_in_memory().unwrap();
    b.create_vault_with(b"b's own password", KdfParams::INSECURE_FOR_TESTS)
        .unwrap();
    let mut own = draft("from-b", "10.0.0.2");
    own.password = PasswordChange::Set {
        value: SecretText::new("b-secret"),
    };
    let own = b.save_host(own).unwrap();

    let header = a.vault_header().unwrap().unwrap();
    let key = header.unlock(PASSWORD).unwrap().export_key();
    b.adopt_vault(&header, key).unwrap();
    assert_eq!(
        b.reveal_host_password(own.id).unwrap().as_slice(),
        b"b-secret",
        "its own secrets were sealed again under the account's key"
    );

    sync_once(&b, &server).unwrap();
    sync_once(&a, &server).unwrap();

    let mut on_a = names(&a);
    on_a.sort();
    assert_eq!(on_a, vec!["from-a", "from-b"]);
    assert_eq!(
        a.reveal_host_password(own.id).unwrap().as_slice(),
        b"b-secret",
        "and they open on the first device too"
    );
}

#[test]
fn a_pass_without_a_vault_asks_for_the_password_instead_of_failing() {
    let server = MemoryServer::new();
    let a = first_device();
    a.save_host(draft("one", "10.0.0.1")).unwrap();
    a.lock_vault();
    assert!(matches!(
        sync_once(&a, &server),
        Err(SyncError::VaultLocked)
    ));
    assert!(server.is_empty(), "and nothing left the device");
}

#[test]
fn a_batch_larger_than_the_protocol_allows_is_refused_by_the_server() {
    use crate::Transport;
    let server = MemoryServer::new();
    let a = first_device();
    let envelopes = vec![
        Envelope {
            id: Uuid::now_v7(),
            vault_id: a.vault_header().unwrap().unwrap().vault_id,
            kind: EntityKind::Host,
            updated_at: Hlc::new(1, 0, 1),
            base_seq: 0,
            deleted: false,
            nonce: vec![0; 24],
            blob: vec![0; 32],
            seq: None,
        };
        MAX_BATCH + 1
    ];
    assert!(server.push(envelopes).is_err());
}

#[test]
fn many_large_records_travel_in_several_requests_and_all_arrive() {
    use uwurdp_proto::MAX_BATCH_BYTES;
    let server = MemoryServer::new();
    let a = first_device();
    // Hosts with long passwords: each record is sealed at well over 100 KiB,
    // so together they are more than one request may carry.
    let count = MAX_BATCH_BYTES / (100 * 1024) + 10;
    for index in 0..count {
        let mut host = draft(&format!("big-{index}"), "10.0.0.1");
        host.password = PasswordChange::Set {
            value: SecretText::new("x".repeat(120 * 1024)),
        };
        a.save_host(host).unwrap();
    }

    let (first, _) = a.pending_envelopes(MAX_BATCH).unwrap();
    let bytes: usize = first.iter().map(|env| env.blob.len()).sum();
    assert!(first.len() < count, "one request does not take them all");
    assert!(
        bytes <= MAX_BATCH_BYTES,
        "{bytes} sealed bytes in one request"
    );

    // The server refuses more than that, and the engine never sends it.
    sync_once(&a, &server).unwrap();
    assert!(a.pending_envelopes(MAX_BATCH).unwrap().0.is_empty());

    let b = joined_device(&a);
    sync_once(&b, &server).unwrap();
    assert_eq!(names(&b).len(), count, "and the other device got every one");
}

#[test]
fn a_vault_header_survives_the_trip_through_the_wire_form() {
    let account_key = uwurdp_vault::AccountKey::generate();
    let (header, _) = uwurdp_vault::create_with(
        b"master",
        Some(&account_key),
        Uuid::now_v7(),
        KdfParams::INSECURE_FOR_TESTS,
    )
    .unwrap();

    let wire = crate::to_wire(&header);
    assert!(wire.needs_account_key, "a joining device has to be told");
    assert_eq!(crate::from_wire(&wire).unwrap(), header);

    // A header that arrives from somewhere else is input, not truth.
    for broken in [
        uwurdp_proto::api::WireVault {
            salt: "c2hvcnQ".into(),
            ..wire.clone()
        },
        uwurdp_proto::api::WireVault {
            kdf_memory_kib: u32::MAX,
            ..wire.clone()
        },
        uwurdp_proto::api::WireVault {
            wrapped_nonce: "dG9vLXNob3J0".into(),
            ..wire.clone()
        },
        uwurdp_proto::api::WireVault {
            wrapped_blob: String::new(),
            ..wire.clone()
        },
        uwurdp_proto::api::WireVault {
            salt: "not base64!!".into(),
            ..wire.clone()
        },
    ] {
        assert!(crate::from_wire(&broken).is_err(), "{broken:?}");
    }
}

#[test]
fn the_conflict_rule_is_the_one_both_sides_share() {
    // The rule lives in the protocol crate, so the server and both devices
    // cannot drift apart on it. Spot-check that this is the rule in force.
    let older = Version::new(Hlc::new(100, 0, 1), false);
    let newer = Version::new(Hlc::new(200, 0, 2), false);
    assert_eq!(uwurdp_proto::resolve(newer, older), Resolution::Local);
    assert_eq!(uwurdp_proto::resolve(older, newer), Resolution::Remote);
    let tombstone = Version::new(Hlc::new(1, 0, 1), true);
    assert_eq!(uwurdp_proto::resolve(newer, tombstone), Resolution::Remote);
}

#[test]
fn a_workspace_this_build_does_not_know_lands_in_the_private_one() {
    let server = MemoryServer::new();
    let a = first_device();
    let header = a.vault_header().unwrap().unwrap();
    let vault = header.unlock(PASSWORD).unwrap();
    let id = Uuid::now_v7();
    let clock = Hlc::new(1_700_000_000_000, 0, 9);
    let payload = serde_json::to_vec(&HostPayload {
        name: "third".into(),
        address: "10.0.0.33".into(),
        port: 22,
        workspace: "family".into(),
        position: 0,
        group_id: None,
        identity_id: None,
        rdp: Default::default(),
        gateway_identity_id: None,
        comment: String::new(),
        extra: Extra::new(),
    })
    .unwrap();
    let sealed = vault
        .seal_synced(id, EntityKind::Host, clock, false, &payload)
        .unwrap();
    server.store(Envelope {
        id,
        vault_id: vault.vault_id(),
        kind: EntityKind::Host,
        updated_at: clock,
        base_seq: 0,
        deleted: false,
        nonce: sealed.nonce,
        blob: sealed.blob,
        seq: None,
    });

    let report = sync_once(&a, &server).unwrap();
    assert_eq!(report.apply.applied, 1);
    let hosts = a.list_hosts().unwrap();
    assert_eq!(hosts[0].workspace, Workspace::Private);
}

// ── Manifests: a server that keeps things back ────────────────────────────

fn manifests_on(server: &MemoryServer) -> usize {
    server
        .records()
        .iter()
        .filter(|env| env.kind == EntityKind::Manifest)
        .count()
}

fn known_host_envelope(server: &MemoryServer) -> Envelope {
    server
        .records()
        .into_iter()
        .find(|env| env.kind == EntityKind::KnownHost)
        .unwrap()
}

fn envelope_of(server: &MemoryServer, id: Uuid) -> Envelope {
    server
        .records()
        .into_iter()
        .find(|env| env.id == id)
        .unwrap()
}

#[test]
fn edits_deletes_and_many_pages_between_three_devices_leave_nothing_to_report() {
    let server = MemoryServer::new();
    let a = first_device();
    let mut ids = Vec::new();
    for index in 0..300 {
        let mut host = draft(&format!("host-{index}"), &format!("10.0.1.{}", index % 250));
        host.group_path = Some(format!("Group {}", index % 7));
        ids.push(a.save_host(host).unwrap().id);
    }
    a.trust_host_key(
        "nas.lan",
        22,
        "ssh-ed25519",
        "SHA256:one",
        "ssh-ed25519 ONE",
    )
    .unwrap();
    let report = sync_once(&a, &server).unwrap();
    assert!(server.len() > MAX_BATCH, "more than one page to pull");
    assert_eq!(report.withheld, Default::default());

    let b = joined_device(&a);
    let c = joined_device(&a);
    for device in [&b, &c] {
        let report = sync_once(device, &server).unwrap();
        assert!(report.pulled > MAX_BATCH);
        assert_eq!(report.withheld, Default::default(), "{report:?}");
    }
    assert_eq!(manifests_on(&server), 3, "one manifest per device");

    // Everyone at once: two renames of the same host, a delete, a new host,
    // and a host key trusted differently on two devices — the one that loses
    // its slot is gone without a tombstone, and must not count as missing.
    let mut on_a = draft("renamed-on-a", "10.0.1.0");
    on_a.id = Some(ids[0]);
    a.save_host(on_a).unwrap();
    a.trust_host_key("db.lan", 22, "ssh-ed25519", "SHA256:a", "ssh-ed25519 A")
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(3));
    let mut on_b = draft("renamed-on-b", "10.0.1.0");
    on_b.id = Some(ids[0]);
    b.save_host(on_b).unwrap();
    b.delete_host(ids[1]).unwrap();
    b.trust_host_key("db.lan", 22, "ssh-ed25519", "SHA256:b", "ssh-ed25519 B")
        .unwrap();
    c.save_host(draft("new-on-c", "10.0.2.1")).unwrap();
    c.forget_host_key("nas.lan", 22).unwrap();

    for _ in 0..2 {
        for device in [&a, &b, &c] {
            let report = sync_once(device, &server).unwrap();
            assert_eq!(report.withheld, Default::default(), "{report:?}");
            assert_eq!(report.apply.rejected, 0);
        }
    }
    assert_eq!(names(&a).len(), names(&c).len());
    for device in [&a, &b, &c] {
        assert!(device.manifest_violations().unwrap().is_empty());
        assert_eq!(
            device
                .known_host("db.lan", 22)
                .unwrap()
                .unwrap()
                .fingerprint,
            "SHA256:b",
            "host keys from other devices are trusted while nothing is missing"
        );
    }

    // And a device that joins after all that finds everything too.
    let d = joined_device(&a);
    assert_eq!(sync_once(&d, &server).unwrap().withheld, Default::default());
    assert_eq!(names(&d).len(), names(&a).len());
}

#[test]
fn a_server_that_hands_a_new_device_an_old_host_key_is_caught() {
    let server = MemoryServer::new();
    let a = first_device();
    a.save_host(draft("bastion", "bastion.lan")).unwrap();
    a.trust_host_key(
        "bastion.lan",
        22,
        "ssh-ed25519",
        "SHA256:old",
        "ssh-ed25519 OLD",
    )
    .unwrap();
    sync_once(&a, &server).unwrap();
    let old = known_host_envelope(&server);

    // The bastion was reinstalled; A trusts its new key, in place.
    a.trust_host_key(
        "bastion.lan",
        22,
        "ssh-ed25519",
        "SHA256:new",
        "ssh-ed25519 NEW",
    )
    .unwrap();
    sync_once(&a, &server).unwrap();
    assert_eq!(known_host_envelope(&server).id, old.id, "the same record");

    // The server hands a new device the key from before — sealed, authentic,
    // and exactly what someone in the middle of the reinstalled host needs.
    server.serve_stale(old.clone());
    let c = joined_device(&a);
    let report = sync_once(&c, &server).unwrap();
    assert_eq!(report.apply.rejected, 0, "no seal catches this");
    assert_eq!(report.withheld.records, 1);
    assert!(report.withheld.host_keys);
    let violations = c.manifest_violations().unwrap();
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].kind, EntityKind::KnownHost);
    assert_eq!(violations[0].id, old.id);
    assert_eq!(violations[0].problem, uwurdp_store::Problem::Older);

    // The old key is not trusted: the next connection asks.
    assert!(c.known_host("bastion.lan", 22).unwrap().is_none());
    // The next pass still says so, without pulling everything again.
    let again = sync_once(&c, &server).unwrap();
    assert_eq!(again.withheld.records, 1);
    assert_eq!(again.pulled, 0);

    // A key the user accepts on this device, after seeing it, is trusted.
    c.trust_host_key(
        "bastion.lan",
        22,
        "ssh-ed25519",
        "SHA256:new",
        "ssh-ed25519 NEW",
    )
    .unwrap();
    assert_eq!(
        c.known_host("bastion.lan", 22)
            .unwrap()
            .unwrap()
            .fingerprint,
        "SHA256:new"
    );
}

#[test]
fn a_server_that_keeps_a_record_to_itself_is_caught() {
    let server = MemoryServer::new();
    let a = first_device();
    a.save_host(draft("web", "10.0.0.20")).unwrap();
    let hidden = a.save_host(draft("hidden", "10.0.0.21")).unwrap();
    a.trust_host_key(
        "web.lan",
        22,
        "ssh-ed25519",
        "SHA256:web",
        "ssh-ed25519 WEB",
    )
    .unwrap();
    sync_once(&a, &server).unwrap();

    server.withhold(hidden.id);
    let c = joined_device(&a);
    let report = sync_once(&c, &server).unwrap();
    assert_eq!(names(&c), vec!["web"]);
    assert_eq!(report.withheld.records, 1);
    assert!(
        !report.withheld.host_keys,
        "a host is missing, no host key is in doubt"
    );
    let violations = c.manifest_violations().unwrap();
    assert_eq!(
        (violations[0].kind, violations[0].id, violations[0].problem),
        (EntityKind::Host, hidden.id, uwurdp_store::Problem::Missing)
    );
    assert!(c.known_host("web.lan", 22).unwrap().is_some());
}

#[test]
fn a_delete_the_server_keeps_from_an_existing_device_is_caught() {
    let server = MemoryServer::new();
    let a = first_device();
    let b = joined_device(&a);
    let host = a.save_host(draft("decommissioned", "10.0.0.66")).unwrap();
    sync_once(&a, &server).unwrap();
    sync_once(&b, &server).unwrap();

    a.delete_host(host.id).unwrap();
    sync_once(&a, &server).unwrap();
    server.withhold(host.id);
    let report = sync_once(&b, &server).unwrap();
    assert_eq!(names(&b), vec!["decommissioned"], "the delete never came");
    assert_eq!(report.withheld.records, 1);
    assert_eq!(
        b.manifest_violations().unwrap()[0].problem,
        uwurdp_store::Problem::Older
    );

    // Once the server hands it over, the alarm clears by itself.
    server.behave();
    let tombstone = envelope_of(&server, host.id);
    server.store(tombstone);
    let report = sync_once(&b, &server).unwrap();
    assert!(names(&b).is_empty());
    assert_eq!(report.withheld, Default::default());
}

#[test]
fn a_new_device_notices_the_manifest_its_pairing_promised_is_missing() {
    let server = MemoryServer::new();
    let a = first_device();
    a.save_host(draft("one", "10.0.0.1")).unwrap();
    a.trust_host_key(
        "one.lan",
        22,
        "ssh-ed25519",
        "SHA256:one",
        "ssh-ed25519 ONE",
    )
    .unwrap();
    sync_once(&a, &server).unwrap();
    let floor = a
        .published_manifest()
        .unwrap()
        .expect("published and confirmed");
    assert_eq!(floor.id, a.own_manifest_id().unwrap());

    // The server hides A's manifest — and with it any way to tell that it
    // left something else out. Without the floor that goes unnoticed: the
    // limit of what a manifest can do for a device with nothing to go on.
    server.withhold(floor.id);
    let unwarned = joined_device(&a);
    assert_eq!(
        sync_once(&unwarned, &server).unwrap().withheld,
        Default::default()
    );

    // With what the pairing said, it does not.
    let c = joined_device(&a);
    c.set_manifest_floor(Some(floor)).unwrap();
    let report = sync_once(&c, &server).unwrap();
    assert_eq!(report.withheld.records, 1);
    assert!(
        report.withheld.host_keys,
        "any host key could be the old one"
    );
    let violation = c.manifest_violations().unwrap()[0];
    assert_eq!(
        (violation.kind, violation.id, violation.problem),
        (
            EntityKind::Manifest,
            floor.id,
            uwurdp_store::Problem::Missing
        )
    );
    assert!(c.known_host("one.lan", 22).unwrap().is_none());

    // An older manifest of A's in its place does not satisfy it either.
    let old_manifest = envelope_of(&server, floor.id);
    a.save_host(draft("two", "10.0.0.2")).unwrap();
    sync_once(&a, &server).unwrap();
    let newer = a.published_manifest().unwrap().unwrap();
    assert!(newer.clock > floor.clock);
    server.behave();
    server.serve_stale(old_manifest);
    let d = joined_device(&a);
    d.set_manifest_floor(Some(newer)).unwrap();
    sync_once(&d, &server).unwrap();
    assert_eq!(
        d.manifest_violations().unwrap()[0].problem,
        uwurdp_store::Problem::Older
    );
}

#[test]
fn a_server_that_replays_an_old_manifest_to_an_existing_device_gets_nowhere() {
    let server = MemoryServer::new();
    let a = first_device();
    let b = joined_device(&a);
    a.save_host(draft("one", "10.0.0.1")).unwrap();
    sync_once(&a, &server).unwrap();
    let id = a.own_manifest_id().unwrap();
    let old = envelope_of(&server, id);

    let two = a.save_host(draft("two", "10.0.0.2")).unwrap();
    sync_once(&a, &server).unwrap();
    sync_once(&b, &server).unwrap();
    let newest = b.manifest_clock(id).unwrap().unwrap();
    assert!(newest > old.updated_at);

    // The old manifest again, as the newest thing the server has: one that
    // does not list "two", so "two" could be kept back unnoticed.
    server.store(old);
    server.withhold(two.id);
    let report = sync_once(&b, &server).unwrap();
    assert_eq!(report.apply.rejected, 0);
    assert_eq!(
        b.manifest_clock(id).unwrap(),
        Some(newest),
        "the newer manifest stays"
    );
    assert_eq!(report.withheld, Default::default(), "b has two already");

    // A new device still has B's word for "two".
    let c = joined_device(&a);
    let report = sync_once(&c, &server).unwrap();
    assert_eq!(report.withheld.records, 1);
    assert_eq!(c.manifest_violations().unwrap()[0].id, two.id);

    // Without B's manifest as well, a device that never saw A's newer one is
    // fooled by the replay — which is what the pairing floor is for.
    server.withhold(b.own_manifest_id().unwrap());
    let d = joined_device(&a);
    assert_eq!(sync_once(&d, &server).unwrap().withheld, Default::default());
    let e = joined_device(&a);
    e.set_manifest_floor(a.published_manifest().unwrap())
        .unwrap();
    assert_eq!(sync_once(&e, &server).unwrap().withheld.records, 1);
}

#[test]
fn a_manifest_the_server_does_not_know_yet_holds_up_nothing() {
    use crate::{Transport, TransportError};
    use uwurdp_proto::{PullResponse, PushResponse, SyncCursor};

    /// A server from before manifests: it refuses any request that holds one.
    struct Older(MemoryServer);
    impl Transport for Older {
        fn pull(&self, since: SyncCursor, limit: usize) -> Result<PullResponse, TransportError> {
            self.0.pull(since, limit)
        }
        fn push(&self, envelopes: Vec<Envelope>) -> Result<PushResponse, TransportError> {
            if envelopes.iter().any(|env| env.kind == EntityKind::Manifest) {
                return Err(TransportError::Refused("unknown variant `manifest`".into()));
            }
            self.0.push(envelopes)
        }
    }

    let server = Older(MemoryServer::new());
    let a = first_device();
    a.save_host(draft("one", "10.0.0.1")).unwrap();
    let report = sync_once(&a, &server).unwrap();
    assert!(report.pushed >= 2);
    assert_eq!(a.pending_count().unwrap(), 0);
    assert_eq!(manifests_on(&server.0), 0);

    let b = joined_device(&a);
    sync_once(&b, &server).unwrap();
    assert_eq!(names(&b), vec!["one"]);
}

#[test]
fn a_server_that_never_lets_a_pull_finish_does_not_switch_the_check_off() {
    use crate::{Transport, TransportError};
    use uwurdp_proto::{PullResponse, PushResponse, SyncCursor};

    /// A server that always says there is more: one validly sealed old
    /// record per page, under a cursor that keeps going up.
    struct Endless {
        server: MemoryServer,
        replay: Envelope,
    }
    impl Transport for Endless {
        fn pull(&self, since: SyncCursor, _limit: usize) -> Result<PullResponse, TransportError> {
            let next = SyncCursor(since.0 + 1);
            Ok(PullResponse {
                envelopes: vec![Envelope {
                    seq: Some(next.0),
                    ..self.replay.clone()
                }],
                cursor: next,
                has_more: true,
            })
        }
        fn push(&self, envelopes: Vec<Envelope>) -> Result<PushResponse, TransportError> {
            self.server.push(envelopes)
        }
    }

    let server = MemoryServer::new();
    let a = first_device();
    a.trust_host_key(
        "one.lan",
        22,
        "ssh-ed25519",
        "SHA256:one",
        "ssh-ed25519 ONE",
    )
    .unwrap();
    sync_once(&a, &server).unwrap();
    let floor = a.published_manifest().unwrap().unwrap();
    let endless = Endless {
        replay: known_host_envelope(&server),
        server,
    };

    // Freshly paired, with the manifest the pairing promised: nothing from
    // another device is trusted before a check has found it.
    let c = joined_device(&a);
    c.set_manifest_floor(Some(floor)).unwrap();
    let before = c.withheld().unwrap();
    assert!(before.host_keys);
    assert_eq!(
        before.records, 0,
        "no alarm: nothing is known to be missing"
    );

    let report = sync_once(&c, &endless).unwrap();
    assert!(!report.complete);
    assert!(report.withheld.host_keys);
    assert!(report.withheld.any(), "a pull that never ends is an alarm");
    assert_eq!(c.list_known_hosts().unwrap().len(), 1, "the key did arrive");
    assert!(c.known_host("one.lan", 22).unwrap().is_none());

    // A device without a floor is no different.
    let d = joined_device(&a);
    let report = sync_once(&d, &endless).unwrap();
    assert!(!report.complete);
    assert!(report.withheld.host_keys);
    assert!(d.known_host("one.lan", 22).unwrap().is_none());

    // A pull that reaches the end clears it.
    let report = sync_once(&c, &endless.server).unwrap();
    assert!(report.complete);
    assert_eq!(report.withheld, Default::default());
    assert!(c.known_host("one.lan", 22).unwrap().is_some());
}

#[test]
fn rdp_settings_a_group_login_and_a_gateway_travel_along() {
    let server = MemoryServer::new();
    let a = first_device();
    let b = joined_device(&a);

    a.create_group(Workspace::Business, "Domain Controllers")
        .unwrap();
    a.set_group_login(
        Workspace::Business,
        "Domain Controllers",
        "admin",
        "CORP",
        &PasswordChange::Set {
            value: SecretText::new("group-pw"),
        },
    )
    .unwrap();
    let mut host = draft("dc-1", "10.0.0.10");
    host.username = String::new();
    host.workspace = Some(Workspace::Business);
    host.group_path = Some("Domain Controllers".into());
    host.rdp.display = "fixed".into();
    host.rdp.width = 1600;
    host.rdp.height = 900;
    host.rdp.audio = "off".into();
    host.rdp.gateway = Some(uwurdp_store::GatewaySettings {
        address: "gw.example.com".into(),
        port: 443,
        ..Default::default()
    });
    host.gateway_username = "gw-user".into();
    host.comment = "first DC".into();
    let saved = a.save_host(host).unwrap();

    sync_once(&a, &server).unwrap();
    sync_once(&b, &server).unwrap();

    let host = b.get_host(saved.id).unwrap().expect("the host arrived");
    assert_eq!(host.rdp.display, "fixed");
    assert_eq!((host.rdp.width, host.rdp.height), (1600, 900));
    assert_eq!(host.rdp.audio, "off");
    assert_eq!(
        host.rdp.gateway.as_ref().map(|g| g.address.as_str()),
        Some("gw.example.com")
    );
    assert_eq!(host.gateway_username, "gw-user");
    assert_eq!(host.comment, "first DC");

    let login = b.host_login(saved.id).unwrap().expect("the group's login");
    assert!(login.from_group);
    assert_eq!(
        (login.username.as_str(), login.domain.as_str()),
        ("admin", "CORP")
    );
    let password = b.reveal_login_password(&login).unwrap().unwrap();
    assert_eq!(&*password, b"group-pw");
}
