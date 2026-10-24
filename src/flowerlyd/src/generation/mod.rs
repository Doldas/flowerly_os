use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::{StorageError, Store};
use flowerly_core::types::{
    ExperimentId, Generation, GenerationId, GenerationStatus, GoalId, ObjectRef, ProvenanceEdge,
    ProvenanceRelation, Timestamp,
};
use flowerly_core::{coded_error_envelope, error_envelope, ok_envelope};
use serde::Deserialize;
use serde_json::Value;

pub(crate) fn handle_command(store: &Arc<Mutex<Store>>, request: &Envelope) -> Envelope {
    let Payload::Command { name, args } = &request.payload else {
        return error_envelope(request.id.clone(), "unexpected request type");
    };
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_envelope(request.id.clone(), "storage lock poisoned"),
    };
    if let Err((code, message)) = validate_generation_command(&store, name, args) {
        return coded_error_envelope(request.id.clone(), code, message);
    }
    match run_command(&mut store, name, args) {
        Ok(data) => ok_envelope(request.id.clone(), data),
        Err(error) => coded_error_envelope(request.id.clone(), error.code, error.message),
    }
}

/// Typed IPC-v1 argument validation. Unknown fields are tolerated to preserve
/// compatibility with older/newer clients; malformed known fields fail closed.
#[derive(Deserialize)]
struct CreateGenerationArgs {
    #[serde(default)]
    parent: Option<GenerationId>,
    #[serde(default)]
    reason_goal: Option<GoalId>,
    #[serde(default)]
    experiment: Option<ExperimentId>,
    #[serde(default)]
    source_commit: Option<String>,
    #[serde(default)]
    system_manifest: Option<String>,
}

#[derive(Deserialize)]
struct GenerationIdArgs {
    id: GenerationId,
}

#[derive(Deserialize)]
struct GenerationListArgs {}

fn typed_generation_args<T: serde::de::DeserializeOwned>(
    args: &Value,
) -> Result<T, (ErrorCode, String)> {
    serde_json::from_value(args.clone())
        .map_err(|error| (ErrorCode::InvalidArgument, format!("invalid generation arguments: {error}")))
}

/// Validate client-facing generation errors while the store lock is held.
/// SQLite errors remain internal instead of being inferred from error text.
fn validate_generation_command(
    store: &Store,
    name: &str,
    args: &Value,
) -> Result<(), (ErrorCode, String)> {
    match name {
        "generation.create" => {
            let typed: CreateGenerationArgs = typed_generation_args(args)?;
            let parent = typed.parent;
            let reason_goal = typed.reason_goal;
            let _ = (&typed.experiment, &typed.source_commit, &typed.system_manifest);
            if let Some(parent) = &parent {
                if store
                    .get_generation(parent)
                    .map_err(|e| (storage_error_code(&e), e.to_string()))?
                    .is_none()
                {
                    return Err((
                        ErrorCode::NotFound,
                        format!("generation not found: {parent}"),
                    ));
                }
            }
            if let Some(goal_id) = &reason_goal {
                if store
                    .get_goal(goal_id)
                    .map_err(|e| (storage_error_code(&e), e.to_string()))?
                    .is_none()
                {
                    return Err((ErrorCode::NotFound, format!("goal not found: {goal_id}")));
                }
            }
        }
        "generation.list" => {
            let _: GenerationListArgs = typed_generation_args(args)?;
        }
        "generation.inspect" | "generation.activate" => {
            let typed: GenerationIdArgs = typed_generation_args(args)?;
            let id = typed.id;
            if store
                .get_generation(&id)
                .map_err(|e| (storage_error_code(&e), e.to_string()))?
                .is_none()
            {
                return Err((ErrorCode::NotFound, format!("generation not found: {id}")));
            }
        }
        other => {
            return Err((
                ErrorCode::InvalidArgument,
                format!("generationd: unknown command `{other}`"),
            ));
        }
    }
    Ok(())
}

fn storage_error_code(error: &StorageError) -> ErrorCode {
    match error {
        StorageError::NotFound(_) => ErrorCode::NotFound,
        StorageError::InvalidOperation(_) | StorageError::ForeignKeyViolation(_) => {
            ErrorCode::InvalidArgument
        }
        StorageError::OperationConflict(_) | StorageError::UniqueViolation(_) => {
            ErrorCode::Conflict
        }
        _ if error.is_busy() => ErrorCode::Unavailable,
        _ if error.is_unique_violation() => ErrorCode::Conflict,
        _ if error.is_foreign_key_violation() => ErrorCode::InvalidArgument,
        _ => ErrorCode::Internal,
    }
}

#[derive(Debug)]
struct CommandFailure {
    code: ErrorCode,
    message: String,
}

impl CommandFailure {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::InvalidArgument,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::NotFound,
            message: message.into(),
        }
    }

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

fn run_command(store: &mut Store, name: &str, args: &Value) -> Result<Value, CommandFailure> {
    match name {
        "generation.create" => {
            let parent: Option<GenerationId> = parse_optional_typed(args, "parent")?;
            if let Some(parent) = &parent {
                if store
                    .get_generation(parent)
                    .map_err(CommandFailure::storage)?
                    .is_none()
                {
                    return Err(CommandFailure::not_found(format!(
                        "generation not found: {parent}"
                    )));
                }
            }

            let reason_goal: Option<GoalId> = parse_optional_typed(args, "reason_goal")?;
            if let Some(goal_id) = &reason_goal {
                if store
                    .get_goal(goal_id)
                    .map_err(CommandFailure::storage)?
                    .is_none()
                {
                    return Err(CommandFailure::not_found(format!(
                        "goal not found: {goal_id}"
                    )));
                }
            }

            let experiment: Option<ExperimentId> = parse_optional_typed(args, "experiment")?;
            let generation = Generation {
                id: GenerationId::new(),
                parent,
                source_commit: parse_optional_string(args, "source_commit")?,
                system_manifest: parse_optional_string(args, "system_manifest")?,
                reason_goal,
                experiment,
                status: GenerationStatus::Candidate,
                created_at: Timestamp::now(),
            };

            let mut edges = Vec::new();
            if let Some(parent) = &generation.parent {
                edges.push(ProvenanceEdge {
                    from: ObjectRef::from(&generation.id),
                    relation: ProvenanceRelation::CausedBy,
                    to: ObjectRef::from(parent),
                    created_at: generation.created_at,
                    note: Some("parent generation".to_owned()),
                });
            }
            if let Some(goal_id) = &generation.reason_goal {
                edges.push(ProvenanceEdge {
                    from: ObjectRef::from(&generation.id),
                    relation: ProvenanceRelation::CausedBy,
                    to: ObjectRef::from(goal_id),
                    created_at: generation.created_at,
                    note: Some("reason goal".to_owned()),
                });
            }
            if let Some(experiment) = &generation.experiment {
                edges.push(ProvenanceEdge {
                    from: ObjectRef::from(&generation.id),
                    relation: ProvenanceRelation::TestedBy,
                    to: ObjectRef::from(experiment),
                    created_at: generation.created_at,
                    note: None,
                });
            }

            store
                .insert_generation_with_provenance(&generation, &edges)
                .map_err(CommandFailure::storage)?;
            tracing::info!(
                generation_id = %generation.id,
                provenance_edges = edges.len(),
                "generation created"
            );
            Ok(serde_json::json!({
                "id": generation.id,
                "status": generation.status,
                "parent": generation.parent,
                "source_commit": generation.source_commit,
                "reason_goal": generation.reason_goal,
                "experiment": generation.experiment,
            }))
        }
        "generation.list" => {
            let items: Vec<Value> = store
                .list_generations()
                .map_err(CommandFailure::storage)?
                .iter()
                .map(|g| {
                    serde_json::json!({
                        "id": g.id,
                        "status": g.status,
                        "parent": g.parent,
                        "source_commit": g.source_commit,
                    })
                })
                .collect();
            Ok(serde_json::json!({ "generations": items, "count": items.len() }))
        }
        "generation.inspect" => {
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| CommandFailure::invalid("missing string arg `id`"))?;
            let generation = store
                .get_generation(
                    &GenerationId::from_str(id)
                        .map_err(|e| CommandFailure::invalid(e.to_string()))?,
                )
                .map_err(CommandFailure::storage)?
                .ok_or_else(|| CommandFailure::not_found(format!("generation not found: {id}")))?;
            Ok(serde_json::to_value(&generation).map_err(err_str)?)
        }
        "generation.activate" => {
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| CommandFailure::invalid("missing string arg `id`"))?;
            let generation_id = GenerationId::from_str(id)
                .map_err(|error| CommandFailure::invalid(error.to_string()))?;
            store
                .activate_generation(&generation_id)
                .map_err(CommandFailure::storage)?;
            Ok(serde_json::json!({ "id": generation_id, "status": GenerationStatus::Stable }))
        }
        other => Err(CommandFailure::invalid(format!(
            "generationd: unknown command `{other}`"
        ))),
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

fn err_str(error: impl std::fmt::Display) -> String {
    error.to_string()
}

pub(crate) async fn self_test() -> anyhow::Result<()> {
    let dir = tempfile::tempdir().context("creating temp dir")?;
    let db = dir.path().join("generationd-self-test.db");
    let mut store = Store::open(&db).context("opening self-test db")?;
    let applied = store.migrate()?;
    let first = Generation {
        id: GenerationId::new(),
        parent: None,
        source_commit: Some("abc123".to_owned()),
        system_manifest: Some("sha256:deadbeef".to_owned()),
        reason_goal: None,
        experiment: None,
        status: GenerationStatus::Candidate,
        created_at: Timestamp::now(),
    };
    store.insert_generation(&first)?;
    let second = Generation {
        id: GenerationId::new(),
        parent: Some(first.id.clone()),
        source_commit: Some("def456".to_owned()),
        system_manifest: None,
        reason_goal: None,
        experiment: None,
        status: GenerationStatus::Candidate,
        created_at: Timestamp::now(),
    };
    store.insert_generation(&second)?;
    store.set_generation_status(&first.id, GenerationStatus::Stable)?;
    store.set_generation_status(&first.id, GenerationStatus::Superseded)?;
    assert_eq!(store.list_generations()?.len(), 2);
    println!("generationd self-test ok: migrations={}", applied.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowerly_core::types::{Goal, ProvenanceRelation};

    #[test]
    fn activation_malformed_id_is_invalid_argument_not_internal() {
        let mut store = migrated_store();
        let error = run_command(
            &mut store,
            "generation.activate",
            &serde_json::json!({ "id": "malformed-generation-id" }),
        )
        .expect_err("malformed activation ID must be rejected");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(store
            .list_generations()
            .expect("list generations")
            .is_empty());
    }

    #[test]
    fn generation_storage_errors_preserve_extended_sqlite_codes() {
        let constraint = |code| {
            StorageError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(code),
                None,
            ))
        };
        assert_eq!(
            storage_error_code(&constraint(rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE)),
            ErrorCode::Conflict
        );
        assert_eq!(
            storage_error_code(&constraint(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY)),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            storage_error_code(&constraint(rusqlite::ffi::SQLITE_CONSTRAINT_CHECK)),
            ErrorCode::Internal
        );
        assert_eq!(
            storage_error_code(&constraint(rusqlite::ffi::SQLITE_CORRUPT)),
            ErrorCode::Internal
        );
        assert_eq!(
            storage_error_code(&constraint(rusqlite::ffi::SQLITE_BUSY)),
            ErrorCode::Unavailable
        );
    }

    fn migrated_store() -> Store {
        let mut store = Store::open_in_memory().expect("open store");
        store.migrate().expect("migrate store");
        store
    }

    fn candidate(parent: Option<GenerationId>) -> Generation {
        Generation {
            id: GenerationId::new(),
            parent,
            source_commit: Some("base".to_owned()),
            system_manifest: None,
            reason_goal: None,
            experiment: None,
            status: GenerationStatus::Candidate,
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn malformed_generation_references_are_rejected() {
        let mut store = migrated_store();

        let error = run_command(
            &mut store,
            "generation.create",
            &serde_json::json!({ "parent": "not-a-generation" }),
        )
        .expect_err("invalid parent must fail");
        assert!(error.contains("generation"), "unexpected error: {error}");

        let error = run_command(
            &mut store,
            "generation.create",
            &serde_json::json!({ "experiment": "not-an-experiment" }),
        )
        .expect_err("invalid experiment must fail");
        assert!(error.contains("experiment"), "unexpected error: {error}");

        assert!(store
            .list_generations()
            .expect("list generations")
            .is_empty());
    }

    #[test]
    fn generation_requires_existing_parent_and_reason_goal() {
        let mut store = migrated_store();
        let missing_parent = GenerationId::new();

        let error = run_command(
            &mut store,
            "generation.create",
            &serde_json::json!({ "parent": missing_parent }),
        )
        .expect_err("missing parent must fail");
        assert_eq!(error, format!("generation not found: {missing_parent}"));

        let missing_goal = GoalId::new();
        let error = run_command(
            &mut store,
            "generation.create",
            &serde_json::json!({ "reason_goal": missing_goal }),
        )
        .expect_err("missing reason goal must fail");
        assert_eq!(error, format!("goal not found: {missing_goal}"));

        assert!(store
            .list_generations()
            .expect("list generations")
            .is_empty());
    }

    #[test]
    fn generation_creation_records_causal_provenance() {
        let mut store = migrated_store();
        let parent = candidate(None);
        store.insert_generation(&parent).expect("insert parent");
        let goal = Goal::new("Improve the system", "test");
        store.insert_goal(&goal).expect("insert goal");
        let experiment = ExperimentId::new();

        let response = run_command(
            &mut store,
            "generation.create",
            &serde_json::json!({
                "parent": parent.id,
                "reason_goal": goal.id,
                "experiment": experiment,
                "source_commit": "abc123",
                "system_manifest": "sha256:manifest"
            }),
        )
        .expect("create generation");

        let id: GenerationId = response["id"]
            .as_str()
            .expect("generation id")
            .parse()
            .expect("typed generation id");
        let generation = store
            .get_generation(&id)
            .expect("generation query")
            .expect("generation exists");
        assert_eq!(generation.parent.as_ref(), Some(&parent.id));
        assert_eq!(generation.reason_goal.as_ref(), Some(&goal.id));
        assert_eq!(generation.experiment.as_ref(), Some(&experiment));

        let edges = store
            .provenance_from(&ObjectRef::from(&id))
            .expect("generation provenance");
        assert_eq!(edges.len(), 3);
        assert!(edges.iter().any(|edge| {
            edge.relation == ProvenanceRelation::CausedBy && edge.to == ObjectRef::from(&parent.id)
        }));
        assert!(edges.iter().any(|edge| {
            edge.relation == ProvenanceRelation::CausedBy && edge.to == ObjectRef::from(&goal.id)
        }));
        assert!(edges.iter().any(|edge| {
            edge.relation == ProvenanceRelation::TestedBy && edge.to == ObjectRef::from(&experiment)
        }));
    }

    #[test]
    fn generation_errors_have_explicit_codes() {
        let store = migrated_store();
        let cases = [
            (
                "generation.create",
                serde_json::json!({"parent": "not-a-generation"}),
                ErrorCode::InvalidArgument,
            ),
            (
                "generation.create",
                serde_json::json!({"parent": GenerationId::new().to_string()}),
                ErrorCode::NotFound,
            ),
            (
                "generation.create",
                serde_json::json!({"reason_goal": GoalId::new().to_string()}),
                ErrorCode::NotFound,
            ),
            (
                "generation.inspect",
                serde_json::json!({}),
                ErrorCode::InvalidArgument,
            ),
            (
                "generation.inspect",
                serde_json::json!({"id": "gen-missing"}),
                ErrorCode::NotFound,
            ),
            (
                "generation.activate",
                serde_json::json!({"id": 42}),
                ErrorCode::InvalidArgument,
            ),
            (
                "generation.activate",
                serde_json::json!({"id": "gen-missing"}),
                ErrorCode::NotFound,
            ),
            (
                "generation.delete",
                serde_json::json!({}),
                ErrorCode::InvalidArgument,
            ),
        ];
        for (command, args, expected) in cases {
            let error = validate_generation_command(&store, command, &args)
                .unwrap_err()
                .0;
            assert_eq!(error, expected, "{command} {args}");
        }
        store
            .insert_generation(&candidate(None))
            .expect("insert generation");
        assert!(
            validate_generation_command(&store, "generation.create", &serde_json::json!({}),)
                .is_ok()
        );
    }

    #[test]
    fn direct_generation_runner_preserves_typed_errors() {
        let mut store = migrated_store();
        let missing = GenerationId::new();
        for (name, args, expected) in [
            (
                "generation.create",
                serde_json::json!({"parent": missing}),
                ErrorCode::NotFound,
            ),
            (
                "generation.inspect",
                serde_json::json!({"id": missing}),
                ErrorCode::NotFound,
            ),
            (
                "generation.activate",
                serde_json::json!({"id": missing}),
                ErrorCode::NotFound,
            ),
            (
                "generation.inspect",
                serde_json::json!({}),
                ErrorCode::InvalidArgument,
            ),
            (
                "generation.activate",
                serde_json::json!({"id": 123}),
                ErrorCode::InvalidArgument,
            ),
            (
                "generation.invalid",
                serde_json::json!({}),
                ErrorCode::InvalidArgument,
            ),
        ] {
            let error = run_command(&mut store, name, &args).expect_err(name);
            assert_eq!(error.code, expected, "{name} {args}");
        }
    }

    #[test]
    fn activation_is_transactional_at_service_boundary() {
        let mut store = migrated_store();
        let mut first = candidate(None);
        first.status = GenerationStatus::Stable;
        let second = candidate(Some(first.id.clone()));
        store.insert_generation(&first).expect("insert first");
        store.insert_generation(&second).expect("insert second");

        run_command(
            &mut store,
            "generation.activate",
            &serde_json::json!({ "id": second.id }),
        )
        .expect("activate second");

        assert_eq!(
            store
                .get_generation(&first.id)
                .expect("first query")
                .expect("first exists")
                .status,
            GenerationStatus::Superseded
        );
        assert_eq!(
            store
                .get_generation(&second.id)
                .expect("second query")
                .expect("second exists")
                .status,
            GenerationStatus::Stable
        );
    }
}
