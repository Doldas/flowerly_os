
use flowerly_core::ipc::{Envelope, Payload};
use flowerly_core::ok_envelope;

const CAPABILITIES: &[&str] = &[
    "file.read",
    "file.write_workspace",
    "process.run",
    "system.query_state",
    "goal.query",
    "goal.create_child",
    "session.query",
    "provenance.query",
    "generation.create_candidate",
    "evolution.run_experiment",
];

pub(crate) fn handle_command(request: &Envelope) -> Envelope {
    match &request.payload {
        Payload::Command { name, .. } if name == "capability.list" => ok_envelope(
            request.id.clone(),
            serde_json::json!({"capabilities": CAPABILITIES}),
        ),
        _ => flowerly_core::error_envelope(request.id.clone(), "unknown capability command"),
    }
}
