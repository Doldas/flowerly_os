use std::process::Command;
use std::time::{Duration, Instant};
use flowerly_core::ipc::{self, Payload};

fn wait_for_socket(path: &std::path::Path) {
    let started = Instant::now();
    while !path.exists() {
        assert!(started.elapsed() < Duration::from_secs(15), "supervisor socket missing");
        std::thread::sleep(Duration::from_millis(80));
    }
}

#[test]
fn unified_supervisor_preserves_flows_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let mut runtime_dirs = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    for attempt in 0..2 {
        let runtime = temp.path().join(format!("runtime-{attempt}"));
        std::fs::create_dir_all(&runtime).unwrap();
        runtime_dirs.push(runtime.clone());
        let mut child = Command::new(env!("CARGO_BIN_EXE_flowerlyd"))
            .env("AGENTIC_RUNTIME_DIR", &runtime)
            .env("AGENTIC_STATE_DIR", &state)
            .spawn().unwrap();
        let socket = runtime.join("flowerlyd.sock");
        wait_for_socket(&socket);
        if attempt == 0 {
            let reply = rt.block_on(ipc::call(&socket, Payload::Command {
                name: "flow.create".into(),
                args: serde_json::json!({"name":"Durable Flow"}),
            })).unwrap();
            assert!(matches!(reply.payload, Payload::Ok { .. }));
        }
        let reply = rt.block_on(ipc::call(&socket, Payload::Command {
            name: "flow.list".into(),
            args: serde_json::json!({}),
        })).unwrap();
        let Payload::Ok { data } = reply.payload else {
            panic!("flow.list returned an error");
        };
        assert!(data["flows"].as_array().unwrap().iter().any(|f| f["name"] == "Durable Flow"));
        let _ = rt.block_on(ipc::call(&socket, Payload::Shutdown)).unwrap();
        assert!(child.wait().unwrap().success());
    }
}
