use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use agentic_core::{error_envelope, init_tracing, ok_envelope, run_service, ServiceConfig};
use agentic_ipc::{Envelope, Payload};
use agentic_storage::Store;
use agentic_types::{
    ExperimentId, Generation, GenerationId, GenerationStatus, GoalId, ObjectRef, ProvenanceEdge,
    ProvenanceRelation, Timestamp,
};
use anyhow::Context;
use clap::Parser;
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(
    name = "generationd",
    version,
    about = "Flowerly OS generation service"
)]
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
        None => ServiceConfig::for_service("generationd"),
    };
    init_tracing(&config.name, config.log_format);

    if cli.self_test {
        return self_test().await;
    }

    let store = open_store(&config)?;
    let extra = serde_json::json!({ "generation_count": store.list_generations()?.len() });
    let store = Arc::new(Mutex::new(store));
    let handler = move |request: &Envelope| -> Envelope { handle_command(&store, request) };

    run_service(config, extra, Arc::new(handler))
        .await
        .context("generationd terminated abnormally")
}

fn open_store(config: &ServiceConfig) -> anyhow::Result<Store> {
    let db_path = config.database_path();
    let mut store = Store::open(&db_path).context("opening generationd database")?;
    store.migrate().context("running generationd migrations")?;
    store
        .health_check()
        .context("generationd database health check")?;
    Ok(store)
}

fn handle_command(store: &Arc<Mutex<Store>>, request: &Envelope) -> Envelope {
    let Payload::Command { name, args } = &request.payload else {
        return error_envelope(request.id.clone(), "unexpected request type");
    };
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_envelope(request.id.clone(), "storage lock poisoned"),
    };
    match run_command(&mut store, name, args) {
        Ok(data) => ok_envelope(request.id.clone(), data),
        Err(message) => error_envelope(request.id.clone(), message),
    }
}

fn run_command(store: &mut Store, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "generation.create" => {
            let parent: Option<GenerationId> = parse_optional_typed(args, "parent")?;
            if let Some(parent) = &parent {
                if store.get_generation(parent).map_err(err_str)?.is_none() {
                    return Err(format!("generation not found: {parent}"));
                }
            }

            let reason_goal: Option<GoalId> = parse_optional_typed(args, "reason_goal")?;
            if let Some(goal_id) = &reason_goal {
                if store.get_goal(goal_id).map_err(err_str)?.is_none() {
                    return Err(format!("goal not found: {goal_id}"));
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
                .map_err(err_str)?;
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
                .map_err(err_str)?
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
                .ok_or("missing string arg `id`")?;
            let generation = store
                .get_generation(&GenerationId::from_str(id).map_err(err_str)?)
                .map_err(err_str)?
                .ok_or_else(|| format!("generation not found: {id}"))?;
            Ok(serde_json::to_value(&generation).map_err(err_str)?)
        }
        "generation.activate" => {
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or("missing string arg `id`")?;
            let generation_id = GenerationId::from_str(id).map_err(err_str)?;
            store.activate_generation(&generation_id).map_err(err_str)?;
            Ok(serde_json::json!({ "id": generation_id, "status": GenerationStatus::Stable }))
        }
        other => Err(format!("generationd: unknown command `{other}`")),
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

async fn self_test() -> anyhow::Result<()> {
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
    use agentic_types::{Goal, ProvenanceRelation};

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
