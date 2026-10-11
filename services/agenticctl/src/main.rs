//! `agenticctl` — the command-line control plane for Flowerly OS.
//!
//! Humans and agents use the same interface: a thin client that talks the
//! local IPC protocol to the running daemons and renders their JSON replies.
//! No privileged logic lives here — this binary only inspects and drives the
//! same API the GUI shell will use.

use std::io::Write;
use std::path::PathBuf;

use agentic_core::default_runtime_dir;
use agentic_ipc::{self, Envelope, Payload};
use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use serde_json::Value;
use tokio::runtime::Runtime;

#[derive(Debug, Parser)]
#[command(name = "agenticctl", version, about = "Flowerly OS control plane")]
struct Cli {
    /// Override the runtime directory holding the daemon sockets.
    #[arg(long, global = true, env = "AGENTIC_RUNTIME_DIR")]
    runtime_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Liveness probe for a service.
    Ping {
        /// Service to ping (default: agenticd).
        #[arg(default_value = "agenticd")]
        service: String,
    },
    /// Fetch structured status from a service.
    Status {
        /// Service to query (default: agenticd).
        #[arg(default_value = "agenticd")]
        service: String,
    },
    /// Flow operations.
    Flow(FlowArgs),
    /// Goal operations.
    Goal(GoalArgs),
    /// Session operations.
    Session(SessionArgs),
    /// Generation operations.
    Generation(GenerationArgs),
    /// Provenance operations.
    Provenance(ProvenanceArgs),
    /// Execution operations.
    Execution(ExecutionArgs),
    /// List the capability catalog.
    Capability,
}

#[derive(Debug, Args)]
struct FlowArgs {
    #[command(subcommand)]
    action: FlowAction,
}

#[derive(Debug, Subcommand)]
enum FlowAction {
    /// Create a Flow.
    Create {
        /// Flow name.
        name: String,
    },
    /// List Flows.
    List,
    /// Activate a Flow (by id or name).
    Activate {
        /// Flow id or name.
        target: String,
    },
    /// Archive a Flow (by id or name).
    Archive {
        /// Flow id or name.
        target: String,
    },
}

#[derive(Debug, Args)]
struct GoalArgs {
    #[command(subcommand)]
    action: GoalAction,
}

#[derive(Debug, Subcommand)]
enum GoalAction {
    /// Create a Goal.
    Create {
        /// Objective.
        objective: String,
        /// Priority.
        #[arg(long)]
        priority: Option<i32>,
    },
    /// List Goals.
    List,
    /// Show a Goal.
    Show {
        /// Goal id.
        id: String,
    },
    /// Mark a Goal completed.
    Complete {
        /// Goal id.
        id: String,
    },
    /// Abandon a Goal.
    Abandon {
        /// Goal id.
        id: String,
    },
}

#[derive(Debug, Args)]
struct SessionArgs {
    #[command(subcommand)]
    action: SessionAction,
}

#[derive(Debug, Subcommand)]
enum SessionAction {
    /// Register a session.
    Create {
        /// Session kind (local-pty, ssh, container, vm-console).
        #[arg(long, default_value = "local-pty")]
        kind: String,
        /// Working directory.
        #[arg(long)]
        cwd: Option<String>,
    },
    /// List registered sessions.
    List,
    /// Mark a session exited.
    Terminate {
        /// Session id.
        id: String,
    },
}

#[derive(Debug, Args)]
struct GenerationArgs {
    #[command(subcommand)]
    action: GenerationAction,
}

#[derive(Debug, Subcommand)]
enum GenerationAction {
    /// Create a candidate generation.
    Create {
        /// Source commit.
        #[arg(long)]
        source_commit: Option<String>,
    },
    /// List generations.
    List,
    /// Inspect a generation.
    Inspect {
        /// Generation id.
        id: String,
    },
    /// Activate a generation.
    Activate {
        /// Generation id.
        id: String,
    },
}

#[derive(Debug, Args)]
struct ProvenanceArgs {
    #[command(subcommand)]
    action: ProvenanceAction,
}

#[derive(Debug, Subcommand)]
enum ProvenanceAction {
    /// Add a provenance edge.
    Add {
        /// Source object reference.
        #[arg(long)]
        from: String,
        /// Relation name.
        #[arg(long)]
        relation: String,
        /// Destination object reference.
        #[arg(long)]
        to: String,
        /// Optional note.
        #[arg(long)]
        note: Option<String>,
    },
    /// Query edges originating from an object.
    From {
        /// Object reference.
        reference: String,
    },
}

#[derive(Debug, Args)]
struct ExecutionArgs {
    #[command(subcommand)]
    action: ExecutionAction,
}

#[derive(Debug, Subcommand)]
enum ExecutionAction {
    /// Record an execution.
    Create {
        /// Command or operation executed.
        #[arg(long)]
        command: String,
        /// Agent id.
        #[arg(long)]
        agent_id: Option<String>,
        /// Goal id.
        #[arg(long)]
        goal_id: Option<String>,
        /// Exit code.
        #[arg(long)]
        exit_code: Option<i32>,
    },
    /// Show an execution.
    Show {
        /// Execution id.
        id: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let runtime_dir = cli.runtime_dir.unwrap_or_else(default_runtime_dir);

    let rt = Runtime::new().context("creating tokio runtime")?;
    let result = rt.block_on(dispatch(&runtime_dir, cli.command))?;
    let pretty = serde_json::to_string_pretty(&result)?;
    let mut out = std::io::stdout().lock();
    match writeln!(out, "{pretty}") {
        Ok(()) => {}
        // Piping into `head`/`jq` and closing early is normal CLI usage.
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

async fn dispatch(runtime_dir: &std::path::Path, command: Command) -> Result<Value> {
    match command {
        Command::Ping { service } => {
            let socket = socket(runtime_dir, &service);
            let response = call(&socket, Payload::Ping).await?;
            reply_value(response)
        }
        Command::Status { service } => {
            let socket = socket(runtime_dir, &service);
            let response = call(&socket, Payload::Status).await?;
            reply_value(response)
        }
        Command::Flow(action) => dispatch_flow(runtime_dir, action.action).await,
        Command::Goal(action) => dispatch_goal(runtime_dir, action.action).await,
        Command::Session(action) => dispatch_session(runtime_dir, action.action).await,
        Command::Generation(action) => dispatch_generation(runtime_dir, action.action).await,
        Command::Provenance(action) => dispatch_provenance(runtime_dir, action.action).await,
        Command::Execution(action) => dispatch_execution(runtime_dir, action.action).await,
        Command::Capability => {
            let socket = socket(runtime_dir, "capabilityd");
            let response = call(
                &socket,
                Payload::Command {
                    name: "capability.list".into(),
                    args: serde_json::json!({}),
                },
            )
            .await?;
            reply_value(response)
        }
    }
}

async fn dispatch_flow(runtime_dir: &std::path::Path, action: FlowAction) -> Result<Value> {
    let socket = socket(runtime_dir, "flowd");
    let payload = match action {
        FlowAction::Create { name } => Payload::Command {
            name: "flow.create".into(),
            args: serde_json::json!({ "name": name, "created_by": "agenticctl" }),
        },
        FlowAction::List => Payload::Command {
            name: "flow.list".into(),
            args: serde_json::json!({}),
        },
        FlowAction::Activate { target } => Payload::Command {
            name: "flow.activate".into(),
            args: lookup_args(&target),
        },
        FlowAction::Archive { target } => Payload::Command {
            name: "flow.archive".into(),
            args: lookup_args(&target),
        },
    };
    reply_value(call(&socket, payload).await?)
}

async fn dispatch_goal(runtime_dir: &std::path::Path, action: GoalAction) -> Result<Value> {
    let socket = socket(runtime_dir, "goald");
    let payload = match action {
        GoalAction::Create {
            objective,
            priority,
        } => Payload::Command {
            name: "goal.create".into(),
            args: serde_json::json!({
                "objective": objective,
                "priority": priority,
                "created_by": "agenticctl",
            }),
        },
        GoalAction::List => Payload::Command {
            name: "goal.list".into(),
            args: serde_json::json!({}),
        },
        GoalAction::Show { id } => Payload::Command {
            name: "goal.show".into(),
            args: serde_json::json!({ "id": id }),
        },
        GoalAction::Complete { id } => Payload::Command {
            name: "goal.complete".into(),
            args: serde_json::json!({ "id": id }),
        },
        GoalAction::Abandon { id } => Payload::Command {
            name: "goal.abandon".into(),
            args: serde_json::json!({ "id": id }),
        },
    };
    reply_value(call(&socket, payload).await?)
}

async fn dispatch_session(runtime_dir: &std::path::Path, action: SessionAction) -> Result<Value> {
    let socket = socket(runtime_dir, "sessiond");
    let payload = match action {
        SessionAction::Create { kind, cwd } => Payload::Command {
            name: "session.create".into(),
            args: serde_json::json!({ "kind": kind, "cwd": cwd }),
        },
        SessionAction::List => Payload::Command {
            name: "session.list".into(),
            args: serde_json::json!({}),
        },
        SessionAction::Terminate { id } => Payload::Command {
            name: "session.terminate".into(),
            args: serde_json::json!({ "id": id }),
        },
    };
    reply_value(call(&socket, payload).await?)
}

async fn dispatch_generation(
    runtime_dir: &std::path::Path,
    action: GenerationAction,
) -> Result<Value> {
    let socket = socket(runtime_dir, "generationd");
    let payload = match action {
        GenerationAction::Create { source_commit } => Payload::Command {
            name: "generation.create".into(),
            args: serde_json::json!({ "source_commit": source_commit }),
        },
        GenerationAction::List => Payload::Command {
            name: "generation.list".into(),
            args: serde_json::json!({}),
        },
        GenerationAction::Inspect { id } => Payload::Command {
            name: "generation.inspect".into(),
            args: serde_json::json!({ "id": id }),
        },
        GenerationAction::Activate { id } => Payload::Command {
            name: "generation.activate".into(),
            args: serde_json::json!({ "id": id }),
        },
    };
    reply_value(call(&socket, payload).await?)
}

async fn dispatch_provenance(
    runtime_dir: &std::path::Path,
    action: ProvenanceAction,
) -> Result<Value> {
    let socket = socket(runtime_dir, "provenanced");
    let payload = match action {
        ProvenanceAction::Add {
            from,
            relation,
            to,
            note,
        } => Payload::Command {
            name: "provenance.add".into(),
            args: serde_json::json!({ "from": from, "relation": relation, "to": to, "note": note }),
        },
        ProvenanceAction::From { reference } => Payload::Command {
            name: "provenance.from".into(),
            args: serde_json::json!({ "ref": reference }),
        },
    };
    reply_value(call(&socket, payload).await?)
}

async fn dispatch_execution(
    runtime_dir: &std::path::Path,
    action: ExecutionAction,
) -> Result<Value> {
    let socket = socket(runtime_dir, "agenticd");
    let payload = match action {
        ExecutionAction::Create {
            command,
            agent_id,
            goal_id,
            exit_code,
        } => Payload::Command {
            name: "execution.create".into(),
            args: serde_json::json!({
                "command": command,
                "agent_id": agent_id,
                "goal_id": goal_id,
                "exit_code": exit_code,
            }),
        },
        ExecutionAction::Show { id } => Payload::Command {
            name: "execution.list".into(),
            args: serde_json::json!({ "id": id }),
        },
    };
    reply_value(call(&socket, payload).await?)
}

fn lookup_args(target: &str) -> Value {
    // `flow-...` is a valid FlowId prefix; anything else is treated as a name.
    if target.starts_with("flow-") {
        serde_json::json!({ "id": target })
    } else {
        serde_json::json!({ "name": target })
    }
}

fn socket(runtime_dir: &std::path::Path, service: &str) -> PathBuf {
    runtime_dir.join(format!("{service}.sock"))
}

async fn call(socket: &std::path::Path, payload: Payload) -> Result<Envelope> {
    agentic_ipc::call(socket, payload).await.with_context(|| {
        format!(
            "cannot contact service at {} — is the daemon running?",
            socket.display()
        )
    })
}

fn reply_value(envelope: Envelope) -> Result<Value> {
    match envelope.payload {
        Payload::Ok { data } => Ok(data),
        Payload::Err { message } => anyhow::bail!("service error: {message}"),
        other => Ok(serde_json::json!({ "type": format!("{other:?}") })),
    }
}
