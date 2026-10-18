use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::{coded_error_envelope, ok_envelope};

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
        Payload::Command { name, .. } => coded_error_envelope(
            request.id.clone(),
            ErrorCode::InvalidArgument,
            format!("capabilityd: unknown command `{name}`"),
        ),
        _ => coded_error_envelope(
            request.id.clone(),
            ErrorCode::InvalidArgument,
            "capabilityd: unexpected request type",
        ),
    }
}
