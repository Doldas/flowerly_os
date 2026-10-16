//! Cross-service, file-backed SQLite lock contention test of the IPC boundary.
//! Actual independent connections share one writer domain; the fast Ping path
//! must remain available while two handlers wait behind an external writer.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flowerly_core::ipc::{call, ErrorCode, Payload};
use flowerly_core::storage::Store;
use flowerly_core::types::Flow;
use flowerly_core::{coded_error_envelope, ok_envelope, run_service, ServiceConfig};
use rusqlite::Connection;

fn config(name: &str, dir: &std::path::Path) -> ServiceConfig {
    ServiceConfig {
        name: name.to_owned(),
        runtime_dir: dir.join("runtime"),
        state_dir: dir.join("state"),
        database_path: None,
        log_format: flowerly_core::LogFormat::Text,
    }
}

async fn wait_for_socket(path: &std::path::Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("socket {} never appeared", path.display());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_writer_lock_does_not_starve_other_service_ping() {
    let dir = tempfile::tempdir().unwrap();
    let first = config("writer-a", dir.path());
    let second = config("writer-b", dir.path());
    let db_path = first.database_path();
    let mut initial = Store::open(&db_path).unwrap();
    initial.migrate().unwrap();
    drop(initial);

    let external = Connection::open(&db_path).unwrap();
    external.execute_batch("BEGIN IMMEDIATE;").unwrap();

    let active = Arc::new(Mutex::new(Vec::new()));
    let mut servers = Vec::new();
    for cfg in [first.clone(), second.clone()] {
        let store = Arc::new(Mutex::new(Store::open(&db_path).unwrap()));
        let active_for_handler = Arc::clone(&active);
        let handler = Arc::new(move |req: &flowerly_core::ipc::Envelope| {
            let Payload::Command { .. } = &req.payload else {
                return coded_error_envelope(
                    req.id.clone(),
                    ErrorCode::InvalidArgument,
                    "unexpected payload",
                );
            };
            let begin = Instant::now();
            let result = store
                .lock()
                .unwrap()
                .insert_flow(&Flow::new("Contended", "test"));
            active_for_handler.lock().unwrap().push(begin.elapsed());
            match result {
                Ok(()) => ok_envelope(req.id.clone(), serde_json::json!({"written": true})),
                Err(err) => {
                    coded_error_envelope(req.id.clone(), ErrorCode::Unavailable, err.to_string())
                }
            }
        });
        servers.push(tokio::spawn(run_service(
            cfg.clone(),
            serde_json::json!({}),
            handler,
        )));
        wait_for_socket(&cfg.socket_path()).await;
    }

    let socket_a = first.socket_path();
    let socket_b = second.socket_path();
    let stalled_a = tokio::spawn(async move {
        call(
            &socket_a,
            Payload::Command {
                name: "write".into(),
                args: serde_json::json!({}),
            },
        )
        .await
    });
    let stalled_b = tokio::spawn(async move {
        call(
            &socket_b,
            Payload::Command {
                name: "write".into(),
                args: serde_json::json!({}),
            },
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(180)).await;
    let started = Instant::now();
    let pong = tokio::time::timeout(
        Duration::from_secs(2),
        call(&first.socket_path(), Payload::Ping),
    )
    .await
    .expect("Ping must not wait for contended SQLite handler")
    .expect("Ping should be delivered");
    assert!(matches!(pong.payload, Payload::Ok { .. }));
    assert!(started.elapsed() < Duration::from_secs(2));

    external.execute_batch("ROLLBACK").unwrap();
    let a = stalled_a.await.unwrap().unwrap();
    let b = stalled_b.await.unwrap().unwrap();
    assert!(matches!(a.payload, Payload::Ok { .. }), "{a:?}");
    assert!(matches!(b.payload, Payload::Ok { .. }), "{b:?}");
    assert_eq!(active.lock().unwrap().len(), 2);

    for cfg in [first, second] {
        let result = call(&cfg.socket_path(), Payload::Shutdown).await.unwrap();
        assert!(matches!(result.payload, Payload::Ok { .. }));
    }
    for server in servers {
        server.await.unwrap().unwrap();
    }
}
