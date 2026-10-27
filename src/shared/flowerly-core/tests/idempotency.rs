//! Durable, transaction-scoped operation replay across SQLite connections.
//! The create + operation record must be atomic even under concurrent retries.
use std::sync::{Arc, Barrier};

use flowerly_core::storage::{MutationOperation, StorageError, Store};
use flowerly_core::types::Flow;

fn operation<'a>(
    actor: &'a str,
    operation_id: &'a str,
    request_payload: &'a str,
    response_payload: &'a str,
) -> MutationOperation<'a> {
    MutationOperation {
        actor,
        command: "flow.create",
        operation_id,
        request_payload,
        response_payload,
    }
}

#[test]
fn actor_scoped_retry_replays_original_result_after_restart() {
    let dir = tempfile::tempdir().expect("dir");
    let db = dir.path().join("state.db");
    let mut store = Store::open(&db).expect("open");
    store.migrate().expect("migrate");

    let first = Flow::new("Plan", "uid:1000");
    let original_result = format!(r#"{{"id":"{}"}}"#, first.id);
    let key = operation(
        "uid:1000",
        "request-1",
        r#"{"name":"Plan"}"#,
        &original_result,
    );
    let result = store
        .insert_flow_with_provenance_once(&first, &[], &key)
        .expect("first commit");
    assert_eq!(result, original_result);
    drop(store);

    let mut reopened = Store::open(&db).expect("reopen");
    reopened.migrate().expect("already migrated");
    let retry = Flow::new("Plan", "uid:1000");
    let retry_result = format!(r#"{{"id":"{}"}}"#, retry.id);
    let same_key = operation("uid:1000", "request-1", r#"{"name":"Plan"}"#, &retry_result);
    let replayed = reopened
        .insert_flow_with_provenance_once(&retry, &[], &same_key)
        .expect("replayed result");
    assert_eq!(replayed, original_result);
    assert_eq!(reopened.list_flows().expect("list").len(), 1);

    let mismatch = operation(
        "uid:1000",
        "request-1",
        r#"{"name":"Different"}"#,
        &retry_result,
    );
    assert!(matches!(
        reopened.insert_flow_with_provenance_once(&retry, &[], &mismatch),
        Err(StorageError::OperationConflict(_))
    ));
    assert_eq!(reopened.list_flows().expect("list").len(), 1);

    // A different trusted actor has a separate namespace for the same key.
    let other = Flow::new("Plan", "uid:2000");
    let other_result = format!(r#"{{"id":"{}"}}"#, other.id);
    let other_key = operation("uid:2000", "request-1", r#"{"name":"Plan"}"#, &other_result);
    assert_eq!(
        reopened
            .insert_flow_with_provenance_once(&other, &[], &other_key)
            .expect("other actor"),
        other_result
    );
    assert_eq!(reopened.list_flows().expect("list").len(), 2);
}

#[test]
fn concurrent_same_actor_operation_only_creates_one_flow() {
    let dir = tempfile::tempdir().expect("dir");
    let db = dir.path().join("shared.db");
    let mut initial = Store::open(&db).expect("open");
    initial.migrate().expect("migrate");
    drop(initial);

    let gate = Arc::new(Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let path = db.clone();
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                let mut store = Store::open(path).expect("writer store");
                let flow = Flow::new("One operation", "uid:42");
                let response = format!(r#"{{"id":"{}"}}"#, flow.id);
                let key = operation(
                    "uid:42",
                    "the-same-key",
                    r#"{"name":"One operation"}"#,
                    &response,
                );
                gate.wait();
                store
                    .insert_flow_with_provenance_once(&flow, &[], &key)
                    .expect("idempotent insert")
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().expect("join"))
        .collect();
    assert_eq!(results[0], results[1]);
    let store = Store::open(db).expect("read");
    assert_eq!(store.list_flows().expect("list").len(), 1);
}

#[test]
fn unkeyed_spawn_records_are_recoverable_but_never_a_retry_identity() {
    use flowerly_core::types::SessionId;
    let dir = tempfile::tempdir().expect("dir");
    let db = dir.path().join("state.db");
    let mut store = Store::open(&db).expect("open");
    store.migrate().expect("migrate");

    let keyed = SessionId::new();
    let crashed = SessionId::new();
    let completed = SessionId::new();
    assert!(store
        .reserve_external_spawn("uid:1000", "spawn-1", r#"{"shell":"/bin/sh"}"#, &keyed)
        .expect("keyed"));
    store
        .reserve_unkeyed_spawn("uid:1000", &crashed)
        .expect("crashed");
    store
        .reserve_unkeyed_spawn("uid:1000", &completed)
        .expect("completed");
    store.release_unkeyed_spawn(&completed).expect("release");

    // A restart (new connection) sees every launch whose cgroup may survive.
    drop(store);
    let mut store = Store::open(&db).expect("reopen");
    let mut listed = store.list_external_spawn_reservations().expect("list");
    listed.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    let mut expected = vec![keyed.clone(), crashed.clone()];
    expected.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    assert_eq!(listed, expected);

    // An unkeyed record is not reachable as an operation_id lookup.
    assert_eq!(
        store
            .lookup_external_spawn("uid:1000", crashed.as_str())
            .expect("lookup"),
        None
    );

    // Recovery clears unkeyed records only; the keyed reservation is kept
    // forever so a late retry still cannot launch a second shell.
    assert_eq!(store.clear_unkeyed_spawn_reservations().expect("clear"), 1);
    assert_eq!(
        store.list_external_spawn_reservations().expect("list"),
        vec![keyed.clone()]
    );
    assert!(!store
        .reserve_external_spawn(
            "uid:1000",
            "spawn-1",
            r#"{"shell":"/bin/sh"}"#,
            &SessionId::new()
        )
        .expect("retry"));
}
