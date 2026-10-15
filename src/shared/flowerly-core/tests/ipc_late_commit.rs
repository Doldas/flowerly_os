//! Prove that an IPC deadline cannot be treated as mutation rollback.
//! A blocking worker intentionally commits after its caller stops waiting.
//! This is a regression characterization, NOT idempotent request handling.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flowerly_core::ipc::{call, ErrorCode, Payload};
use flowerly_core::storage::Store;
use flowerly_core::types::Flow;
use flowerly_core::{ok_envelope, run_service, ServiceConfig};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_timed_out_mutation_can_commit_later_and_naive_retry_duplicates_it() {
    let dir = tempfile::tempdir().expect("test dir");
    let config = ServiceConfig {
        name: "late-commit".to_owned(),
        runtime_dir: dir.path().join("runtime"),
        state_dir: dir.path().join("state"),
        database_path: None,
        log_format: flowerly_core::LogFormat::Text,
    };
    let db_path = config.database_path();
    let mut initial = Store::open(&db_path).expect("open");
    initial.migrate().expect("migrate");
    drop(initial);

    let store = Arc::new(Mutex::new(Store::open(&db_path).expect("worker store")));
    let started = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let worker_store = Arc::clone(&store);
    let worker_started = Arc::clone(&started);
    let worker_completed = Arc::clone(&completed);
    let handler = Arc::new(move |request: &flowerly_core::ipc::Envelope| {
        let call_number = worker_started.fetch_add(1, Ordering::SeqCst);
        if call_number == 0 {
            // The server's request deadline is 10 seconds. Hold this worker
            // beyond it so that the caller cannot know whether it committed.
            std::thread::sleep(Duration::from_secs(11));
        }
        let flow = Flow::new("Same user intention", "timeout retry demonstration");
        worker_store
            .lock()
            .expect("lock")
            .insert_flow(&flow)
            .expect("insert");
        worker_completed.fetch_add(1, Ordering::SeqCst);
        ok_envelope(request.id.clone(), serde_json::json!({"id": flow.id}))
    });
    let server = tokio::spawn(run_service(config.clone(), serde_json::json!({}), handler));
    for _ in 0..100 {
        if config.socket_path().exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(config.socket_path().exists(), "socket started");

    let began = std::time::Instant::now();
    let first = call(
        &config.socket_path(),
        Payload::Command {
            name: "create".into(),
            args: serde_json::json!({}),
        },
    )
    .await;
    assert!(
        began.elapsed() < Duration::from_secs(11),
        "request must be bounded"
    );
    // Depending on timing, server may send a typed unavailable, or the
    // client's own deadline may elapse before it receives that response.
    match first {
        Ok(envelope) => assert!(
            matches!(
                envelope.payload,
                Payload::Err {
                    code: Some(ErrorCode::Unavailable),
                    ..
                }
            ),
            "unexpected response: {envelope:?}"
        ),
        Err(flowerly_core::ipc::IpcError::Timeout) => {}
        Err(other) => panic!("unexpected transport error: {other}"),
    }
    // The original mutation is still in flight: wait for the real commit.
    for _ in 0..300 {
        if completed.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(completed.load(Ordering::SeqCst), 1, "late write committed");

    let retry = call(
        &config.socket_path(),
        Payload::Command {
            name: "create".into(),
            args: serde_json::json!({}),
        },
    )
    .await
    .expect("naive retry");
    assert!(matches!(retry.payload, Payload::Ok { .. }));
    assert_eq!(completed.load(Ordering::SeqCst), 2);
    let stored = Store::open(&db_path)
        .expect("reader")
        .list_flows()
        .expect("flows");
    let duplicates = stored
        .iter()
        .filter(|flow| flow.name == "Same user intention")
        .count();
    assert_eq!(duplicates, 2, "same intended operation was applied twice");
    eprintln!(
        "late-commit demonstration: first request timed out in {:?}; two distinct durable flow rows after retry",
        began.elapsed()
    );

    let response = call(&config.socket_path(), Payload::Shutdown)
        .await
        .expect("shutdown");
    assert!(matches!(response.payload, Payload::Ok { .. }));
    server.await.expect("server join").expect("server shutdown");
}
