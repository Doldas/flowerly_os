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
            serde_json::json!({
                "capabilities": CAPABILITIES,
                "enforced": false,
                "kind": "descriptive_catalog",
                "authorization": "not_implemented",
                "warning": "Capability descriptors are not grants, permissions or policy enforcement"
            }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use flowerly_core::ipc::PROTOCOL_VERSION;

    #[test]
    fn catalog_never_claims_enforcement() {
        let request = Envelope {
            version: PROTOCOL_VERSION,
            id: "catalog".into(),
            payload: Payload::Command {
                name: "capability.list".into(),
                args: serde_json::json!({}),
            },
        };
        let reply = handle_command(&request);
        match reply.payload {
            Payload::Ok { data } => {
                assert_eq!(data["enforced"], false);
                assert_eq!(data["kind"], "descriptive_catalog");
                assert_eq!(data["authorization"], "not_implemented");
                assert!(!data["capabilities"].as_array().unwrap().is_empty());
            }
            other => panic!("unexpected reply: {other:?}"),
        }
    }
}
