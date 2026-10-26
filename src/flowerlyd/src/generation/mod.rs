use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::{StorageError, Store, MAX_AUDIT_REASON};
use flowerly_core::types::{Caller, Owner};
use flowerly_core::types::{
    ExperimentId, Generation, GenerationId, GenerationStatus, GoalId, ObjectRef, ProvenanceEdge,
    ProvenanceRelation, Timestamp,
};
use flowerly_core::{coded_error_envelope, error_envelope, ok_envelope};
use serde::Deserialize;
use serde_json::Value;

pub(crate) fn handle_command(
    store: &Arc<Mutex<Store>>,
    request: &Envelope,
    caller: Caller,
) -> Envelope {
    let Payload::Command { name, args } = &request.payload else {
        return error_envelope(request.id.clone(), "unexpected request type");
    };
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_envelope(request.id.clone(), "storage lock poisoned"),
    };
    if let Err((code, message)) = validate_generation_command(&store, name, args, caller) {
        return coded_error_envelope(request.id.clone(), code, message);
    }
    match run_command(&mut store, name, args, caller) {
        Ok(data) => ok_envelope(request.id.clone(), data),
        Err(error) => coded_error_envelope(request.id.clone(), error.code, error.message),
    }
}

/// Typed IPC-v1 argument validation. Unknown fields are tolerated to preserve
/// compatibility with older/newer clients; malformed known fields fail closed.
#[derive(Debug, Deserialize)]
struct CreateGenerationArgs {
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    reason_goal: Option<String>,
    #[serde(default)]
    experiment: Option<String>,
    #[serde(default)]
    source_commit: Option<String>,
    #[serde(default)]
    system_manifest: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GenerationIdArgs {
    id: String,
}

#[derive(Debug, Deserialize)]
struct GenerationListArgs {}

#[derive(Debug, Deserialize)]
struct GenerationPromoteArgs {
    id: String,
    reason: String,
}

fn typed_generation_args<T: serde::de::DeserializeOwned>(
    args: &Value,
) -> Result<T, (ErrorCode, String)> {
    crate::args::typed_args("generation", args)
}

/// Validate client-facing generation errors while the store lock is held.
/// SQLite errors remain internal instead of being inferred from error text.
fn validate_generation_command(
    store: &Store,
    name: &str,
    args: &Value,
    caller: Caller,
) -> Result<(), (ErrorCode, String)> {
    match name {
        "generation.create" => {
            let typed: CreateGenerationArgs = typed_generation_args(args)?;
            let invalid = |error: String| (ErrorCode::InvalidArgument, error);
            let parent: Option<GenerationId> = typed
                .parent
                .map(|raw| GenerationId::from_str(&raw).map_err(|error| invalid(error.to_string())))
                .transpose()?;
            let reason_goal: Option<GoalId> = typed
                .reason_goal
                .map(|raw| GoalId::from_str(&raw).map_err(|error| invalid(error.to_string())))
                .transpose()?;
            let _: Option<ExperimentId> = typed
                .experiment
                .map(|raw| ExperimentId::from_str(&raw).map_err(|error| invalid(error.to_string())))
                .transpose()?;
            let _ = (&typed.source_commit, &typed.system_manifest);
            // Only a parent the caller can see (own or system) (#225 PR 6).
            if let Some(parent) = &parent {
                if store
                    .generation_for(caller, parent)
                    .map_err(|e| (storage_error_code(&e), e.to_string()))?
                    .is_none()
                {
                    return Err((
                        ErrorCode::NotFound,
                        format!("generation not found: {parent}"),
                    ));
                }
            }
            // Only the caller's own Goal; another uid's is `not_found` (#225).
            if let Some(goal_id) = &reason_goal {
                if store
                    .goal_for(caller, goal_id)
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
        "generation.inspect" => {
            let typed: GenerationIdArgs = typed_generation_args(args)?;
            let id = GenerationId::from_str(&typed.id)
                .map_err(|error| (ErrorCode::InvalidArgument, error.to_string()))?;
            if store
                .generation_for(caller, &id)
                .map_err(|e| (storage_error_code(&e), e.to_string()))?
                .is_none()
            {
                return Err((ErrorCode::NotFound, format!("generation not found: {id}")));
            }
        }
        // Admin: existence and ownership are checked in the transaction.
        "generation.activate" => {
            let typed: GenerationIdArgs = typed_generation_args(args)?;
            GenerationId::from_str(&typed.id)
                .map_err(|error| (ErrorCode::InvalidArgument, error.to_string()))?;
        }
        "generation.promote" => {
            let typed: GenerationPromoteArgs = typed_generation_args(args)?;
            GenerationId::from_str(&typed.id)
                .map_err(|error| (ErrorCode::InvalidArgument, error.to_string()))?;
            if typed.reason.trim().is_empty() || typed.reason.len() > MAX_AUDIT_REASON {
                return Err((
                    ErrorCode::InvalidArgument,
                    format!("`reason` must be 1..{MAX_AUDIT_REASON} bytes and not blank"),
                ));
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
    error.ipc_error_code()
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

fn run_command(
    store: &mut Store,
    name: &str,
    args: &Value,
    caller: Caller,
) -> Result<Value, CommandFailure> {
    match name {
        "generation.create" => {
            let parent: Option<GenerationId> = parse_optional_typed(args, "parent")?;
            // Rechecked in the inserting transaction and by a 0013 trigger.
            if let Some(parent) = &parent {
                if store
                    .generation_for(caller, parent)
                    .map_err(CommandFailure::storage)?
                    .is_none()
                {
                    return Err(CommandFailure::not_found(format!(
                        "generation not found: {parent}"
                    )));
                }
            }

            let reason_goal: Option<GoalId> = parse_optional_typed(args, "reason_goal")?;
            // Rechecked in the inserting transaction and by a 0011 trigger.
            if let Some(goal_id) = &reason_goal {
                if store
                    .goal_for(caller, goal_id)
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
                // Candidates are private to their creator (#225); only an
                // audited Admin promotion makes a shared system record.
                owner: caller.owner(),
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
                .generations_for(caller)
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
            let mut generation = store
                .generation_for(
                    caller,
                    &GenerationId::from_str(id)
                        .map_err(|e| CommandFailure::invalid(e.to_string()))?,
                )
                .map_err(CommandFailure::storage)?
                .ok_or_else(|| CommandFailure::not_found(format!("generation not found: {id}")))?;
            // Visible: own and system Generations (#225 PR 6). A reason Goal
            // the caller cannot see is not revealed, also on a promoted
            // system Generation (redaction contract, PR 4).
            if let Some(goal) = &generation.reason_goal {
                if store
                    .goal_for(caller, goal)
                    .map_err(CommandFailure::storage)?
                    .is_none()
                {
                    generation.reason_goal = None;
                }
            }
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
        // Admin, audited: the caller is the real admin uid (#225 PR 6).
        "generation.promote" => {
            let typed: GenerationPromoteArgs =
                serde_json::from_value(args.clone()).map_err(err_str)?;
            let generation_id = GenerationId::from_str(&typed.id)
                .map_err(|error| CommandFailure::invalid(error.to_string()))?;
            let promoted = store
                .promote_generation(caller, &generation_id, &typed.reason)
                .map_err(CommandFailure::storage)?;
            tracing::info!(
                generation_id = %promoted.id,
                admin_uid = caller.uid(),
                "generation promoted to system"
            );
            Ok(serde_json::json!({
                "id": promoted.id,
                "status": promoted.status,
                "owner": promoted.owner.class(),
            }))
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
        owner: Owner::System,
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
        owner: Owner::System,
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

    const TEST_CALLER: Caller = Caller::new(1000);
    use flowerly_core::types::{Goal, ProvenanceRelation};

    #[test]
    fn activation_malformed_id_is_invalid_argument_not_internal() {
        let mut store = migrated_store();
        let error = run_command(
            &mut store,
            "generation.activate",
            &serde_json::json!({ "id": "malformed-generation-id" }),
            TEST_CALLER,
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

    #[test]
    fn typed_generation_v1_arguments_accept_legacy_unknown_fields() {
        let args = serde_json::json!({
            "parent": null,
            "reason_goal": null,
            "experiment": null,
            "source_commit": "abc",
            "system_manifest": null,
            "future_optional_field": true
        });
        let parsed: CreateGenerationArgs =
            typed_generation_args(&args).expect("legacy compatible fields");
        assert!(parsed.parent.is_none());
        assert!(parsed.reason_goal.is_none());
        assert_eq!(parsed.source_commit.as_deref(), Some("abc"));
        let _: GenerationListArgs = typed_generation_args(&serde_json::json!({"future": 1}))
            .expect("list ignores unknown fields");
    }

    #[test]
    fn typed_generation_v1_arguments_reject_malformed_fields() {
        for args in [
            serde_json::json!({"parent": 123}),
            serde_json::json!({"reason_goal": false}),
            serde_json::json!({"source_commit": []}),
            serde_json::json!([]),
        ] {
            let error = typed_generation_args::<CreateGenerationArgs>(&args)
                .expect_err("invalid arguments must fail");
            assert_eq!(error.0, ErrorCode::InvalidArgument);
        }
        for args in [
            serde_json::json!({}),
            serde_json::json!({"id": null}),
            serde_json::json!({"id": 123}),
        ] {
            let error = typed_generation_args::<GenerationIdArgs>(&args)
                .expect_err("invalid identity must fail");
            assert_eq!(error.0, ErrorCode::InvalidArgument);
        }
        let store = migrated_store();
        for (name, args) in [
            (
                "generation.create",
                serde_json::json!({"parent": "malformed-id"}),
            ),
            (
                "generation.create",
                serde_json::json!({"experiment": "invalid-id"}),
            ),
            (
                "generation.activate",
                serde_json::json!({"id": "invalid-id"}),
            ),
        ] {
            let error = validate_generation_command(&store, name, &args, TEST_CALLER)
                .expect_err("invalid typed identity must fail");
            assert_eq!(error.0, ErrorCode::InvalidArgument);
        }
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
            owner: Owner::User(1000),
        }
    }

    #[test]
    fn malformed_generation_references_are_rejected() {
        let mut store = migrated_store();

        let error = run_command(
            &mut store,
            "generation.create",
            &serde_json::json!({ "parent": "not-a-generation" }),
            TEST_CALLER,
        )
        .expect_err("invalid parent must fail");
        assert!(error.contains("generation"), "unexpected error: {error}");

        let error = run_command(
            &mut store,
            "generation.create",
            &serde_json::json!({ "experiment": "not-an-experiment" }),
            TEST_CALLER,
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
            TEST_CALLER,
        )
        .expect_err("missing parent must fail");
        assert_eq!(error, format!("generation not found: {missing_parent}"));

        let missing_goal = GoalId::new();
        let error = run_command(
            &mut store,
            "generation.create",
            &serde_json::json!({ "reason_goal": missing_goal }),
            TEST_CALLER,
        )
        .expect_err("missing reason goal must fail");
        assert_eq!(error, format!("goal not found: {missing_goal}"));

        assert!(store
            .list_generations()
            .expect("list generations")
            .is_empty());
    }

    #[test]
    fn reason_goal_must_be_the_callers_own_and_is_not_revealed_otherwise() {
        let mut store = migrated_store();
        let mut theirs = Goal::new("theirs", "uid:2000");
        theirs.owner = Caller::new(2000).owner();
        store.insert_goal(&theirs).expect("insert goal");
        let args = serde_json::json!({ "reason_goal": theirs.id });
        let (code, message) =
            validate_generation_command(&store, "generation.create", &args, TEST_CALLER)
                .unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
        assert_eq!(message, format!("goal not found: {}", theirs.id));
        let failure = run_command(&mut store, "generation.create", &args, TEST_CALLER).unwrap_err();
        assert_eq!(failure.code, ErrorCode::NotFound);
        assert!(store.list_generations().unwrap().is_empty());

        // Another uid's candidate is private (#225 PR 6); once an admin
        // promotes it, its reason Goal is still shown only to the Goal's owner.
        let mut legacy = candidate(None);
        legacy.reason_goal = Some(theirs.id.clone());
        legacy.owner = theirs.owner;
        store.insert_generation(&legacy).expect("insert generation");
        let hidden = run_command(
            &mut store,
            "generation.inspect",
            &serde_json::json!({ "id": legacy.id }),
            TEST_CALLER,
        )
        .unwrap_err();
        assert_eq!(hidden.code, ErrorCode::NotFound);
        run_command(
            &mut store,
            "generation.promote",
            &serde_json::json!({ "id": legacy.id, "reason": "share it" }),
            Caller::new(0),
        )
        .unwrap();
        let shown = run_command(
            &mut store,
            "generation.inspect",
            &serde_json::json!({ "id": legacy.id }),
            TEST_CALLER,
        )
        .unwrap();
        assert_eq!(shown["reason_goal"], Value::Null);
        let owner = Caller::new(2000);
        let shown = run_command(
            &mut store,
            "generation.inspect",
            &serde_json::json!({ "id": legacy.id }),
            owner,
        )
        .unwrap();
        assert_eq!(shown["reason_goal"], serde_json::json!(theirs.id));
    }

    #[test]
    fn generation_creation_records_causal_provenance() {
        let mut store = migrated_store();
        let parent = candidate(None);
        store.insert_generation(&parent).expect("insert parent");
        let mut goal = Goal::new("Improve the system", "test");
        goal.owner = TEST_CALLER.owner();
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
            TEST_CALLER,
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
                "generation.promote",
                serde_json::json!({"id": "gen-missing"}),
                ErrorCode::InvalidArgument,
            ),
            (
                "generation.promote",
                serde_json::json!({"id": "gen-missing", "reason": " "}),
                ErrorCode::InvalidArgument,
            ),
            (
                "generation.delete",
                serde_json::json!({}),
                ErrorCode::InvalidArgument,
            ),
        ];
        for (command, args, expected) in cases {
            let error = validate_generation_command(&store, command, &args, TEST_CALLER)
                .unwrap_err()
                .0;
            assert_eq!(error, expected, "{command} {args}");
        }
        store
            .insert_generation(&candidate(None))
            .expect("insert generation");
        assert!(validate_generation_command(
            &store,
            "generation.create",
            &serde_json::json!({}),
            TEST_CALLER
        )
        .is_ok());
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
            let error = run_command(&mut store, name, &args, TEST_CALLER).expect_err(name);
            assert_eq!(error.code, expected, "{name} {args}");
        }
    }

    #[test]
    fn activation_is_transactional_at_service_boundary() {
        let mut store = migrated_store();
        let mut first = candidate(None);
        first.status = GenerationStatus::Stable;
        first.owner = Owner::System;
        let second = candidate(Some(first.id.clone()));
        store.insert_generation(&first).expect("insert first");
        store.insert_generation(&second).expect("insert second");

        let missing = run_command(
            &mut store,
            "generation.activate",
            &serde_json::json!({ "id": "gen-missing" }),
            TEST_CALLER,
        )
        .unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);
        // A private candidate is promoted, with an audit row, before activation.
        let private = run_command(
            &mut store,
            "generation.activate",
            &serde_json::json!({ "id": second.id }),
            TEST_CALLER,
        )
        .unwrap_err();
        assert_eq!(private.code, ErrorCode::Conflict);
        let promoted = run_command(
            &mut store,
            "generation.promote",
            &serde_json::json!({ "id": second.id, "reason": "release" }),
            TEST_CALLER,
        )
        .expect("promote second");
        assert_eq!(promoted["owner"], "system");
        assert_eq!(
            store
                .admin_audit_for_object(&ObjectRef::from(&second.id))
                .unwrap()
                .len(),
            1
        );

        run_command(
            &mut store,
            "generation.activate",
            &serde_json::json!({ "id": second.id }),
            TEST_CALLER,
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
