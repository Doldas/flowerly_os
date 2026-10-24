use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::{StorageError, Store};
use flowerly_core::types::{ObjectRef, ProvenanceEdge, ProvenanceRelation, Timestamp};
use flowerly_core::{coded_error_envelope, error_envelope, ok_envelope};
use serde_json::Value;

pub(crate) fn handle_command(store: &Arc<Mutex<Store>>, request: &Envelope) -> Envelope {
    let Payload::Command { name, args } = &request.payload else {
        return error_envelope(request.id.clone(), "unexpected request type");
    };
    let store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_envelope(request.id.clone(), "storage lock poisoned"),
    };
    if let Err((code, message)) = validate_provenance_command(name, args) {
        return coded_error_envelope(request.id.clone(), code, message);
    }
    match run_command(&store, name, args) {
        Ok(data) => ok_envelope(request.id.clone(), data),
        Err(error) => coded_error_envelope(request.id.clone(), error.code, error.message),
    }
}

/// Validate client-facing provenance errors while the store lock is held.
fn validate_provenance_command(name: &str, args: &Value) -> Result<(), (ErrorCode, String)> {
    let invalid = |message: &str| (ErrorCode::InvalidArgument, message.to_owned());
    match name {
        "provenance.add" => {
            let from = args
                .get("from")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("missing string arg `from`"))?;
            ObjectRef::from_str(from).map_err(|e| invalid(&e.to_string()))?;
            let relation = args
                .get("relation")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("missing string arg `relation`"))?;
            parse_relation(relation).map_err(|e| invalid(&e))?;
            let to = args
                .get("to")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("missing string arg `to`"))?;
            ObjectRef::from_str(to).map_err(|e| invalid(&e.to_string()))?;
            if let Some(note) = args.get("note").filter(|value| !value.is_null()) {
                note.as_str()
                    .ok_or_else(|| invalid("arg `note` must be a string or null"))?;
            }
        }
        "provenance.from" | "provenance.to" => {
            let key = if name == "provenance.to" {
                "to"
            } else {
                "from"
            };
            let raw = args
                .get("ref")
                .or_else(|| args.get(key))
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("missing string arg `ref`"))?;
            ObjectRef::from_str(raw).map_err(|e| invalid(&e.to_string()))?;
        }
        other => {
            return Err((
                ErrorCode::InvalidArgument,
                format!("provenanced: unknown command `{other}`"),
            ));
        }
    }
    Ok(())
}

fn storage_error_code(error: &StorageError) -> ErrorCode {
    error.ipc_error_code()
}

#[derive(Debug)]
struct CommandFailure {
    code: ErrorCode,
    message: String,
}

impl CommandFailure {
    fn storage(error: StorageError) -> Self {
        Self {
            code: storage_error_code(&error),
            message: error.to_string(),
        }
    }
}

impl From<String> for CommandFailure {
    fn from(message: String) -> Self {
        Self {
            code: ErrorCode::Internal,
            message,
        }
    }
}

impl From<&str> for CommandFailure {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

impl std::fmt::Display for CommandFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[cfg(test)]
impl CommandFailure {
    fn contains(&self, pattern: &str) -> bool {
        self.message.contains(pattern)
    }
}

impl PartialEq<String> for CommandFailure {
    fn eq(&self, other: &String) -> bool {
        &self.message == other
    }
}

fn run_command(store: &Store, name: &str, args: &Value) -> Result<Value, CommandFailure> {
    match name {
        "provenance.add" => {
            let from = args
                .get("from")
                .and_then(Value::as_str)
                .ok_or("missing string arg `from`")?;
            let relation = args
                .get("relation")
                .and_then(Value::as_str)
                .ok_or("missing string arg `relation`")?;
            let to = args
                .get("to")
                .and_then(Value::as_str)
                .ok_or("missing string arg `to`")?;
            let relation = parse_relation(relation)?;
            let edge = ProvenanceEdge {
                from: ObjectRef::from_str(from).map_err(err_str)?,
                relation,
                to: ObjectRef::from_str(to).map_err(err_str)?,
                created_at: Timestamp::now(),
                note: args.get("note").and_then(Value::as_str).map(str::to_owned),
            };
            store
                .insert_provenance(&edge)
                .map_err(CommandFailure::storage)?;
            tracing::info!(from, relation = %relation, to, "provenance edge added");
            Ok(serde_json::json!({ "from": from, "relation": relation.to_string(), "to": to }))
        }
        "provenance.from" => {
            let from = args
                .get("ref")
                .or_else(|| args.get("from"))
                .and_then(Value::as_str)
                .ok_or("missing string arg `ref`")?;
            let object_ref = ObjectRef::from_str(from).map_err(err_str)?;
            let edges = store
                .provenance_from(&object_ref)
                .map_err(CommandFailure::storage)?;
            Ok(edges_response(&edges))
        }
        "provenance.to" => {
            let to = args
                .get("ref")
                .or_else(|| args.get("to"))
                .and_then(Value::as_str)
                .ok_or("missing string arg `ref`")?;
            let object_ref = ObjectRef::from_str(to).map_err(err_str)?;
            let edges = store
                .provenance_to(&object_ref)
                .map_err(CommandFailure::storage)?;
            Ok(edges_response(&edges))
        }
        other => Err(format!("provenanced: unknown command `{other}`").into()),
    }
}

fn edges_response(edges: &[ProvenanceEdge]) -> Value {
    let items: Vec<Value> = edges
        .iter()
        .map(|edge| {
            serde_json::json!({
                "from": edge.from,
                "relation": edge.relation.to_string(),
                "to": edge.to,
                "note": edge.note,
            })
        })
        .collect();
    serde_json::json!({ "edges": items, "count": items.len() })
}

fn parse_relation(raw: &str) -> Result<ProvenanceRelation, String> {
    match raw {
        "created_by" => Ok(ProvenanceRelation::CreatedBy),
        "caused_by" => Ok(ProvenanceRelation::CausedBy),
        "belongs_to" => Ok(ProvenanceRelation::BelongsTo),
        "produced" => Ok(ProvenanceRelation::Produced),
        "tested_by" => Ok(ProvenanceRelation::TestedBy),
        "promoted_to" => Ok(ProvenanceRelation::PromotedTo),
        other => Err(format!("unknown relation `{other}`")),
    }
}

fn err_str(error: impl std::fmt::Display) -> String {
    error.to_string()
}

pub(crate) async fn self_test() -> anyhow::Result<()> {
    let dir = tempfile::tempdir().context("creating temp dir")?;
    let db = dir.path().join("provenanced-self-test.db");
    let mut store = Store::open(&db).context("opening self-test db")?;
    let applied = store.migrate()?;
    let edge = ProvenanceEdge {
        from: ObjectRef::from_str("exec-123")?,
        relation: ProvenanceRelation::Produced,
        to: ObjectRef::from_str("artifact-456")?,
        created_at: Timestamp::now(),
        note: None,
    };
    store.insert_provenance(&edge)?;
    store.insert_provenance(&edge)?;
    let edges = store.provenance_from(&ObjectRef::from_str("exec-123")?)?;
    assert_eq!(edges.len(), 1);
    println!(
        "provenanced self-test ok: migrations={} edges={}",
        applied.len(),
        edges.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowerly_core::types::{ExecutionId, GoalId};

    fn migrated_store() -> Store {
        let mut store = Store::open_in_memory().expect("open store");
        store.migrate().expect("migrate store");
        store
    }

    #[test]
    fn provenance_queries_work_in_both_directions() {
        let store = migrated_store();
        let execution = ExecutionId::new();
        let goal = GoalId::new();
        let edge = ProvenanceEdge {
            from: ObjectRef::from(&execution),
            relation: ProvenanceRelation::CausedBy,
            to: ObjectRef::from(&goal),
            created_at: Timestamp::now(),
            note: Some("test edge".to_owned()),
        };
        store.insert_provenance(&edge).expect("insert edge");

        let from = run_command(
            &store,
            "provenance.from",
            &serde_json::json!({ "ref": execution }),
        )
        .expect("query outgoing");
        assert_eq!(from["count"], 1);
        assert_eq!(from["edges"][0]["to"], serde_json::json!(goal));

        let to = run_command(&store, "provenance.to", &serde_json::json!({ "ref": goal }))
            .expect("query incoming");
        assert_eq!(to["count"], 1);
        assert_eq!(to["edges"][0]["from"], serde_json::json!(execution));
    }

    #[test]
    fn provenance_query_rejects_untyped_references() {
        let store = migrated_store();
        let error = run_command(
            &store,
            "provenance.to",
            &serde_json::json!({ "ref": "plain-string" }),
        )
        .expect_err("untyped reference must fail");
        assert!(
            error.contains("object reference"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn provenance_errors_have_explicit_codes() {
        let cases = [
            (
                "provenance.add",
                serde_json::json!({"relation": "caused_by", "to": "goal-1"}),
                ErrorCode::InvalidArgument,
            ),
            (
                "provenance.add",
                serde_json::json!({"from": "exec-123", "relation": "WAT", "to": "goal-1"}),
                ErrorCode::InvalidArgument,
            ),
            (
                "provenance.add",
                serde_json::json!({"from": "plain", "relation": "caused_by", "to": "goal-1"}),
                ErrorCode::InvalidArgument,
            ),
            (
                "provenance.add",
                serde_json::json!({"from": "exec-123", "relation": "caused_by", "to": "plain"}),
                ErrorCode::InvalidArgument,
            ),
            (
                "provenance.to",
                serde_json::json!({"ref": "plain-string"}),
                ErrorCode::InvalidArgument,
            ),
            (
                "provenance.from",
                serde_json::json!({}),
                ErrorCode::InvalidArgument,
            ),
            (
                "provenance.delete",
                serde_json::json!({}),
                ErrorCode::InvalidArgument,
            ),
        ];
        for (command, args, expected) in cases {
            let error = validate_provenance_command(command, &args).unwrap_err().0;
            assert_eq!(error, expected, "{command} {args}");
        }
        assert!(validate_provenance_command(
            "provenance.add",
            &serde_json::json!({"from": "exec-123", "relation": "caused_by", "to": "goal-1"}),
        )
        .is_ok());
    }
}
