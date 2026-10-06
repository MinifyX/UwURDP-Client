//! UwULock as the backend, against [`Fake`]: a UwULock Server on 127.0.0.1
//! with one account, real HTTP, the real store and the real crypto.

use super::fake::{Fake, Live, CODE, EMAIL, PASSWORD};
use super::*;
use crate::engine::{Transport, TransportError};
use crate::{sync_once, MemoryServer, SyncError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use uwurdp_proto::{EntityKind, Envelope, Hlc};
use uwurdp_store::{HostDraft, Joining, LockEnrolment, PasswordChange, SecretText, Space, Store};
use uwurdp_vault::KdfParams;
use zeroize::Zeroizing;

fn device() -> LockDevice {
    LockDevice::this_system(Uuid::new_v4())
}

fn request<'a>(fake: &'a Fake) -> SignIn<'a> {
    SignIn {
        server_url: &fake.url,
        email: EMAIL,
        password: PASSWORD,
        two_factor: None,
        remember_token: None,
        known: Known::default(),
        accept_space: None,
    }
}

fn signed_in(fake: &Fake) -> SignedIn {
    match sign_in(&request(fake), device()).unwrap() {
        SignInOutcome::SignedIn(signed) => *signed,
        _ => panic!("no two-step login on this account, nor another space"),
    }
}

fn copy(space: &Space) -> Space {
    Space {
        id: space.id,
        key: space.key.clone(),
    }
}

/// A device that signed in and took the space as its vault.
fn joined(fake: &Fake, store: &Store) -> Lock {
    let signed = signed_in(fake);
    store
        .join_space(
            copy(&signed.space),
            PASSWORD.as_bytes(),
            KdfParams::INSECURE_FOR_TESTS,
            Joining::SignIn,
            &LockEnrolment {
                server_url: &signed.server_url,
                email: &signed.email,
                protected_refresh_token: None,
                protected_remember_token: None,
                kdf: &signed.kdf_to_keep(),
            },
        )
        .unwrap();
    signed.lock
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
        rdp: uwurdp_store::RdpSettings::default(),
        comment: String::new(),
        gateway_username: String::new(),
        gateway_domain: String::new(),
        gateway_password: PasswordChange::Keep,
    }
}

fn names(store: &Store) -> Vec<String> {
    let mut names: Vec<String> = store
        .list_hosts()
        .unwrap()
        .into_iter()
        .map(|h| h.name)
        .collect();
    names.sort();
    names
}

#[test]
fn the_first_sign_in_makes_the_keys_and_the_space_and_the_next_one_takes_them() {
    let fake = Fake::start();
    let first = signed_in(&fake);
    assert!(first.made_space);
    assert_eq!(first.email, EMAIL);
    assert!(fake.account.lock().extras.is_some());
    let (id, _) = fake.space().unwrap();
    assert_eq!(first.space.id, id);

    let second = signed_in(&fake);
    assert!(!second.made_space);
    assert_eq!(second.space.id, first.space.id);
    assert_eq!(*second.space.key, *first.space.key);

    // As a suite app, and nothing else.
    for (scope, client) in fake.account.lock().logins.iter() {
        assert_eq!(scope, "uwu.suite offline_access");
        assert_eq!(client, "uwurdp");
    }
}

#[test]
fn a_wrong_password_says_so_in_the_servers_words() {
    let fake = Fake::start();
    let wrong = SignIn {
        password: "not it",
        ..request(&fake)
    };
    match sign_in(&wrong, device()) {
        Err(LockError::WrongPassword(message)) => assert!(message.contains("incorrect")),
        other => panic!("{:?}", other.err()),
    }
}

#[test]
fn two_step_login_asks_for_a_code_and_can_remember_this_device() {
    let fake = Fake::start().with_two_factor();
    let SignInOutcome::TwoFactor { methods, message } = sign_in(&request(&fake), device()).unwrap()
    else {
        panic!("a second step")
    };
    assert_eq!(message, None);
    assert_eq!(
        methods
            .iter()
            .map(|m| (m.kind, m.supported))
            .collect::<Vec<_>>(),
        vec![("authenticator", true), ("email", true)]
    );
    assert_eq!(methods[1].hint.as_deref(), Some("n***@example.com"));

    let wrong = SignIn {
        two_factor: Some(TwoFactorAnswer {
            provider: 0,
            code: "000000".into(),
            remember: false,
        }),
        ..request(&fake)
    };
    // Refused in the server's words; the app stays at the code.
    match sign_in(&wrong, device()) {
        Err(LockError::WrongPassword(message)) => assert!(message.contains("code"), "{message}"),
        other => panic!("{:?}", other.err()),
    }

    let right = SignIn {
        two_factor: Some(TwoFactorAnswer {
            provider: 0,
            code: format!("{} {}", &CODE[..3], &CODE[3..]),
            remember: true,
        }),
        ..request(&fake)
    };
    // The server remembers a device by its identifier.
    let this = device();
    let SignInOutcome::SignedIn(signed) = sign_in(&right, this.clone()).unwrap() else {
        panic!("in")
    };
    let remembered = signed.remember_token.unwrap();

    let again = SignIn {
        remember_token: Some(&remembered),
        ..request(&fake)
    };
    assert!(matches!(
        sign_in(&again, this).unwrap(),
        SignInOutcome::SignedIn(_)
    ));
    assert!(matches!(
        sign_in(&again, device()).unwrap(),
        SignInOutcome::TwoFactor { message: None, .. }
    ));
    send_email_code_works(&fake);
}

fn send_email_code_works(fake: &Fake) {
    super::account::send_email_code(&fake.url, EMAIL, PASSWORD, &Known::default(), device())
        .unwrap();
    assert!(fake
        .requests()
        .contains(&"POST /api/two-factor/send-email-login".to_string()));
}

#[test]
fn a_weaker_key_derivation_than_last_time_is_refused_before_anything_is_sent() {
    use uwulock_core::crypto::Kdf;
    let fake = Fake::start();
    let store = Store::open_in_memory().unwrap();
    joined(&fake, &store);
    let first = known(&store, &format!("{}/", fake.url), "NYU@example.com").unwrap();
    assert_eq!(
        first.kdf,
        Some(Kdf::Pbkdf2 { iterations: 5_000 }),
        "kept at the sign-in"
    );
    // The fake's prelogin says PBKDF2 at 5 000 rounds; say this account's
    // last sign-in used more, or Argon2id.
    let stronger = [
        Kdf::Pbkdf2 {
            iterations: 600_000,
        },
        Kdf::Argon2id {
            iterations: 3,
            memory_mib: 64,
            parallelism: 4,
        },
    ];
    for before in stronger {
        let logins = fake.account.lock().logins.len();
        let known = Known {
            kdf: Some(before),
            ..Known::default()
        };
        let request = SignIn {
            known: known.clone(),
            ..request(&fake)
        };
        match sign_in(&request, device()) {
            Err(LockError::WeakerKdf(message)) => assert!(message.contains("5000"), "{message}"),
            other => panic!("{:?}", other.err()),
        }
        assert!(matches!(
            super::account::send_email_code(&fake.url, EMAIL, PASSWORD, &known, device()),
            Err(LockError::WeakerKdf(_))
        ));
        assert_eq!(fake.account.lock().logins.len(), logins, "no hash was sent");
    }
    assert!(!fake
        .requests()
        .contains(&"POST /api/two-factor/send-email-login".to_string()));

    // The same as last time, or stronger, signs in; what it keeps reads back.
    let request = SignIn {
        known: first,
        ..request(&fake)
    };
    assert!(matches!(
        sign_in(&request, device()).unwrap(),
        SignInOutcome::SignedIn(_)
    ));
}

/// Sign in as a device that knows what `store` knows of the account.
fn sign_in_known(
    fake: &Fake,
    store: &Store,
    accept_space: Option<AcceptSpace>,
) -> Result<SignInOutcome, LockError> {
    let request = SignIn {
        known: known(store, &fake.url, EMAIL).unwrap(),
        accept_space,
        ..request(fake)
    };
    sign_in(&request, device())
}

fn join_signed(store: &Store, signed: &SignedIn) {
    store
        .join_space(
            copy(&signed.space),
            PASSWORD.as_bytes(),
            KdfParams::INSECURE_FOR_TESTS,
            Joining::SignIn,
            &LockEnrolment {
                server_url: &signed.server_url,
                email: &signed.email,
                protected_refresh_token: None,
                protected_remember_token: None,
                kdf: &signed.kdf_to_keep(),
            },
        )
        .unwrap();
}

#[test]
fn another_space_is_taken_only_when_agreed_and_a_left_one_never() {
    let fake = Fake::start();
    let store = Store::open_in_memory().unwrap();
    let lock = joined(&fake, &store);
    store.save_host(draft("prox-1", "192.0.2.10")).unwrap();
    sync_once(&store, &lock).unwrap();
    let (old, _) = fake.space().unwrap();
    let pushed = fake.records.records().len();
    assert!(pushed > 0);

    // The server lists another space (a rekey elsewhere — or a server that
    // is not honest): nothing is taken or written without asking.
    let new = Uuid::new_v4();
    fake.account.lock().spaces.get_mut("rdp").unwrap().0 = new;
    for accept in [
        None,
        Some(AcceptSpace::Listed(Uuid::new_v4())),
        Some(AcceptSpace::New),
    ] {
        match sign_in_known(&fake, &store, accept).unwrap() {
            SignInOutcome::SpaceChanged { was, now, .. } => {
                assert_eq!((was, now), (old, Some(new)))
            }
            _ => panic!("asked first"),
        }
    }
    assert_eq!(store.lock_state().unwrap().space_id, Some(old));
    assert_eq!(fake.records.records().len(), pushed);

    // Agreed: taken, and the old one is left.
    let SignInOutcome::SignedIn(signed) =
        sign_in_known(&fake, &store, Some(AcceptSpace::Listed(new))).unwrap()
    else {
        panic!("in")
    };
    assert_eq!(signed.space.id, new);
    join_signed(&store, &signed);
    assert_eq!(store.lock_state().unwrap().space_id, Some(new));
    assert_eq!(
        known(&store, &fake.url, EMAIL).unwrap().left_spaces,
        vec![old]
    );
    // Signing in again to the same one asks nothing.
    assert!(matches!(
        sign_in_known(&fake, &store, None).unwrap(),
        SignInOutcome::SignedIn(_)
    ));

    // The server lists the old one again, as one would that rolls the rekey
    // back: refused, agreed to or not.
    fake.account.lock().spaces.get_mut("rdp").unwrap().0 = old;
    for accept in [None, Some(AcceptSpace::Listed(old))] {
        assert!(matches!(
            sign_in_known(&fake, &store, accept),
            Err(LockError::SpaceLeft(id)) if id == old
        ));
    }

    // Deleted: made again only when agreed.
    fake.account.lock().spaces.clear();
    match sign_in_known(&fake, &store, None).unwrap() {
        SignInOutcome::SpaceChanged { was, now, .. } => assert_eq!((was, now), (new, None)),
        _ => panic!("asked first"),
    }
    assert!(fake.space().is_none(), "nothing made");
    let SignInOutcome::SignedIn(made) =
        sign_in_known(&fake, &store, Some(AcceptSpace::New)).unwrap()
    else {
        panic!("in")
    };
    assert!(made.made_space);
    assert_eq!(fake.space().unwrap().0, made.space.id);
}

#[test]
fn a_remember_token_got_before_the_question_comes_along_with_it() {
    let fake = Fake::start().with_two_factor();
    let request = SignIn {
        two_factor: Some(TwoFactorAnswer {
            provider: 0,
            code: CODE.into(),
            remember: true,
        }),
        known: Known {
            space: Some(Uuid::new_v4()),
            ..Known::default()
        },
        ..request(&fake)
    };
    match sign_in(&request, device()).unwrap() {
        SignInOutcome::SpaceChanged {
            now: None,
            remember_token: Some(_),
            ..
        } => {}
        _ => panic!("asked, with the token"),
    }
}

#[test]
fn after_an_official_rotation_the_key_opens_with_the_private_key_and_is_wrapped_again() {
    let fake = Fake::start();
    let first = signed_in(&fake);
    fake.drop_user_wrap();
    let second = signed_in(&fake);
    assert_eq!(*second.space.key, *first.space.key);
    let (user_wrap, _) = fake.account.lock().extras.clone().unwrap();
    assert!(user_wrap.is_some(), "wrapped for the user key again");
}

#[test]
fn a_lost_extras_key_is_named_as_such() {
    let fake = Fake::start();
    signed_in(&fake);
    fake.account.lock().lost = true;
    assert!(matches!(
        sign_in(&request(&fake), device()),
        Err(LockError::KeysLost)
    ));
}

#[test]
fn two_devices_keep_each_other_in_step_through_the_space() {
    let fake = Fake::start();
    let a = Store::open_in_memory().unwrap();
    let b = Store::open_in_memory().unwrap();
    let lock_a = joined(&fake, &a);
    let lock_b = joined(&fake, &b);

    a.save_host(draft("prox-1", "192.0.2.10")).unwrap();
    a.save_host(draft("nas", "192.0.2.20")).unwrap();
    let report = sync_once(&a, &lock_a).unwrap();
    // Two hosts and their logins.
    assert_eq!(report.pushed, 4);
    sync_once(&b, &lock_b).unwrap();
    assert_eq!(names(&b), vec!["nas", "prox-1"]);

    // What the server holds, it cannot read.
    for record in fake.records.records() {
        assert!(!String::from_utf8_lossy(&record.blob).contains("prox-1"));
    }
    // And nothing left to do on either side.
    let quiet = sync_once(&b, &lock_b).unwrap();
    assert_eq!((quiet.pulled, quiet.pushed), (0, 0));
}

#[test]
fn a_token_that_ran_out_is_renewed_once_and_the_new_refresh_token_kept() {
    let fake = Fake::start();
    let store = Store::open_in_memory().unwrap();
    let kept = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let lock = {
        let kept = Arc::clone(&kept);
        joined(&fake, &store).keeping(move |token| kept.lock().push(token.to_string()))
    };
    store.save_host(draft("prox-1", "192.0.2.10")).unwrap();
    fake.expire_access();
    sync_once(&store, &lock).unwrap();
    assert_eq!(kept.lock().as_slice(), ["refresh-2"]);
    assert!(!fake.records.is_empty());

    // From a kept refresh token, as after a restart.
    let restarted = Lock::connect(&fake.url, device())
        .unwrap()
        .with_space(lock.space().unwrap())
        .with_refresh_token(Zeroizing::new("refresh-2".into()));
    sync_once(&store, &restarted).unwrap();
}

#[test]
fn a_session_that_ended_asks_for_the_master_password() {
    let fake = Fake::start();
    let store = Store::open_in_memory().unwrap();
    let lock = joined(&fake, &store);
    fake.end_session();
    assert!(matches!(
        sync_once(&store, &lock),
        Err(SyncError::Transport(TransportError::SignIn(_)))
    ));
}

#[test]
fn a_pull_told_to_start_over_gets_everything_again() {
    let fake = Fake::start();
    let a = Store::open_in_memory().unwrap();
    let b = Store::open_in_memory().unwrap();
    let lock_a = joined(&fake, &a);
    let lock_b = joined(&fake, &b);
    a.save_host(draft("prox-1", "192.0.2.10")).unwrap();
    sync_once(&a, &lock_a).unwrap();
    sync_once(&b, &lock_b).unwrap();
    assert!(b.sync_state().unwrap().cursor > 0);

    fake.account.lock().reset_next_pull = true;
    let report = sync_once(&b, &lock_b).unwrap();
    assert!(report.complete);
    assert_eq!(names(&b), vec!["prox-1"]);
    assert!(!lock_b.take_reset(), "asked once, cleared");
}

#[test]
fn a_space_with_a_new_key_asks_for_the_master_password() {
    let fake = Fake::start();
    let store = Store::open_in_memory().unwrap();
    let lock = joined(&fake, &store);
    store.save_host(draft("prox-1", "192.0.2.10")).unwrap();
    sync_once(&store, &lock).unwrap();
    {
        let mut account = fake.account.lock();
        account.spaces.get_mut("rdp").unwrap().0 = Uuid::new_v4();
        account.reset_next_pull = true;
    }
    assert!(matches!(
        lock.pull(uwurdp_proto::SyncCursor(5), 10),
        Err(TransportError::SignIn(_))
    ));
}

#[test]
fn nothing_is_written_to_or_read_from_the_start_of_a_space_that_changed() {
    let fake = Fake::start();
    let store = Store::open_in_memory().unwrap();
    let lock = joined(&fake, &store);
    // Given a new key (and id) by another device: no reset comes for a pull
    // from the start or for a push, so this device asks before either.
    fake.account.lock().spaces.get_mut("rdp").unwrap().0 = Uuid::new_v4();
    store.save_host(draft("prox-1", "192.0.2.10")).unwrap();
    assert!(matches!(
        sync_once(&store, &lock),
        Err(SyncError::Transport(TransportError::SignIn(message))) if message.contains("new key")
    ));
    assert!(matches!(
        lock.pull(uwurdp_proto::SyncCursor(0), 10),
        Err(TransportError::SignIn(_))
    ));
    assert!(fake.records.is_empty());

    // Deleted in the web vault.
    fake.account.lock().spaces.clear();
    assert!(matches!(
        sync_once(&store, &lock),
        Err(SyncError::Transport(TransportError::SignIn(message))) if message.contains("deleted")
    ));
}

#[test]
fn a_push_names_its_space_and_a_new_key_in_between_keeps_the_edit_here() {
    let fake = Fake::start();
    let store = Store::open_in_memory().unwrap();
    let lock = joined(&fake, &store);
    let space = lock.space().unwrap();
    store.save_host(draft("prox-1", "192.0.2.10")).unwrap();
    sync_once(&store, &lock).unwrap();
    assert!(fake
        .account
        .lock()
        .pushed_space_ids
        .iter()
        .all(|id| *id == serde_json::json!(space)));
    let held = fake.records.len();

    // Another device gives the space a new key right after this one looked:
    // the server refuses the push, and this device asks for the master
    // password instead of trying again.
    store.save_host(draft("nas", "192.0.2.20")).unwrap();
    fake.account.lock().rekey_at_next_push = true;
    assert!(matches!(
        sync_once(&store, &lock),
        Err(SyncError::Transport(TransportError::SignIn(message))) if message.contains("new key")
    ));
    assert_eq!(fake.records.len(), held, "nothing of it was taken");
    assert_eq!(names(&store), vec!["nas", "prox-1"]);
    assert!(
        store.pending_count().unwrap() > 0,
        "still waiting to go out"
    );
}

#[test]
fn only_a_409_about_the_space_asks_for_the_master_password() {
    let fake = Fake::start();
    let store = Store::open_in_memory().unwrap();
    let lock = joined(&fake, &store);
    store.save_host(draft("prox-1", "192.0.2.10")).unwrap();

    // A record id another space or account has: an error about that record.
    fake.account.lock().refuse_next_push = Some(super::api::EXISTS);
    assert!(matches!(
        sync_once(&store, &lock),
        Err(SyncError::Transport(TransportError::Refused(message))) if message.contains("exists")
    ));
    // A 409 of a newer server, in a word this build does not know: taken
    // as a space that changed, never as a reason to push again.
    fake.account.lock().refuse_next_push = Some("space_mismatch");
    assert!(matches!(
        sync_once(&store, &lock),
        Err(SyncError::Transport(TransportError::SignIn(_)))
    ));
    assert!(fake.records.is_empty());
    assert!(store.pending_count().unwrap() > 0);
    sync_once(&store, &lock).unwrap();
    assert!(!fake.records.is_empty());
}

// ── The move ─────────────────────────────────────────────────────────────

/// A device on UwUSync ([`MemoryServer`]) with a vault of its own and
/// something in it.
fn on_uwusync() -> (Store, MemoryServer) {
    let store = Store::open_in_memory().unwrap();
    store
        .create_vault_with(b"uwusync password", KdfParams::INSECURE_FOR_TESTS)
        .unwrap();
    let mut host = draft("prox-1", "192.0.2.10");
    host.password = PasswordChange::Set {
        value: SecretText::from(Zeroizing::new("hunter2".to_string())),
    };
    store.save_host(host).unwrap();
    store.save_host(draft("nas", "192.0.2.20")).unwrap();
    let gone = store.save_host(draft("old", "192.0.2.30")).unwrap();
    store.delete_host(gone.id).unwrap();
    let server = MemoryServer::new();
    sync_once(&store, &server).unwrap();
    (store, server)
}

/// A record of a kind this build keeps no table for, as a newer build on
/// another device would have written it to UwUSync.
fn newer_kind(store: &Store, server: &MemoryServer) -> Envelope {
    let header = store.vault_header().unwrap().unwrap();
    let vault = header.unlock(b"uwusync password").unwrap();
    let id = Uuid::new_v4();
    let clock = Hlc::new(1_790_000_000_000, 0, 7);
    let sealed = vault
        .seal_synced(
            id,
            EntityKind::PortForward,
            clock,
            false,
            b"{\"local\":8080}",
        )
        .unwrap();
    let envelope = Envelope {
        id,
        vault_id: header.vault_id,
        kind: EntityKind::PortForward,
        updated_at: clock,
        base_seq: 0,
        deleted: false,
        nonce: sealed.nonce,
        blob: sealed.blob,
        seq: None,
    };
    server.store(envelope.clone());
    envelope
}

fn move_over(store: &Store, from: &MemoryServer, fake: &Fake) -> (MoveReport, SignedIn) {
    let signed = signed_in(fake);
    let report = copy_to_lock(store, from, &signed.lock, &signed.space).unwrap();
    (report, signed)
}

#[test]
fn the_move_copies_everything_checks_it_and_a_second_run_copies_nothing() {
    let fake = Fake::start();
    let (store, uwusync) = on_uwusync();
    let foreign = newer_kind(&store, &uwusync);
    let before = uwusync.records();

    let (report, signed) = move_over(&store, &uwusync, &fake);
    // Three hosts (one a tombstone), their identities, one secret, the
    // record of a newer kind; no manifest.
    assert_eq!(report.read, report.copied);
    assert_eq!(report.unreadable, 0);
    assert!(report.copied >= 6);
    assert!(fake
        .records
        .records()
        .iter()
        .all(|r| r.kind != EntityKind::Manifest && r.vault_id == signed.space.id));
    assert_eq!(uwusync.records(), before, "UwUSync is only read from");

    // The record of a newer kind opens with the space's key, header and all.
    let moved = fake
        .records
        .records()
        .into_iter()
        .find(|r| r.id == foreign.id)
        .unwrap();
    let space_vault =
        uwurdp_vault::UnlockedVault::from_key(signed.space.id, signed.space.key.clone());
    let payload = space_vault
        .open_synced(
            moved.id,
            moved.kind,
            moved.updated_at,
            moved.deleted,
            &uwurdp_vault::Sealed {
                nonce: moved.nonce,
                blob: moved.blob,
            },
        )
        .unwrap();
    assert_eq!(&*payload, b"{\"local\":8080}");

    let (again, _) = move_over(&store, &uwusync, &fake);
    assert_eq!(again.copied, 0);
    assert_eq!(again.already_there, report.copied);

    // Switch, and the first sync there has nothing to send or take.
    store
        .join_space(
            copy(&signed.space),
            PASSWORD.as_bytes(),
            KdfParams::INSECURE_FOR_TESTS,
            Joining::Moved,
            &LockEnrolment {
                server_url: &signed.server_url,
                email: EMAIL,
                protected_refresh_token: None,
                protected_remember_token: None,
                kdf: &signed.kdf_to_keep(),
            },
        )
        .unwrap();
    let first = sync_once(&store, &signed.lock).unwrap();
    assert_eq!(
        (first.pushed, first.conflicts, first.apply.applied),
        (0, 0, 0)
    );
    assert!(first.complete && !first.withheld.any());

    // A new device signs in and has it all, the password included.
    let other = Store::open_in_memory().unwrap();
    let lock = joined(&fake, &other);
    sync_once(&other, &lock).unwrap();
    assert_eq!(names(&other), vec!["nas", "prox-1"]);
    let prox = other
        .list_hosts()
        .unwrap()
        .into_iter()
        .find(|h| h.name == "prox-1")
        .unwrap();
    assert_eq!(&**other.reveal_host_password(prox.id).unwrap(), b"hunter2");
}

#[test]
fn what_another_device_changed_on_uwulock_meanwhile_is_kept() {
    let fake = Fake::start();
    let (store, uwusync) = on_uwusync();
    let (_, signed) = move_over(&store, &uwusync, &fake);

    // Another device moved first and renamed a host there since.
    let other = Store::open_in_memory().unwrap();
    let lock = joined(&fake, &other);
    sync_once(&other, &lock).unwrap();
    let host = other
        .list_hosts()
        .unwrap()
        .into_iter()
        .find(|h| h.name == "nas")
        .unwrap();
    let mut edit = draft("nas (new)", &host.address);
    edit.id = Some(host.id);
    other.save_host(edit).unwrap();
    sync_once(&other, &lock).unwrap();

    let report = copy_to_lock(&store, &uwusync, &signed.lock, &signed.space).unwrap();
    // The host and its login were edited there.
    assert_eq!(report.newer_there, 2);
    assert_eq!(report.copied, 0);
}

#[test]
fn a_copy_the_server_keeps_back_stops_the_move() {
    let fake = Fake::start();
    let (store, uwusync) = on_uwusync();
    let signed = signed_in(&fake);
    let kept_back = uwusync
        .records()
        .into_iter()
        .find(|r| r.kind == EntityKind::Host && !r.deleted)
        .unwrap()
        .id;
    fake.records.withhold(kept_back);
    match copy_to_lock(&store, &uwusync, &signed.lock, &signed.space) {
        Err(LockError::MoveCheck(differences)) => {
            assert_eq!(differences.len(), 1);
            assert_eq!(differences[0].id, kept_back);
            assert_eq!(differences[0].problem, Problem::Missing);
        }
        other => panic!("{:?}", other.map(|_| ())),
    }
    assert!(store.sync_state().unwrap().paired() || !store.lock_state().unwrap().active);
}

#[test]
fn headers_a_server_made_up_do_not_pass_the_check() {
    let fake = Fake::start();
    let (store, uwusync) = on_uwusync();
    let signed = signed_in(&fake);
    // Every record the move reads back claims to be newer than it is, as if
    // another device had written it since: by the headers alone, the copy
    // would pass and this device forget UwUSync.
    fake.account.lock().forge_pulls = true;
    match copy_to_lock(&store, &uwusync, &signed.lock, &signed.space) {
        Err(LockError::MoveCheck(differences)) => {
            assert!(differences.len() >= 6, "{differences:?}");
            assert!(differences.iter().all(|d| d.problem == Problem::Missing));
        }
        other => panic!("{:?}", other.map(|_| ())),
    }
}

#[test]
fn a_locked_vault_moves_nothing() {
    let fake = Fake::start();
    let (store, uwusync) = on_uwusync();
    store.lock_vault();
    let signed = signed_in(&fake);
    assert!(matches!(
        copy_to_lock(&store, &uwusync, &signed.lock, &signed.space),
        Err(LockError::VaultLocked)
    ));
    assert!(fake.records.is_empty());
}

// ── Realtime ─────────────────────────────────────────────────────────────

#[test]
fn the_channel_signs_in_passes_on_changes_to_this_space_and_ends_on_logout() {
    let fake = Fake::start();
    let signed = signed_in(&fake);
    let script = fake.live();
    let (seen, events) = mpsc::channel();
    let lock = signed.lock;
    let runner = std::thread::spawn(move || {
        live::run(&lock, &|| true, &mut |event| {
            let _ = seen.send(event);
        })
    });

    let (reply, auth) = mpsc::channel();
    script.send(Live::Expect(reply)).unwrap();
    let auth: serde_json::Value =
        serde_json::from_str(&auth.recv_timeout(Duration::from_secs(10)).unwrap()).unwrap();
    assert_eq!(auth["type"], "auth");
    assert_eq!(auth["token"], "access-1");

    let far = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    script
        .send(Live::Send(format!(
            r#"{{"type":"ready","connectionId":"c1","expires":{far},"heartbeat":25}}"#
        )))
        .unwrap();
    assert_eq!(
        events.recv_timeout(Duration::from_secs(10)).unwrap(),
        live::Event::Ready
    );

    script
        .send(Live::Send(
            r#"{"type":"changed","areas":["suite"],"spaces":["mail"]}"#.into(),
        ))
        .unwrap();
    script
        .send(Live::Send(
            r#"{"type":"changed","areas":["suite"],"spaces":["rdp"]}"#.into(),
        ))
        .unwrap();
    assert_eq!(
        events.recv_timeout(Duration::from_secs(10)).unwrap(),
        live::Event::Changed
    );

    script
        .send(Live::Send(
            r#"{"type":"logout","reason":"deviceRemoved"}"#.into(),
        ))
        .unwrap();
    assert_eq!(
        runner.join().unwrap(),
        live::Ended::Logout("deviceRemoved".into())
    );
    assert!(
        events.try_recv().is_err(),
        "the other space's change was not passed on"
    );
}

#[test]
fn the_channel_renews_its_token_in_time_and_after_the_server_refused_it() {
    let fake = Fake::start();
    let signed = signed_in(&fake);
    let script = fake.live();
    let stop = Arc::new(AtomicBool::new(false));
    let lock = signed.lock;
    let runner = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || live::run(&lock, &|| !stop.load(Ordering::SeqCst), &mut |_| {}))
    };
    let (reply, messages) = mpsc::channel();
    let next = |script: &mpsc::Sender<Live>| -> serde_json::Value {
        script.send(Live::Expect(reply.clone())).unwrap();
        serde_json::from_str(&messages.recv_timeout(Duration::from_secs(10)).unwrap()).unwrap()
    };
    assert_eq!(next(&script)["token"], "access-1");

    // The server says the token runs out in half a minute: a new `auth` on
    // the same connection, with one that lasts longer.
    let soon = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 30;
    script
        .send(Live::Send(format!(
            r#"{{"type":"ready","connectionId":"c1","expires":{soon},"heartbeat":25}}"#
        )))
        .unwrap();
    assert_eq!(next(&script)["type"], "auth");

    // The server refuses the token (4401): renewed, then connected again.
    fake.expire_access();
    let script_again = fake.live_later();
    script.send(Live::Close(4401)).unwrap();
    let again = next(&script_again);
    assert_eq!(again["type"], "auth");
    assert!(again["token"].as_str().unwrap().starts_with("access-"));
    assert_ne!(again["token"], "access-1");
    stop.store(true, Ordering::SeqCst);
    assert_eq!(runner.join().unwrap(), live::Ended::Stopped);
}

#[test]
fn the_channel_takes_no_message_larger_than_the_contract_allows() {
    let fake = Fake::start();
    let signed = signed_in(&fake);
    let script = fake.live();
    let (seen, events) = mpsc::channel();
    let lock = signed.lock;
    let runner = std::thread::spawn(move || {
        live::connection(&lock, &|| true, &mut |event| {
            let _ = seen.send(event);
        })
    });
    let (reply, auth) = mpsc::channel();
    script.send(Live::Expect(reply)).unwrap();
    auth.recv_timeout(Duration::from_secs(10)).unwrap();
    script
        .send(Live::Send(
            r#"{"type":"ready","connectionId":"c1","expires":0,"heartbeat":25}"#.into(),
        ))
        .unwrap();
    assert_eq!(
        events.recv_timeout(Duration::from_secs(10)).unwrap(),
        live::Event::Ready
    );

    // A change, padded with a field no one reads to exactly this size.
    let padded = |size: usize| {
        let bare = format!(r#"{{"type":"changed","spaces":["{SPACE}"],"pad":""}}"#);
        bare.replace(
            r#""pad":"""#,
            &format!(r#""pad":"{}""#, "x".repeat(size - bare.len())),
        )
    };
    assert_eq!(padded(live::MAX_MESSAGE).len(), live::MAX_MESSAGE);
    script.send(Live::Send(padded(live::MAX_MESSAGE))).unwrap();
    assert_eq!(
        events.recv_timeout(Duration::from_secs(10)).unwrap(),
        live::Event::Changed
    );
    // One byte more, and the connection ends instead of taking it in.
    script
        .send(Live::Send(padded(live::MAX_MESSAGE + 1)))
        .unwrap();
    assert_eq!(runner.join().unwrap(), live::Ended::Retry(Duration::ZERO));
    assert!(events.try_recv().is_err());
}

// ── App sync switched off on the server ──────────────────────────────────

#[test]
fn app_sync_switched_off_waits_and_keeps_the_session_for_when_it_is_on_again() {
    let fake = Fake::start();
    let store = Store::open_in_memory().unwrap();
    let lock = joined(&fake, &store);
    store.save_host(draft("prox-1", "192.0.2.10")).unwrap();

    fake.switch_suite(true);
    assert!(matches!(
        sync_once(&store, &lock),
        Err(SyncError::Transport(TransportError::SwitchedOff))
    ));
    // Its access token ran out meanwhile: the refused refresh is no ended
    // session, and the refresh token stays.
    fake.expire_access();
    assert!(matches!(
        sync_once(&store, &lock),
        Err(SyncError::Transport(TransportError::SwitchedOff))
    ));
    assert!(lock.refresh_token().is_some());
    assert!(fake.records.is_empty());

    // On again: the edit made meanwhile goes out, without a new sign-in.
    fake.switch_suite(false);
    let report = sync_once(&store, &lock).unwrap();
    assert!(report.pushed > 0);
    assert!(!fake.records.is_empty());
}

#[test]
fn signing_in_or_moving_to_a_server_with_app_sync_off_says_so_before_anything_is_sent() {
    let fake = Fake::start();
    fake.switch_suite(true);
    assert!(app_sync_switched_off(&fake.url, device()).unwrap());
    assert!(matches!(
        sign_in(&request(&fake), device()),
        Err(LockError::Transport(TransportError::SwitchedOff))
    ));
    // Asked before the key derivation: no password hash went out.
    assert!(!fake
        .requests()
        .iter()
        .any(|r| r.ends_with("/connect/token") || r.ends_with("/prelogin")));

    // Switched off between the sign-in and the copy: the move stops, and
    // the device stays on UwUSync.
    fake.switch_suite(false);
    assert!(!app_sync_switched_off(&fake.url, device()).unwrap());
    let (store, uwusync) = on_uwusync();
    let signed = signed_in(&fake);
    fake.switch_suite(true);
    assert!(matches!(
        copy_to_lock(&store, &uwusync, &signed.lock, &signed.space),
        Err(LockError::Transport(TransportError::SwitchedOff))
    ));

    // A server before feature switches says nothing about it.
    fake.account.lock().switches = false;
    assert!(!app_sync_switched_off(&fake.url, device()).unwrap());
}

#[test]
fn the_channel_of_a_switched_off_app_sync_is_refused_not_retried() {
    let fake = Fake::start();
    let signed = signed_in(&fake);
    fake.switch_suite(true);
    assert_eq!(
        live::run(&signed.lock, &|| true, &mut |_| {}),
        live::Ended::Refused
    );
}

// ── Two-step login, once per device ───────────────────────────────────────

/// Sealed as the operating system seals it, for the tests: as it is.
fn sealed(token: &Zeroizing<String>) -> Vec<u8> {
    token.as_bytes().to_vec()
}

fn unsealed(bytes: &[u8]) -> std::io::Result<Zeroizing<Vec<u8>>> {
    Ok(Zeroizing::new(bytes.to_vec()))
}

/// A store in a file of its own, to be opened again as a restart opens it.
struct StoreFile(std::path::PathBuf);

impl StoreFile {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uwurdp-lock-{}.db", Uuid::new_v4())))
    }

    fn open(&self) -> Store {
        Store::open(&self.0).unwrap()
    }
}

impl Drop for StoreFile {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

/// What the app keeps of a sign-in: the space as its vault, and both
/// tokens.
fn keep(store: &Store, signed: &SignedIn) {
    store
        .join_space(
            copy(&signed.space),
            PASSWORD.as_bytes(),
            KdfParams::INSECURE_FOR_TESTS,
            Joining::SignIn,
            &LockEnrolment {
                server_url: &signed.server_url,
                email: &signed.email,
                protected_refresh_token: signed.refresh_token.as_ref().map(sealed),
                protected_remember_token: signed.remember_token.as_ref().map(sealed),
                kdf: &signed.kdf_to_keep(),
            },
        )
        .unwrap();
}

/// A sign-in as the app makes one, as this install: with a code (and
/// "remember this device", as the form has it), or else with the token this
/// device holds for the server and account, if any.
fn sign_in_as_the_app(store: &Store, server_url: &str, code: Option<&str>) -> SignInOutcome {
    let remembered = match code {
        None => remember_token(store, server_url, EMAIL, unsealed).unwrap(),
        Some(_) => None,
    };
    let request = SignIn {
        server_url,
        email: EMAIL,
        password: PASSWORD,
        two_factor: code.map(|code| TwoFactorAnswer {
            provider: 0,
            code: code.into(),
            remember: true,
        }),
        remember_token: remembered.as_deref().map(String::as_str),
        known: known(store, server_url, EMAIL).unwrap(),
        accept_space: None,
    };
    let device = LockDevice::this_system(store.lock_device_identifier().unwrap());
    sign_in(&request, device).unwrap()
}

fn signed(outcome: SignInOutcome) -> SignedIn {
    match outcome {
        SignInOutcome::SignedIn(signed) => *signed,
        _ => panic!("not signed in"),
    }
}

fn asked(outcome: SignInOutcome) -> bool {
    matches!(outcome, SignInOutcome::TwoFactor { message: None, .. })
}

#[test]
fn two_step_login_is_asked_once_and_the_device_stays_remembered_through_lock_and_restart() {
    let fake = Fake::start().with_two_factor();
    let file = StoreFile::new();
    let store = file.open();
    let device = store.lock_device_identifier().unwrap();
    assert!(asked(sign_in_as_the_app(&store, &fake.url, None)));
    keep(
        &store,
        &signed(sign_in_as_the_app(&store, &fake.url, Some(CODE))),
    );

    // The session runs out, the vault is locked, the app quits.
    store.end_lock_session().unwrap();
    store.lock_vault();
    drop(store);
    let store = file.open();
    assert_eq!(
        store.lock_device_identifier().unwrap(),
        device,
        "the same device to the server"
    );
    assert!(!store.lock_state().unwrap().signed_in);

    // Email and master password, nothing else — whichever way the address
    // and the email are typed.
    let typed = format!(" {}/ ", fake.url);
    let remembered = remember_token(&store, &typed, "NYU@example.com", unsealed).unwrap();
    assert!(remembered.is_some());
    keep(&store, &signed(sign_in_as_the_app(&store, &typed, None)));
    let token = fake.account.lock().remembered[&device.to_string()].clone();
    assert_eq!(
        fake.account.lock().second_steps.last(),
        Some(&("5".to_string(), token))
    );
    // Signed in once more after that: still remembered.
    store.end_lock_session().unwrap();
    signed(sign_in_as_the_app(&store, &fake.url, None));
}

#[test]
fn the_remember_token_goes_only_to_the_server_that_issued_it_and_signing_out_forgets_it() {
    let issuer = Fake::start().with_two_factor();
    let other = Fake::start().with_two_factor();
    let store = Store::open_in_memory().unwrap();
    keep(
        &store,
        &signed(sign_in_as_the_app(&store, &issuer.url, Some(CODE))),
    );
    assert!(remember_token(&store, &issuer.url, EMAIL, unsealed)
        .unwrap()
        .is_some());

    // The same email on another server: nothing of the token goes there.
    assert!(remember_token(&store, &other.url, EMAIL, unsealed)
        .unwrap()
        .is_none());
    assert!(remember_token(&store, "not an address", EMAIL, unsealed)
        .unwrap()
        .is_none());
    assert!(asked(sign_in_as_the_app(&store, &other.url, None)));
    assert_eq!(
        other.account.lock().second_steps,
        vec![(String::new(), String::new())]
    );

    // Signing out is where the device is forgotten: the next sign-in asks.
    store.leave_lock().unwrap();
    assert!(remember_token(&store, &issuer.url, EMAIL, unsealed)
        .unwrap()
        .is_none());
    assert!(asked(sign_in_as_the_app(&store, &issuer.url, None)));
    assert_eq!(
        issuer.account.lock().second_steps.last(),
        Some(&(String::new(), String::new()))
    );
}

#[test]
fn a_token_the_server_no_longer_takes_asks_for_the_code_again_and_the_new_one_is_kept() {
    let fake = Fake::start().with_two_factor();
    let store = Store::open_in_memory().unwrap();
    let first = signed(sign_in_as_the_app(&store, &fake.url, Some(CODE)));
    keep(&store, &first);

    // How long the server remembers a device is the server's to say.
    fake.account.lock().remembered.clear();
    assert!(asked(sign_in_as_the_app(&store, &fake.url, None)));
    let second = signed(sign_in_as_the_app(&store, &fake.url, Some(CODE)));
    keep(&store, &second);
    let kept = remember_token(&store, &fake.url, EMAIL, unsealed)
        .unwrap()
        .unwrap();
    assert_eq!(kept, *second.remember_token.as_ref().unwrap());
    assert_ne!(kept, *first.remember_token.as_ref().unwrap());
    signed(sign_in_as_the_app(&store, &fake.url, None));
}
