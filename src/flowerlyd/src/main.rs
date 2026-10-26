//! Flowerly OS unified supervisor. Only the CLI-facing socket leaves this process.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use flowerly_core::ipc::{
    AccessPolicy, AuthenticatedHandler, Envelope, ErrorCode, Payload, PeerIdentity,
};
use flowerly_core::storage::Store;
use flowerly_core::types::Caller;
use flowerly_core::{coded_error_envelope, init_tracing, run_authorized_service, ServiceConfig};
use tokio::sync::mpsc;

#[allow(dead_code)]
mod agentic;

mod args;

mod authorization;
#[allow(dead_code)]
mod capability;
#[allow(dead_code)]
mod compute;
#[allow(dead_code)]
mod flow;
#[allow(dead_code)]
mod generation;
#[allow(dead_code)]
mod goal;
mod ownership;
#[allow(dead_code)]
mod provenance;
#[allow(dead_code)]
mod session;

type SharedStore = Arc<Mutex<Store>>;

#[derive(Debug, Parser)]
#[command(name = "flowerlyd", version, about = "Flowerly OS unified supervisor")]
struct Cli {
    #[arg(long, env = "AGENTIC_CONFIG")]
    config: Option<PathBuf>,
    #[arg(long)]
    state_dir: Option<PathBuf>,
    #[arg(long)]
    runtime_dir: Option<PathBuf>,
    #[arg(long)]
    self_test: bool,
}

struct Job {
    request: Envelope,
    /// Kernel-attested caller, carried with the request so handlers never
    /// re-derive identity from request JSON (#225).
    caller: Caller,
    reply: std::sync::mpsc::SyncSender<Envelope>,
}
type Queue = mpsc::Sender<Job>;

/// A domain handler: store, request and the kernel-attested caller.
type Handler = fn(&SharedStore, &Envelope, Caller) -> Envelope;

fn spawn_worker(store: &SharedStore, handler: Handler) -> Queue {
    let (tx, mut rx) = mpsc::channel::<Job>(64);
    let store = Arc::clone(store);
    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            let store = Arc::clone(&store);
            let Job {
                request,
                caller,
                reply,
            } = job;
            let result =
                tokio::task::spawn_blocking(move || handler(&store, &request, caller)).await;
            match result {
                Ok(response) => {
                    let _ = reply.send(response);
                }
                Err(error) => tracing::error!(%error, "module task panicked"),
            }
        }
    });
    tx
}

const MODULE_DEADLINE: Duration = Duration::from_secs(7);

fn via_queue(tx: &Queue, request: &Envelope, caller: Caller) -> Envelope {
    let (reply, received) = std::sync::mpsc::sync_channel(1);
    if tx
        .try_send(Job {
            request: request.clone(),
            caller,
            reply,
        })
        .is_err()
    {
        return coded_error_envelope(
            request.id.clone(),
            ErrorCode::Unavailable,
            "module queue unavailable or full; the request was not executed",
        );
    }
    match received.recv_timeout(MODULE_DEADLINE) {
        Ok(response) => response,
        // The job may already be running and can still commit.
        Err(_) => coded_error_envelope(
            request.id.clone(),
            ErrorCode::Unavailable,
            format!(
                "module handler deadline exceeded after {} ms; the outcome is unknown",
                MODULE_DEADLINE.as_millis()
            ),
        ),
    }
}

struct Router {
    policy: AccessPolicy,
    store: SharedStore,
    session: session::SessionRuntime,
    flow: Queue,
    generation: Queue,
    provenance: Queue,
    compute: Queue,
}

impl Router {
    fn handle(&self, request: &Envelope, peer: &PeerIdentity) -> Envelope {
        let Payload::Command { name, .. } = &request.payload else {
            return coded_error_envelope(
                request.id.clone(),
                ErrorCode::InvalidArgument,
                "command required",
            );
        };
        // Deny by default: only inventoried commands the kernel-attested
        // peer is authorized for reach a handler.
        let admin = authorization::classify(name) == Some(authorization::CommandClass::Admin);
        if let Err((code, message)) = authorization::authorize(name, peer, &self.policy) {
            if admin {
                audit_admin(request, name, peer, "denied", Some(code));
            }
            return coded_error_envelope(request.id.clone(), code, message);
        }
        let response = self.dispatch(name, request, peer.caller());
        if admin {
            let code = match &response.payload {
                Payload::Err { code, .. } => Some(code.unwrap_or(ErrorCode::Internal)),
                _ => None,
            };
            let outcome = if code.is_some() {
                "failed"
            } else {
                "succeeded"
            };
            audit_admin(request, name, peer, outcome, code);
        }
        response
    }

    fn dispatch(&self, name: &str, request: &Envelope, caller: Caller) -> Envelope {
        match name.split('.').next() {
            Some("flow") => via_queue(&self.flow, request, caller),
            Some("generation") => via_queue(&self.generation, request, caller),
            Some("provenance") => via_queue(&self.provenance, request, caller),
            Some("goal") => goal::handle_command(&self.store, request, caller),
            Some("ownership") => ownership::handle_command(&self.store, request, caller),
            Some("session") => self.session.handle(&self.store, request, caller),
            Some("execution") | Some("agent") => {
                agentic::handle_command(&self.store, request, caller)
            }
            Some("capability") => capability::handle_command(request),
            Some("model") | Some("device") | Some("profile") | Some("provider")
            | Some("workload") | Some("usage") | Some("inference") => {
                via_queue(&self.compute, request, caller)
            }
            _ => coded_error_envelope(
                request.id.clone(),
                ErrorCode::InvalidArgument,
                format!("unknown command: {name}"),
            ),
        }
    }
}

/// One journal record per Admin command decision (#225). The peer is the
/// kernel-attested client: the human administrator's own uid when they
/// connect through the opt-in users group. Arguments are never logged.
fn audit_admin(
    request: &Envelope,
    name: &str,
    peer: &PeerIdentity,
    outcome: &str,
    code: Option<ErrorCode>,
) {
    tracing::info!(
        target: "flowerly::admin_audit",
        request_id = %request.id,
        command = name,
        peer_uid = peer.uid,
        peer_gid = peer.gid,
        peer_pid = ?peer.pid,
        service_account = peer.service_account,
        outcome,
        code = ?code,
        "admin command"
    );
}

const MODULES: [&str; 8] = [
    "capability",
    "compute",
    "flow",
    "goal",
    "session",
    "generation",
    "provenance",
    "execution",
];

/// Status reports ready only when the database answers a consistent count
/// snapshot; a failed read is reported with its typed storage error code
/// instead of `ready: true` with missing counts. Flow, Goal, Session and
/// Execution counts and the foreground are those of the calling peer (#225).
fn status_details(db: &Store, policy: &AccessPolicy, caller: Caller) -> serde_json::Value {
    let snapshot = db
        .status_counts_for(caller)
        .and_then(|counts| Ok((counts, db.foreground_flow_for(caller)?)));
    match snapshot {
        Ok((counts, foreground)) => serde_json::json!({
            "ready": true,
            "modules": MODULES,
            "default_flow": "General",
            "foreground_flow": {"id": foreground.flow.id, "name": foreground.flow.name},
            "flow_count": counts.flows,
            "goal_count": counts.goals,
            "session_count": counts.sessions,
            "execution_count": counts.executions,
            "generation_count": counts.generations,
            "provenance_edge_count": counts.provenance_edges,
            "model_count": counts.models,
            "profile_count": counts.profiles,
            "active_workload_count": counts.active_workloads,
            "provider_count": counts.providers,
            "admin_authorization": authorization::admin_policy(policy),
            "client_access": authorization::client_policy(policy),
            "ownership_pending_commands": authorization::pending_scopes().count(),
            "database": "ready"
        }),
        Err(error) => serde_json::json!({
            "ready": false,
            "modules": MODULES,
            "database": "error",
            "error_code": error.ipc_error_code(),
            "reason": error.to_string(),
        }),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.self_test {
        agentic::self_test().await?;
        compute::self_test().await?;
        flow::self_test().await?;
        goal::self_test().await?;
        generation::self_test().await?;
        provenance::self_test().await?;
        session::self_test().await?;
        println!("flowerlyd self-test ok: unified service modules");
        return Ok(());
    }
    let mut config = match cli.config {
        Some(ref path) => ServiceConfig::load(path).context("loading supervisor config")?,
        None => ServiceConfig::for_service("flowerlyd"),
    };
    config.name = "flowerlyd".into();
    if let Some(state_dir) = cli.state_dir {
        config.state_dir = state_dir;
    }
    if let Some(runtime_dir) = cli.runtime_dir {
        config.runtime_dir = runtime_dir;
    }
    init_tracing(&config.name, config.log_format);
    let policy = config
        .access_policy()
        .context("resolving IPC access groups")?;
    authorization::refuse_unenforced_multi_user(&policy).map_err(anyhow::Error::msg)?;
    tracing::info!(
        admin_authorization = authorization::admin_policy(&policy),
        client_access = authorization::client_policy(&policy),
        users_gid = ?policy.users_gid,
        admin_gid = ?policy.admin_gid,
        "IPC access policy"
    );
    let mut db = Store::open(config.database_path()).context("opening unified state")?;
    db.migrate().context("migrating unified SQLite state")?;
    db.health_check().context("checking unified state")?;
    db.ensure_general_flow().context("ensuring General Flow")?;
    compute::init_runtime_dir(config.runtime_dir.clone());
    let store: SharedStore = Arc::new(Mutex::new(db));
    let session = session::SessionRuntime::start(&config, &store)?;
    let router = Arc::new(Router {
        policy,
        flow: spawn_worker(&store, flow::handle_command),
        generation: spawn_worker(&store, generation::handle_command),
        provenance: spawn_worker(&store, provenance::handle_command),
        compute: spawn_worker(&store, compute::handle_command),
        store: Arc::clone(&store),
        session,
    });
    let status_store = Arc::clone(&store);
    let status = Arc::new(move |peer: &PeerIdentity| match status_store.try_lock() {
        Ok(db) => status_details(&db, &policy, peer.caller()),
        Err(_) => serde_json::json!({"ready": false, "reason": "store busy"}),
    });
    let handler: Arc<AuthenticatedHandler> =
        Arc::new(move |request: &Envelope, peer: &PeerIdentity| router.handle(request, peer));
    run_authorized_service(config, policy, status, handler)
        .await
        .context("flowerlyd terminated")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_ready_with_live_counts() {
        let mut store = Store::open_in_memory().expect("open store");
        store.migrate().expect("migrate");
        store.ensure_general_flow().expect("general flow");
        let details = status_details(&store, &AccessPolicy::default(), Caller::new(1000));
        assert_eq!(details["ready"], true, "{details}");
        assert_eq!(details["admin_authorization"], "denied_unconfigured");
        assert_eq!(details["client_access"], "service_account_only");
        assert_eq!(details["database"], "ready");
        assert_eq!(details["flow_count"], 1);
        // Bootstrapping General records its created-by provenance edge.
        assert_eq!(details["provenance_edge_count"], 1);
    }

    #[test]
    fn status_is_not_ready_when_the_database_cannot_answer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.db");
        let mut store = Store::open(&path).expect("open store");
        store.migrate().expect("migrate");
        rusqlite::Connection::open(&path)
            .expect("open second connection")
            .execute_batch("DROP TABLE sessions;")
            .expect("damage schema");
        let details = status_details(&store, &AccessPolicy::default(), Caller::new(1000));
        assert_eq!(details["ready"], false, "{details}");
        assert_eq!(details["database"], "error");
        assert_eq!(details["error_code"], "internal");
        assert!(details.get("flow_count").is_none(), "{details}");
    }
}

#[cfg(test)]
mod malformed_arguments {
    use super::*;

    /// Gate B (#31) / #53: client-caused failures must carry a typed code.
    /// Every store-backed command is sent missing, wrongly typed, malformed
    /// and well-formed-but-absent arguments; none may answer `internal` or
    /// an untyped legacy error. PTY commands need a live session runtime
    /// and are covered by the PTY black-box suites.
    #[test]
    fn client_errors_are_never_internal_or_untyped() {
        let mut db = Store::open_in_memory().expect("open store");
        db.migrate().expect("migrate");
        db.ensure_general_flow().expect("general flow");
        let store: SharedStore = Arc::new(Mutex::new(db));
        let commands = [
            "flow.create",
            "flow.list",
            "flow.activate",
            "flow.archive",
            "goal.create",
            "goal.child",
            "goal.show",
            "goal.list",
            "goal.complete",
            "goal.abandon",
            "session.create",
            "session.list",
            "session.terminate",
            "execution.create",
            "execution.show",
            "execution.list",
            "generation.create",
            "generation.list",
            "generation.inspect",
            "generation.activate",
            "generation.promote",
            "ownership.inspect",
            "ownership.claim",
            "ownership.release",
            "provenance.add",
            "provenance.from",
            "provenance.to",
            "model.register",
            "model.list",
            "model.show",
            "model.retire",
            "model.compatibility",
            "device.budget",
            "profile.clone",
            "profile.edit",
            "profile.export",
            "profile.import",
            "profile.list",
            "profile.rename",
            "profile.resolve",
            "profile.show",
            "profile.version_history",
            "provider.list",
            "provider.register",
            "provider.show",
            "workload.submit",
            "workload.list",
            "workload.show",
            "workload.complete",
            "workload.usage_brief",
            "usage.record",
        ];
        let shapes = [
            serde_json::json!({}),
            serde_json::json!({
                "id": 42, "name": 42, "objective": 7, "priority": "x", "from": 1, "to": true,
                "ref": [], "flow_id": 3, "goal_id": 3, "agent_id": 3, "operation_id": 5,
                "parent": 9, "model_id": 1, "profile_id": 1, "profile": 1, "source": 1,
                "version": "x", "workload_id": 1
            }),
            serde_json::json!({
                "id": "bogus", "name": "bogus", "objective": "o", "from": "bogus", "to": "bogus",
                "ref": "bogus", "relation": "bogus", "flow_id": "bogus", "goal_id": "bogus",
                "agent_id": "bogus", "parent": "bogus", "model_id": "bogus", "profile": "bogus",
                "source": "bogus", "workload_id": "bogus"
            }),
            serde_json::json!({
                "id": "flow-00000000000000000000000000000000",
                "ref": "flow:flow-00000000000000000000000000000000",
                "goal_id": "goal-00000000000000000000000000000000",
                "model_id": "model-00000000000000000000000000000000",
                "workload_id": "wl-00000000000000000000000000000000"
            }),
        ];
        let mut answered = 0;
        let mut leaks = Vec::new();
        for command in commands {
            for shape in &shapes {
                let mut args = shape.clone();
                args["created_by"] = serde_json::json!("uid:1000");
                let request = Envelope::new(Payload::Command {
                    name: command.into(),
                    args: args.clone(),
                });
                let reply = match command.split('.').next() {
                    Some("flow") => flow::handle_command(&store, &request, Caller::new(1000)),
                    Some("goal") => goal::handle_command(&store, &request, Caller::new(1000)),
                    Some("ownership") => {
                        ownership::handle_command(&store, &request, Caller::new(1000))
                    }
                    Some("session") => session::handle_command(&store, &request, Caller::new(1000)),
                    Some("execution") => {
                        agentic::handle_command(&store, &request, Caller::new(1000))
                    }
                    Some("generation") => {
                        generation::handle_command(&store, &request, Caller::new(1000))
                    }
                    Some("provenance") => {
                        provenance::handle_command(&store, &request, Caller::new(1000))
                    }
                    _ => compute::handle_command(&store, &request, Caller::new(1000)),
                };
                answered += 1;
                if let Payload::Err { code, message } = &reply.payload {
                    if matches!(code, None | Some(ErrorCode::Internal)) {
                        leaks.push(format!("{command} {args} -> {code:?}: {message}"));
                    }
                }
            }
        }
        assert_eq!(answered, commands.len() * shapes.len());
        assert!(
            leaks.is_empty(),
            "untyped client errors:\n{}",
            leaks.join("\n")
        );
    }
}

#[cfg(test)]
mod ownership_recording {
    use super::*;

    const ALICE: u32 = 1234;

    fn ok(reply: Envelope, what: &str) -> serde_json::Value {
        match reply.payload {
            Payload::Ok { data } => data,
            other => panic!("{what}: {other:?}"),
        }
    }

    /// #225 PR 2 (recording only; reads are not yet filtered): every command
    /// that persists a user object records the kernel-attested caller as its
    /// owner, never the client-supplied `created_by`, and no table ends with
    /// an unresolved row.
    #[test]
    fn every_create_path_records_an_explicit_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut db = Store::open(&path).unwrap();
        db.migrate().unwrap();
        db.ensure_general_flow().unwrap();
        let store: SharedStore = Arc::new(Mutex::new(db));
        let caller = Caller::new(ALICE);
        let req = |name: &str, mut args: serde_json::Value| {
            // The daemon overwrites created_by with the kernel uid; here it is
            // deliberately forged to prove ownership does not come from JSON.
            args["created_by"] = serde_json::json!("uid:7");
            Envelope::new(Payload::Command {
                name: name.into(),
                args,
            })
        };

        let flow = ok(
            flow::handle_command(
                &store,
                &req("flow.create", serde_json::json!({"name": "f"})),
                caller,
            ),
            "flow",
        );
        let flow_id = flow["id"].as_str().unwrap().to_owned();
        let goal = ok(
            goal::handle_command(
                &store,
                &req("goal.create", serde_json::json!({"objective": "o"})),
                caller,
            ),
            "goal",
        );
        let goal_id = goal["id"].as_str().unwrap().to_owned();
        ok(
            goal::handle_command(
                &store,
                &req(
                    "goal.child",
                    serde_json::json!({"id": goal_id, "objective": "c"}),
                ),
                caller,
            ),
            "child",
        );
        ok(
            session::handle_command(
                &store,
                &req("session.create", serde_json::json!({"flow_id": flow_id})),
                caller,
            ),
            "session",
        );
        ok(
            agentic::handle_command(
                &store,
                &req("execution.create", serde_json::json!({"goal_id": goal_id})),
                caller,
            ),
            "execution",
        );
        ok(
            generation::handle_command(
                &store,
                &req("generation.create", serde_json::json!({})),
                caller,
            ),
            "generation",
        );
        let profile = ok(
            compute::handle_command(
                &store,
                &req(
                    "profile.create",
                    serde_json::json!({"name": "p", "toml": "capability_tier = 'advanced'\n"}),
                ),
                caller,
            ),
            "profile",
        );
        let profile_id = profile["id"].as_str().unwrap().to_owned();
        ok(
            compute::handle_command(
                &store,
                &req(
                    "profile.clone",
                    serde_json::json!({"id": profile_id, "name": "p2"}),
                ),
                caller,
            ),
            "clone",
        );
        ok(
            compute::handle_command(
                &store,
                &req(
                    "provider.register",
                    serde_json::json!({"name": "prov", "capabilities": ["text"]}),
                ),
                caller,
            ),
            "provider",
        );
        let model = ok(
            compute::handle_command(
                &store,
                &req("model.register", serde_json::json!({"name": "m"})),
                caller,
            ),
            "model",
        );
        let workload = ok(
            compute::handle_command(
                &store,
                &req(
                    "workload.submit",
                    serde_json::json!({"profile_id": profile_id, "model_id": model["id"]}),
                ),
                caller,
            ),
            "workload",
        );
        ok(
            compute::handle_command(
                &store,
                &req(
                    "usage.record",
                    serde_json::json!({
                        "workload_id": workload["id"], "tokens_in": 1, "tokens_out": 1, "latency_ms": 1, "cost_micros": 0
                    }),
                ),
                caller,
            ),
            "usage",
        );

        let conn = rusqlite::Connection::open(&path).unwrap();
        let tables = [
            "flows",
            "goals",
            "sessions",
            "executions",
            "generations",
            "ai_profiles",
            "ai_providers",
            "inference_workloads",
            "inference_usage",
        ];
        for table in tables {
            let unresolved: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE owner_class = 'unresolved'"),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(unresolved, 0, "{table} has rows without an owner");
            let rows: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert!(rows >= 1, "{table} was not exercised");
            let foreign: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE owner_class = 'user' AND owner_uid != ?1"),
                    [ALICE],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(foreign, 0, "{table} has a user row owned by another uid");
        }
        // Two Goals (create + child), two profiles (create + clone).
        for (table, expected) in [("goals", 2), ("ai_profiles", 2)] {
            let owned: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE owner_uid = ?1"),
                    [ALICE],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(owned, expected, "{table}");
        }
        // General and providers are system; the forged created_by stayed audit text.
        let general: String = conn
            .query_row(
                "SELECT owner_class FROM flows WHERE name = 'General'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(general, "system");
        let providers: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ai_providers WHERE owner_class = 'system'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(providers, 1);
        let created_by: String = conn
            .query_row(
                "SELECT created_by FROM goals WHERE id = ?1",
                [&goal_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(created_by, "uid:7");
    }
}

/// #225 PRs 3–4: Flow, Goal, Session, Execution and provenance handlers (and
/// Generation's Goal references) reach storage only through the owner-scoped
/// API. The unscoped helpers remain for bootstrap,
/// offline self-tests and unit tests, so production handler code is scanned.
#[cfg(test)]
mod owner_scoped_storage {
    const HANDLERS: [(&str, &str); 13] = [
        ("compute", include_str!("compute/mod.rs")),
        ("compute/profiles", include_str!("compute/profiles.rs")),
        ("compute/admission", include_str!("compute/admission.rs")),
        ("compute/inference", include_str!("compute/inference.rs")),
        ("flow", include_str!("flow/mod.rs")),
        ("goal", include_str!("goal/mod.rs")),
        ("session", include_str!("session/mod.rs")),
        ("session/pty", include_str!("session/pty.rs")),
        ("execution", include_str!("agentic/mod.rs")),
        ("provenance", include_str!("provenance/mod.rs")),
        ("generation", include_str!("generation/mod.rs")),
        ("ownership", include_str!("ownership/mod.rs")),
        ("main", include_str!("main.rs")),
    ];

    /// Reads and mutations that ignore the caller.
    const UNSCOPED: [&str; 17] = [
        ".get_flow(",
        ".list_flows(",
        ".get_goal(",
        ".list_goals(",
        ".get_session(",
        ".list_sessions(",
        ".get_execution(",
        ".list_executions(",
        ".set_flow_state(",
        ".set_goal_status(",
        ".status_counts(",
        ".provenance_from(",
        ".provenance_to(",
        ".usage_totals_per_model(",
        ".get_workload(",
        ".get_generation(",
        ".list_generations(",
    ];

    /// Production code: everything before the offline self-test and tests.
    fn production(source: &str) -> &str {
        let end = ["pub(crate) async fn self_test", "#[cfg(test)]"]
            .iter()
            .filter_map(|marker| source.find(marker))
            .min()
            .unwrap_or(source.len());
        &source[..end]
    }

    #[test]
    fn handlers_use_only_owner_scoped_storage() {
        for (name, source) in HANDLERS {
            let code = production(source);
            for call in UNSCOPED {
                if name == "compute/admission"
                    && matches!(call, ".get_workload(" | ".usage_totals_per_model(")
                {
                    continue; // see the next test
                }
                assert!(!code.contains(call), "{name} calls unscoped `{call}`");
            }
        }
    }

    /// `usage.record` (Admin) writes the shared usage ledger for any workload
    /// and is the only caller-blind workload read; an Admin audit design
    /// (PR 7) replaces it.
    #[test]
    fn unscoped_workload_reads_are_confined_to_admin_usage_record() {
        let admission = production(include_str!("compute/admission.rs"));
        let start = admission
            .find("fn record_usage(")
            .expect("record_usage exists");
        let end = start + admission[start..].find("\n}\n").expect("record_usage ends");
        let record_usage = &admission[start..end];
        for call in [".get_workload(", ".usage_totals_per_model("] {
            assert_eq!(record_usage.matches(call).count(), 1, "{call}");
            assert_eq!(
                admission.matches(call).count(),
                1,
                "{call} outside record_usage"
            );
        }
    }

    /// The PTY supervisor (service-account-only commands, authorized by the
    /// guardian's control token) updates the state of Sessions it launched
    /// and reconciles them after restart; nothing else may write a Session
    /// state without the caller.
    #[test]
    fn unscoped_session_state_writes_are_confined_to_pty_reconciliation() {
        for (name, source) in HANDLERS {
            let code = production(source);
            let writes = code.matches(".update_session_state(").count()
                + code.matches(".update_session_presentation_state(").count();
            let allowed = if name == "session/pty" { 6 } else { 0 };
            assert_eq!(writes, allowed, "{name}");
        }
    }
}
