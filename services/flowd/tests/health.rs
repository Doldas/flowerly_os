use std::process::Command;
use std::time::{Duration, Instant};

use agentic_ipc::Payload;

fn start_daemon(runtime_dir: &std::path::Path, state_dir: &std::path::Path) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_flowd"))
        .env("AGENTIC_RUNTIME_DIR", runtime_dir)
        .env("AGENTIC_STATE_DIR", state_dir)
        .env("AGENTIC_LOG_FORMAT", "text")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn flowd")
}

fn wait_for_socket(path: &std::path::Path, timeout: Duration) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < timeout,
            "daemon did not create socket {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn flowd_self_test_passes() {
    let output = Command::new(env!("CARGO_BIN_EXE_flowd"))
        .arg("--self-test")
        .output()
        .expect("run flowd --self-test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "flowd --self-test failed: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("flowd self-test ok"));
}

#[test]
fn flowd_starts_and_answers_status() {
    let dir = tempfile::tempdir().expect("temp dir");
    let socket = dir.path().join("flowd.sock");
    let state_dir = dir.path().join("state");
    let mut child = start_daemon(dir.path(), &state_dir);
    wait_for_socket(&socket, Duration::from_secs(15));

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let status = rt
        .block_on(agentic_ipc::call(&socket, Payload::Status))
        .expect("status request");
    let data = match status.payload {
        Payload::Ok { data } => data,
        other => panic!("expected ok, got {other:?}"),
    };
    assert_eq!(data["service"], "flowd");
    assert_eq!(data["details"]["default_flow"], "General");

    let created = rt
        .block_on(agentic_ipc::call(
            &socket,
            Payload::Command {
                name: "flow.create".into(),
                args: serde_json::json!({ "name": "Development" }),
            },
        ))
        .expect("flow.create request");
    let created = match created.payload {
        Payload::Ok { data } => data,
        other => panic!("expected ok, got {other:?}"),
    };
    assert_eq!(created["name"], "Development");

    let list = rt
        .block_on(agentic_ipc::call(
            &socket,
            Payload::Command {
                name: "flow.list".into(),
                args: serde_json::json!({}),
            },
        ))
        .expect("flow.list request");
    let list = match list.payload {
        Payload::Ok { data } => data,
        other => panic!("expected ok, got {other:?}"),
    };
    assert_eq!(list["count"], 2);

    let _ = rt.block_on(agentic_ipc::call(&socket, Payload::Shutdown));

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_status) => break,
            None if Instant::now() > deadline => {
                let _ = child.kill();
                panic!("flowd did not exit after shutdown");
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}


#[test]
fn durable_flow_state_survives_runtime_restart() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state_dir = dir.path().join("state");
    let runtime_a = dir.path().join("runtime-a");
    let runtime_b = dir.path().join("runtime-b");
    std::fs::create_dir_all(&runtime_a).expect("create first runtime dir");
    std::fs::create_dir_all(&runtime_b).expect("create second runtime dir");

    let socket_a = runtime_a.join("flowd.sock");
    let mut first = start_daemon(&runtime_a, &state_dir);
    wait_for_socket(&socket_a, Duration::from_secs(15));

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let created = rt
        .block_on(agentic_ipc::call(
            &socket_a,
            Payload::Command {
                name: "flow.create".into(),
                args: serde_json::json!({ "name": "Persistent Development" }),
            },
        ))
        .expect("create persistent flow");
    assert!(matches!(created.payload, Payload::Ok { .. }));

    let _ = rt.block_on(agentic_ipc::call(&socket_a, Payload::Shutdown));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match first.try_wait().expect("try_wait first daemon") {
            Some(status) => {
                assert!(status.success(), "first flowd exited unsuccessfully: {status}");
                break;
            }
            None if Instant::now() > deadline => {
                let _ = first.kill();
                panic!("first flowd did not exit after shutdown");
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }

    let socket_b = runtime_b.join("flowd.sock");
    let mut second = start_daemon(&runtime_b, &state_dir);
    wait_for_socket(&socket_b, Duration::from_secs(15));

    let list = rt
        .block_on(agentic_ipc::call(
            &socket_b,
            Payload::Command {
                name: "flow.list".into(),
                args: serde_json::json!({}),
            },
        ))
        .expect("list flows after restart");
    let data = match list.payload {
        Payload::Ok { data } => data,
        other => panic!("expected ok, got {other:?}"),
    };
    let flows = data["flows"].as_array().expect("flows array");
    assert!(
        flows
            .iter()
            .any(|flow| flow["name"] == "Persistent Development"),
        "durable Flow disappeared across runtime restart: {data}"
    );

    let _ = rt.block_on(agentic_ipc::call(&socket_b, Payload::Shutdown));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match second.try_wait().expect("try_wait second daemon") {
            Some(status) => {
                assert!(status.success(), "second flowd exited unsuccessfully: {status}");
                break;
            }
            None if Instant::now() > deadline => {
                let _ = second.kill();
                panic!("second flowd did not exit after shutdown");
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}
