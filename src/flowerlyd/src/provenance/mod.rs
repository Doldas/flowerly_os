use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::{StorageError, Store};
use flowerly_core::types::Caller;
use flowerly_core::types::{ObjectRef, ProvenanceEdge, ProvenanceRelation, Timestamp};
use flowerly_core::{coded_error_envelope, error_envelope, ok_envelope};
use serde_json::Value;

/// Provenance reads are filtered by the visibility of both endpoints to the
/// kernel-attested caller (#225 PR 4). `provenance.add` is an audited Admin
/// command and writes unscoped.
pub(crate) fn handle_command(
    store: &Arc<Mutex<Store>>,
    request: &Envelope,
    caller: Caller,
) -> Envelope {
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
    match run_command(&store, name, args, caller) {
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

fn run_command(
    store: &Store,
    name: &str,
    args: &Value,
    caller: Caller,
) -> Result<Value, CommandFailure> {
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
                .provenance_from_for(caller, &object_ref)
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
                .provenance_to_for(caller, &object_ref)
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
    use flowerly_core::types::{Execution, ExecutionId, Goal, GoalId};

    fn migrated_store() -> Store {
        let mut store = Store::open_in_memory().expect("open store");
        store.migrate().expect("migrate store");
        store
    }

    const TEST_CALLER: Caller = Caller::new(1000);
    const OTHER: Caller = Caller::new(2000);

    /// A Goal and an Execution owned by `caller`, linked by one edge.
    fn linked(store: &Store, caller: Caller) -> (ExecutionId, GoalId) {
        let mut goal = Goal::new("g", caller.actor());
        goal.owner = caller.owner();
        store.insert_goal(&goal).expect("insert goal");
        let execution = Execution {
            id: ExecutionId::new(),
            agent_id: None,
            goal_id: Some(goal.id.clone()),
            reason: None,
            command: None,
            exit_code: None,
            duration_ms: None,
            started_at: Timestamp::now(),
            owner: caller.owner(),
        };
        store
            .insert_execution(&execution)
            .expect("insert execution");
        let edge = ProvenanceEdge {
            from: ObjectRef::from(&execution.id),
            relation: ProvenanceRelation::CausedBy,
            to: ObjectRef::from(&goal.id),
            created_at: Timestamp::now(),
            note: Some("test edge".to_owned()),
        };
        store.insert_provenance(&edge).expect("insert edge");
        (execution.id, goal.id)
    }

    fn count(store: &Store, command: &str, reference: &str, caller: Caller) -> Value {
        run_command(
            store,
            command,
            &serde_json::json!({ "ref": reference }),
            caller,
        )
        .expect("query")["count"]
            .clone()
    }

    #[test]
    fn provenance_queries_work_in_both_directions() {
        let store = migrated_store();
        let (execution, goal) = linked(&store, TEST_CALLER);

        let from = run_command(
            &store,
            "provenance.from",
            &serde_json::json!({ "ref": execution }),
            TEST_CALLER,
        )
        .expect("query outgoing");
        assert_eq!(from["count"], 1);
        assert_eq!(from["edges"][0]["to"], serde_json::json!(goal));

        let to = run_command(
            &store,
            "provenance.to",
            &serde_json::json!({ "ref": goal }),
            TEST_CALLER,
        )
        .expect("query incoming");
        assert_eq!(to["count"], 1);
        assert_eq!(to["edges"][0]["from"], serde_json::json!(execution));
    }

    #[test]
    fn another_owners_provenance_is_answered_like_a_missing_reference() {
        let store = migrated_store();
        let (execution, goal) = linked(&store, OTHER);
        for (command, reference) in [
            ("provenance.from", execution.to_string()),
            ("provenance.to", goal.to_string()),
            ("provenance.from", ExecutionId::new().to_string()),
        ] {
            assert_eq!(
                count(&store, command, &reference, TEST_CALLER),
                0,
                "{command}"
            );
        }
        assert_eq!(
            count(&store, "provenance.from", execution.as_str(), OTHER),
            1
        );
        // Another uid's actor label never reveals that uid's objects.
        let actor = ObjectRef::actor("uid:2000").unwrap();
        store
            .insert_provenance(&ProvenanceEdge {
                from: ObjectRef::from(&goal),
                relation: ProvenanceRelation::CreatedBy,
                to: actor.clone(),
                created_at: Timestamp::now(),
                note: None,
            })
            .unwrap();
        assert_eq!(
            count(&store, "provenance.to", actor.as_str(), TEST_CALLER),
            0
        );
        assert_eq!(count(&store, "provenance.to", actor.as_str(), OTHER), 1);
    }

    #[test]
    fn an_edge_needs_both_endpoints_visible_and_one_positively() {
        let store = migrated_store();
        let (mine, _) = linked(&store, TEST_CALLER);
        let (theirs, _) = linked(&store, OTHER);
        let agent = flowerly_core::types::AgentId::new();
        for edge in [
            // own -> other's: hidden (the other endpoint is not visible)
            (ObjectRef::from(&mine), ObjectRef::from(&theirs)),
            // own -> unowned kind: visible
            (ObjectRef::from(&mine), ObjectRef::from(&agent)),
            // unowned -> actor label: neutral on both sides, hidden
            (
                ObjectRef::from(&agent),
                ObjectRef::actor("planner").unwrap(),
            ),
        ] {
            store
                .insert_provenance(&ProvenanceEdge {
                    from: edge.0,
                    relation: ProvenanceRelation::BelongsTo,
                    to: edge.1,
                    created_at: Timestamp::now(),
                    note: None,
                })
                .unwrap();
        }
        // The fixture edge to its own Goal plus the edge to the agent.
        assert_eq!(
            count(&store, "provenance.from", mine.as_str(), TEST_CALLER),
            2
        );
        assert_eq!(
            count(&store, "provenance.to", theirs.as_str(), TEST_CALLER),
            0
        );
        assert_eq!(
            count(&store, "provenance.from", agent.as_str(), TEST_CALLER),
            0
        );
        assert_eq!(
            count(&store, "provenance.to", agent.as_str(), TEST_CALLER),
            1
        );
        assert_eq!(count(&store, "provenance.to", agent.as_str(), OTHER), 0);
    }

    #[test]
    fn provenance_query_rejects_untyped_references() {
        let store = migrated_store();
        let error = run_command(
            &store,
            "provenance.to",
            &serde_json::json!({ "ref": "plain-string" }),
            TEST_CALLER,
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
