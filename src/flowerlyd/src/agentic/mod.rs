#![allow(dead_code)]
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use clap::Parser;
use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::repository::{
    CoreCommandRepository, CoreQueryRepository, CreateExecution,
};
use flowerly_core::storage::{StorageError, Store};
use flowerly_core::types::{
    AgentId, Execution, ExecutionId, GoalId, ObjectRef, ProvenanceEdge, ProvenanceRelation,
    Timestamp,
};
use flowerly_core::{
    coded_error_envelope, error_envelope, init_tracing, ok_envelope, run_service, ServiceConfig,
};
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(name = "agenticd", version, about = "Flowerly OS orchestration daemon")]
struct Cli {
    #[arg(long, env = "AGENTIC_CONFIG")]
    config: Option<PathBuf>,
    #[arg(long)]
    self_test: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = match cli.config {
        Some(path) => ServiceConfig::load(&path).context("loading config")?,
        None => ServiceConfig::for_service("agenticd"),
    };
    init_tracing(&config.name, config.log_format);

    if cli.self_test {
        return self_test().await;
    }

    let store = open_store(&config)?;
    let extra = serde_json::json!({ "role": "orchestrator", "agents_started": 0 });
    let store = Arc::new(Mutex::new(store));
    let handler = move |request: &Envelope| -> Envelope { handle_command(&store, request) };

    run_service(config, extra, Arc::new(handler))
        .await
        .context("agenticd terminated abnormally")
}

fn open_store(config: &ServiceConfig) -> anyhow::Result<Store> {
    let db_path = config.database_path();
    let mut store = Store::open(&db_path).context("opening agenticd database")?;
    store.migrate().context("running agenticd migrations")?;
    store
        .health_check()
        .context("agenticd database health check")?;
    Ok(store)
}

pub(crate) fn handle_command(store: &Arc<Mutex<Store>>, request: &Envelope) -> Envelope {
    let Payload::Command { name, args } = &request.payload else {
        return error_envelope(request.id.clone(), "unexpected request type");
    };
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_envelope(request.id.clone(), "storage lock poisoned"),
    };
    match run_command(&mut store, name, args) {
        Ok(data) => ok_envelope(request.id.clone(), data),
        Err(error) => coded_error_envelope(request.id.clone(), error.code, error.message),
    }
}

fn storage_error_code(error: &StorageError) -> ErrorCode {
    if error.is_busy() {
        ErrorCode::Unavailable
    } else {
        ErrorCode::Internal
    }
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

impl PartialEq<&str> for CommandFailure {
    fn eq(&self, other: &&str) -> bool {
        self.message == *other
    }
}

impl PartialEq<String> for CommandFailure {
    fn eq(&self, other: &String) -> bool {
        &self.message == other
    }
}

fn run_command(store: &mut Store, name: &str, args: &Value) -> Result<Value, CommandFailure> {
    match name {
        "execution.create" => {
            let agent_id: Option<AgentId> = parse_optional_typed(args, "agent_id")?;
            let goal_id: Option<GoalId> = parse_optional_typed(args, "goal_id")?;
            if let Some(goal_id) = &goal_id {
                if store
                    .get_goal(goal_id)
                    .map_err(CommandFailure::storage)?
                    .is_none()
                {
                    return Err(format!("goal not found: {goal_id}").into());
                }
            }

            let exec = Execution {
                id: ExecutionId::new(),
                agent_id,
                goal_id,
                reason: parse_optional_string(args, "reason")?,
                command: parse_optional_string(args, "command")?,
                exit_code: parse_optional_i32(args, "exit_code")?,
                duration_ms: parse_optional_i64(args, "duration_ms")?,
                started_at: Timestamp::now(),
            };

            let mut edges = Vec::new();
            if let Some(agent_id) = &exec.agent_id {
                edges.push(ProvenanceEdge {
                    from: ObjectRef::from(&exec.id),
                    relation: ProvenanceRelation::CreatedBy,
                    to: ObjectRef::from(agent_id),
                    created_at: exec.started_at,
                    note: None,
                });
            }
            if let Some(goal_id) = &exec.goal_id {
                edges.push(ProvenanceEdge {
                    from: ObjectRef::from(&exec.id),
                    relation: ProvenanceRelation::CausedBy,
                    to: ObjectRef::from(goal_id),
                    created_at: exec.started_at,
                    note: None,
                });
            }

            CoreCommandRepository::create_execution(
                store,
                CreateExecution {
                    execution: &exec,
                    provenance: &edges,
                },
            )
            .map_err(CommandFailure::storage)?;
            tracing::info!(
                execution_id = %exec.id,
                agent_id = ?exec.agent_id,
                goal_id = ?exec.goal_id,
                provenance_edges = edges.len(),
                "execution recorded"
            );
            Ok(serde_json::json!({ "id": exec.id }))
        }
        "execution.show" => {
            let raw = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or("missing `id`")?;
            let exec_id = ExecutionId::from_str(raw).map_err(err_str)?;
            let record =
                CoreQueryRepository::execution(store, &exec_id).map_err(CommandFailure::storage)?;
            let data = match record {
                Some(record) => serde_json::to_value(&record).map_err(err_str)?,
                None => serde_json::json!(null),
            };
            Ok(serde_json::json!({ "execution": data }))
        }
        "execution.list" => {
            let records =
                CoreQueryRepository::executions(store).map_err(CommandFailure::storage)?;
            Ok(serde_json::json!({ "executions": records, "count": records.len() }))
        }
        other => Err(format!("agenticd: unknown command `{other}`").into()),
    }
}

fn parse_optional_typed<T>(args: &Value, key: &str) -> Result<Option<T>, String>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }

    let raw = value
        .as_str()
        .ok_or_else(|| format!("arg `{key}` must be a string or null"))?;
    raw.parse::<T>().map(Some).map_err(err_str)
}

fn parse_optional_string(args: &Value, key: &str) -> Result<Option<String>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }

    value
        .as_str()
        .map(|raw| Some(raw.to_owned()))
        .ok_or_else(|| format!("arg `{key}` must be a string or null"))
}

fn parse_optional_i64(args: &Value, key: &str) -> Result<Option<i64>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }

    value
        .as_i64()
        .map(Some)
        .ok_or_else(|| format!("arg `{key}` must be an integer or null"))
}

fn parse_optional_i32(args: &Value, key: &str) -> Result<Option<i32>, String> {
    let Some(raw) = parse_optional_i64(args, key)? else {
        return Ok(None);
    };
    i32::try_from(raw)
        .map(Some)
        .map_err(|_| format!("{key} out of range for i32: {raw}"))
}

fn err_str(error: impl std::fmt::Display) -> String {
    error.to_string()
}

pub(crate) async fn self_test() -> anyhow::Result<()> {
    let dir = tempfile::tempdir().context("creating temp dir")?;
    let db = dir.path().join("agenticd-self-test.db");
    let mut store = Store::open(&db).context("opening self-test db")?;
    let applied = store.migrate()?;
    let execution = Execution {
        id: ExecutionId::new(),
        agent_id: None,
        goal_id: None,
        reason: Some("self-test".to_owned()),
        command: Some("cargo test".to_owned()),
        exit_code: Some(0),
        duration_ms: Some(1),
        started_at: Timestamp::now(),
    };
    store.insert_execution(&execution)?;
    let loaded = store
        .get_execution(&execution.id)?
        .expect("execution persisted");
    assert_eq!(loaded.exit_code, Some(0));
    println!("agenticd self-test ok: migrations={}", applied.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowerly_core::types::{Goal, ProvenanceRelation};

    fn migrated_store() -> Store {
        let mut store = Store::open_in_memory().expect("open store");
        store.migrate().expect("migrate store");
        store
    }

    #[test]
    fn typed_execution_query_returns_created_record_and_edges() {
        let mut store = migrated_store();
        let response = run_command(
            &mut store,
            "execution.create",
            &serde_json::json!({ "command": "true", "reason": "typed facade" }),
        )
        .expect("create");
        let id = ExecutionId::from_str(response["id"].as_str().expect("id")).expect("execution id");
        let record = CoreQueryRepository::execution(&store, &id)
            .expect("query")
            .expect("exists");
        assert_eq!(record.command.as_deref(), Some("true"));
        let shown = run_command(
            &mut store,
            "execution.show",
            &serde_json::json!({ "id": id }),
        )
        .expect("show");
        assert_eq!(shown["execution"]["id"], response["id"]);
    }

    #[test]
    fn malformed_execution_references_are_rejected() {
        let mut store = migrated_store();

        let error = run_command(
            &mut store,
            "execution.create",
            &serde_json::json!({ "command": "true", "goal_id": "not-a-goal" }),
        )
        .expect_err("invalid goal id must fail");
        assert!(error.contains("goal"), "unexpected error: {error}");

        let error = run_command(
            &mut store,
            "execution.create",
            &serde_json::json!({ "command": "true", "agent_id": "not-an-agent" }),
        )
        .expect_err("invalid agent id must fail");
        assert!(error.contains("agent"), "unexpected error: {error}");

        assert!(store.list_executions().expect("list executions").is_empty());
    }

    #[test]
    fn execution_requires_existing_goal() {
        let mut store = migrated_store();
        let missing = GoalId::new();

        let error = run_command(
            &mut store,
            "execution.create",
            &serde_json::json!({ "command": "true", "goal_id": missing }),
        )
        .expect_err("missing goal must fail");

        assert_eq!(error, format!("goal not found: {missing}"));
        assert!(store.list_executions().expect("list executions").is_empty());
    }

    #[test]
    fn execution_integer_inputs_do_not_silently_coerce() {
        let mut store = migrated_store();

        let error = run_command(
            &mut store,
            "execution.create",
            &serde_json::json!({ "exit_code": i64::from(i32::MAX) + 1 }),
        )
        .expect_err("overflowing exit code must fail");
        assert!(error.contains("out of range"), "unexpected error: {error}");

        let error = run_command(
            &mut store,
            "execution.create",
            &serde_json::json!({ "duration_ms": "fast" }),
        )
        .expect_err("non-integer duration must fail");
        assert_eq!(error, "arg `duration_ms` must be an integer or null");

        assert!(store.list_executions().expect("list executions").is_empty());
    }

    #[test]
    fn execution_and_goal_provenance_commit_together() {
        let mut store = migrated_store();
        let goal = Goal::new("Run the test", "test");
        store.insert_goal(&goal).expect("insert goal");

        let response = run_command(
            &mut store,
            "execution.create",
            &serde_json::json!({
                "command": "cargo test",
                "goal_id": goal.id,
                "exit_code": 0,
                "duration_ms": 42
            }),
        )
        .expect("create execution");

        let id: ExecutionId = response["id"]
            .as_str()
            .expect("execution id")
            .parse()
            .expect("typed execution id");
        let execution = store
            .get_execution(&id)
            .expect("execution query")
            .expect("execution exists");
        assert_eq!(execution.goal_id.as_ref(), Some(&goal.id));

        let edges = store
            .provenance_from(&ObjectRef::from(&id))
            .expect("execution provenance");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relation, ProvenanceRelation::CausedBy);
        assert_eq!(edges[0].to, ObjectRef::from(&goal.id));
    }
}
