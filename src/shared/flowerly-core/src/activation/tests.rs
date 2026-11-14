use super::*;
use crate::storage::lifecycle::{ActivationState, EvidenceOutcome, RootIdentity};
use crate::types::{Generation, GenerationStatus, ObjectRef, Owner, Timestamp};
use crate::witness::testing::{Chain, BOOT_1, COMMIT, PART_B};
use crate::witness::LedgerPolicy;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::thread::JoinHandle;

const ADMIN: Caller = Caller::new(4_000_001);

fn hash(c: char) -> String {
    c.to_string().repeat(64)
}

/// What the fake broker does with one connection.
enum Reply {
    Answer(Value),
    /// Reads the intent and closes without answering.
    Hangup,
}

struct Rig {
    dir: tempfile::TempDir,
    host: ActivationHost,
    store: Mutex<Store>,
    generation: GenerationId,
    ledger: Chain,
}

fn planned(irreversible: bool) -> Value {
    json!({"ok": true, "status": "PLANNED", "target": "root-b", "root_partuuid": PART_B,
           "irreversible": irreversible, "target_min_reader": 17, "fallback_schema_version": 16})
}

fn armed() -> Value {
    json!({"ok": true, "status": "ARMED_PENDING", "target": "root-b",
           "boot_entry": "flowerly-b+3.conf"})
}

fn refusal(reason: &str) -> Value {
    json!({"ok": false, "reason": reason})
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let generation = Generation {
        id: GenerationId::new(),
        parent: None,
        source_commit: None,
        system_manifest: None,
        reason_goal: None,
        experiment: None,
        status: GenerationStatus::Candidate,
        created_at: Timestamp::now(),
        owner: Owner::System,
    };
    store.insert_generation(&generation).unwrap();
    let identity = RootIdentity {
        rootfs_sha256: hash('a'),
        kernel_sha256: hash('b'),
        initramfs_sha256: hash('c'),
        input_manifest_sha256: "d".repeat(64),
        build_id: "build-1".to_owned(),
    };
    store
        .set_root_identity(ADMIN, &generation.id, COMMIT, &identity, "built")
        .unwrap();
    let experiment = store
        .record_evidence(
            ADMIN,
            &generation.id,
            EvidenceOutcome::Passed,
            &hash('9'),
            "kvm",
        )
        .unwrap();
    store
        .declare_tested(ADMIN, &generation.id, &ObjectRef::from(&experiment), "ok")
        .unwrap();

    let meta = std::fs::metadata(dir.path()).unwrap();
    let marker = dir.path().join("test-profile");
    std::fs::write(&marker, b"").unwrap();
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut ledger = Chain::default();
    ledger.witness(BOOT_1, "root-a").health(BOOT_1);
    let ledger_path = dir.path().join("boot-witness.jsonl");
    std::fs::write(&ledger_path, ledger.bytes()).unwrap();
    std::fs::set_permissions(&ledger_path, std::fs::Permissions::from_mode(0o640)).unwrap();
    let host = ActivationHost {
        broker_socket: dir.path().join("broker.sock"),
        test_profile_marker: marker,
        marker_owner_uid: meta.uid(),
        ledger: LedgerSource {
            path: ledger_path,
            policy: LedgerPolicy {
                owner_uid: meta.uid(),
                reader_gid: meta.gid(),
            },
            boot_id_path: dir.path().join("boot_id"),
        },
        backup_dir: dir.path().join("activation-backups"),
    };
    Rig {
        dir,
        host,
        store: Mutex::new(store),
        generation: generation.id,
        ledger,
    }
}

impl Rig {
    /// A broker on the rig's socket answering one connection per reply, in
    /// order; it returns the intents it received.
    fn broker(&self, replies: Vec<Reply>) -> JoinHandle<Vec<Value>> {
        let listener = UnixListener::bind(&self.host.broker_socket).unwrap();
        std::thread::spawn(move || {
            let mut seen = Vec::new();
            for reply in replies {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                seen.push(serde_json::from_str(&line).unwrap());
                if let Reply::Answer(value) = reply {
                    let mut out = value.to_string();
                    out.push('\n');
                    (&stream).write_all(out.as_bytes()).unwrap();
                }
            }
            seen
        })
    }

    fn command(&self) -> ActivateCommand {
        ActivateCommand {
            generation: self.generation.clone(),
            kind: ActivationKind::Activate,
            reason: "roll out".to_owned(),
            operation_id: None,
            acknowledges: None,
            acknowledge_irreversible: false,
        }
    }

    fn run(&self, command: &ActivateCommand) -> Result<Outcome, Failure> {
        activate(&self.store, Some(&self.host), ADMIN, command)
    }

    fn history(&self) -> Vec<Activation> {
        self.store.lock().unwrap().activation_history().unwrap()
    }

    fn backups(&self) -> Vec<String> {
        match std::fs::read_dir(&self.host.backup_dir) {
            Ok(entries) => entries
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn nothing_written(&self) {
        assert!(self.history().is_empty(), "no activation row");
        assert!(self.backups().is_empty(), "no backup");
    }
}

fn claim_of(intent: &Value) -> &Value {
    &intent["generation"]
}

#[test]
fn a_reversible_activation_plans_records_and_then_arms() {
    let rig = rig();
    let broker = rig.broker(vec![Reply::Answer(planned(false)), Reply::Answer(armed())]);
    let outcome = rig.run(&rig.command()).unwrap();
    let intents = broker.join().unwrap();
    let Outcome::Armed { activation, broker } = outcome else {
        panic!("not armed: {outcome:?}");
    };
    assert_eq!(broker["status"], "ARMED_PENDING");
    // The row was written from the plan and the ledger before arming.
    assert_eq!(activation.state, ActivationState::Pending);
    assert_eq!(activation.expected_slot, "root-b");
    assert_eq!(activation.expected_root_partuuid, PART_B);
    assert_eq!(activation.ledger_sequence, 2);
    assert_eq!(activation.ledger_head, rig.ledger.head());
    assert_eq!(activation.actor_uid, ADMIN.uid());
    assert!(!activation.irreversible);
    assert_eq!(rig.history().len(), 1);
    assert!(
        rig.backups().is_empty(),
        "a reversible activation takes no backup"
    );
    // Both intents name the attested admin and the recorded identity, never
    // a path; the plan leaves the slot to the broker, the arm names the one
    // the plan gave and carries tries.
    assert_eq!(intents.len(), 2);
    assert_eq!(intents[0]["intent"], "plan");
    assert_eq!(intents[1]["intent"], "arm");
    assert!(intents[0].get("slot").is_none());
    assert_eq!(intents[1]["slot"], "root-b");
    for intent in &intents {
        assert_eq!(intent["version"], 1);
        assert_eq!(intent["admin_uid"], ADMIN.uid());
        let claim = claim_of(intent);
        assert_eq!(claim["rootfs_sha256"], hash('a'));
        assert_eq!(claim["kernel_sha256"], hash('b'));
        assert_eq!(claim["initramfs_sha256"], hash('c'));
        assert_eq!(claim["source_commit"], COMMIT);
        assert_eq!(claim["build_id"], "build-1");
        assert!(intent.get("irreversible").is_none());
    }
    assert!(intents[0].get("tries").is_none());
    assert_eq!(intents[1]["tries"], 3);
}

#[test]
fn nothing_happens_while_activation_is_disabled() {
    let mut rig = rig();
    rig.host.test_profile_marker = rig.dir.path().join("no-marker");
    assert!(matches!(rig.run(&rig.command()), Err(Failure::Disabled)));
    rig.nothing_written();
    // No host at all (no state directory) is disabled too, but a missing
    // Generation is still reported as missing.
    let rig = super::tests::rig();
    let none = activate(&rig.store, None, ADMIN, &rig.command());
    assert!(matches!(none, Err(Failure::Disabled)));
    let mut missing = rig.command();
    missing.generation = GenerationId::new();
    let error = activate(&rig.store, None, ADMIN, &missing).unwrap_err();
    assert!(
        matches!(error, Failure::Storage(StorageError::NotFound(_))),
        "{error:?}"
    );
    rig.nothing_written();
    // A marker the service account could have written does not count.
    let rig = {
        let mut rig = super::tests::rig();
        std::fs::set_permissions(
            &rig.host.test_profile_marker,
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        rig.host.marker_owner_uid = rig.host.marker_owner_uid.wrapping_add(1);
        rig
    };
    assert!(matches!(rig.run(&rig.command()), Err(Failure::Disabled)));
    rig.nothing_written();
}

#[test]
fn the_marker_must_be_a_regular_file_owned_as_required_and_not_writable_by_others() {
    let rig = rig();
    assert!(rig.host.enabled() || RELEASE_ACTIVATION_ENABLED);
    let mut host = rig.host.clone();
    host.marker_owner_uid = host.marker_owner_uid.wrapping_add(1);
    assert!(!host.enabled());
    let host = rig.host.clone();
    std::fs::set_permissions(
        &host.test_profile_marker,
        std::fs::Permissions::from_mode(0o664),
    )
    .unwrap();
    assert!(!host.enabled());
    let mut host = rig.host.clone();
    let link = rig.dir.path().join("marker-link");
    std::os::unix::fs::symlink(&host.test_profile_marker, &link).unwrap();
    host.test_profile_marker = link;
    assert!(!host.enabled());
}

#[test]
fn an_irreversible_activation_needs_the_admins_acknowledgement() {
    let rig = rig();
    let broker = rig.broker(vec![Reply::Answer(planned(true))]);
    let Err(Failure::Invalid(message)) = rig.run(&rig.command()) else {
        panic!("an unacknowledged irreversible activation went ahead");
    };
    assert!(message.contains("irreversible"), "{message}");
    assert_eq!(broker.join().unwrap().len(), 1, "only the plan was asked");
    rig.nothing_written();
}

#[test]
fn an_acknowledged_irreversible_activation_takes_a_backup_first() {
    let rig = rig();
    let broker = rig.broker(vec![Reply::Answer(planned(true)), Reply::Answer(armed())]);
    let mut command = rig.command();
    command.acknowledge_irreversible = true;
    let Outcome::Armed { activation, .. } = rig.run(&command).unwrap() else {
        panic!("not armed");
    };
    let intents = broker.join().unwrap();
    assert!(activation.irreversible);
    let digest = activation
        .backup_sha256
        .clone()
        .expect("backup hash on the row");
    assert_eq!(rig.backups(), [format!("{digest}.sqlite")]);
    assert_eq!(intents[1]["irreversible"]["backup_sha256"], digest);
    // The backup is the database as it was before the request was recorded.
    let copy = rusqlite::Connection::open_with_flags(
        rig.host.backup_dir.join(format!("{digest}.sqlite")),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let rows: i64 = copy
        .query_row("SELECT count(*) FROM generation_activations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn an_acknowledgement_for_a_reversible_activation_is_refused() {
    let rig = rig();
    let broker = rig.broker(vec![Reply::Answer(planned(false))]);
    let mut command = rig.command();
    command.acknowledge_irreversible = true;
    let Err(Failure::Invalid(message)) = rig.run(&command) else {
        panic!("a false acknowledgement was recorded");
    };
    assert!(message.contains("reversible"), "{message}");
    broker.join().unwrap();
    rig.nothing_written();
}

#[test]
fn a_refused_arm_abandons_the_row_with_the_brokers_reason() {
    let rig = rig();
    let broker = rig.broker(vec![
        Reply::Answer(planned(false)),
        Reply::Answer(refusal("root-b already has a boot entry")),
    ]);
    let Err(Failure::Refused { activation, reason }) = rig.run(&rig.command()) else {
        panic!("not refused");
    };
    broker.join().unwrap();
    assert!(reason.contains("boot entry"));
    let activation = activation.expect("the row exists");
    assert_eq!(activation.state, ActivationState::Abandoned);
    assert!(activation.detail.unwrap().contains("boot entry"));
    assert!(rig
        .store
        .lock()
        .unwrap()
        .pending_activation()
        .unwrap()
        .is_none());
}

#[test]
fn a_refused_plan_writes_nothing() {
    let rig = rig();
    let broker = rig.broker(vec![Reply::Answer(refusal(
        "the staged image's build_id is not the claimed one",
    ))]);
    let Err(Failure::Refused { activation, reason }) = rig.run(&rig.command()) else {
        panic!("not refused");
    };
    broker.join().unwrap();
    assert!(activation.is_none());
    assert!(reason.contains("build_id"));
    rig.nothing_written();
}

#[test]
fn an_unreachable_broker_writes_nothing() {
    let rig = rig();
    let Err(Failure::Refused { activation, reason }) = rig.run(&rig.command()) else {
        panic!("not refused");
    };
    assert!(activation.is_none());
    assert!(reason.contains("broker"), "{reason}");
    rig.nothing_written();
}

#[test]
fn an_unanswered_arm_leaves_the_row_pending_as_an_unknown_outcome() {
    let rig = rig();
    let broker = rig.broker(vec![Reply::Answer(planned(false)), Reply::Hangup]);
    let Err(Failure::Unknown { activation, .. }) = rig.run(&rig.command()) else {
        panic!("not unknown");
    };
    broker.join().unwrap();
    assert_eq!(activation.state, ActivationState::Pending);
    assert_eq!(rig.history().len(), 1);
}

#[test]
fn a_retry_with_the_same_operation_id_reads_the_row_and_asks_no_broker() {
    let rig = rig();
    let broker = rig.broker(vec![Reply::Answer(planned(false)), Reply::Answer(armed())]);
    let mut command = rig.command();
    command.operation_id = Some("op-1".to_owned());
    let Outcome::Armed { activation, .. } = rig.run(&command).unwrap() else {
        panic!("not armed");
    };
    broker.join().unwrap();
    // No broker is listening any more: a retry must not need one.
    let Outcome::AlreadyRequested(again) = rig.run(&command).unwrap() else {
        panic!("not read back");
    };
    assert_eq!(again.id, activation.id);
    // The same key for another request is a conflict.
    let mut other = command.clone();
    other.kind = ActivationKind::Rollback;
    assert!(matches!(rig.run(&other), Err(Failure::Storage(_))));
    assert_eq!(rig.history().len(), 1);
}

#[test]
fn a_request_the_database_refuses_is_refused_before_any_backup_or_arm() {
    let rig = rig();
    // The candidate already fell back once; a new attempt must acknowledge it.
    {
        let mut store = rig.store.lock().unwrap();
        let request = ActivationRequest {
            generation: rig.generation.clone(),
            kind: ActivationKind::Activate,
            reason: "first".to_owned(),
            operation_id: None,
            expected_slot: "root-b".to_owned(),
            expected_root_partuuid: PART_B.to_owned(),
            ledger_sequence: 0,
            ledger_head: ActivationRequest::empty_ledger_head(),
            irreversible: false,
            backup_sha256: None,
            acknowledges: None,
        };
        let first = store.request_activation(ADMIN, &request).unwrap();
        store.record_fell_back(first.id, None, "came back").unwrap();
    }
    let broker = rig.broker(vec![Reply::Answer(planned(true))]);
    let mut command = rig.command();
    command.acknowledge_irreversible = true;
    let Err(Failure::Storage(error)) = rig.run(&command) else {
        panic!("not refused by the database");
    };
    assert!(error.to_string().contains("FELL_BACK"), "{error}");
    assert_eq!(broker.join().unwrap().len(), 1, "no arm");
    assert_eq!(rig.history().len(), 1, "only the earlier row");
    assert!(rig.backups().is_empty());
}

#[test]
fn a_ledger_that_cannot_be_verified_stops_the_request_before_the_broker() {
    let rig = rig();
    std::fs::set_permissions(
        &rig.host.ledger.path,
        std::fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    let Err(Failure::Unavailable(message)) = rig.run(&rig.command()) else {
        panic!("went ahead without a verified ledger");
    };
    assert!(message.contains("ledger"), "{message}");
    rig.nothing_written();
}

#[test]
fn a_generation_without_a_complete_identity_is_refused_before_the_broker() {
    let rig = rig();
    let bare = Generation {
        id: GenerationId::new(),
        parent: None,
        source_commit: None,
        system_manifest: None,
        reason_goal: None,
        experiment: None,
        status: GenerationStatus::Candidate,
        created_at: Timestamp::now(),
        owner: Owner::System,
    };
    rig.store.lock().unwrap().insert_generation(&bare).unwrap();
    let mut command = rig.command();
    command.generation = bare.id;
    let Err(Failure::Invalid(message)) = rig.run(&command) else {
        panic!("went ahead without an identity");
    };
    assert!(message.contains("identity"), "{message}");
    rig.nothing_written();
}

#[test]
fn the_broker_client_bounds_the_answer_and_reads_one_line() {
    let rig = rig();
    let listener = UnixListener::bind(&rig.host.broker_socket).unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let _ = (&stream).write_all(&vec![b'x'; MAX_ANSWER + 10]);
    });
    let error = call_broker(&rig.host.broker_socket, &json!({"intent": "plan"})).unwrap_err();
    server.join().unwrap();
    assert!(matches!(error, BrokerError::Unknown(_)), "{error:?}");
}
