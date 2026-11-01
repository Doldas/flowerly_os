use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use flowerly_core::default_runtime_dir;
use flowerly_core::ipc::{Envelope, Payload};
use serde_json::Value;
use tokio::runtime::Runtime;

#[derive(Debug, Parser)]
#[command(name = "agenticctl", version, about = "Flowerly OS control plane")]
struct Cli {
    #[arg(
        long,
        global = true,
        env = "AGENTIC_RUNTIME_DIR",
        help = "Override the runtime directory holding the daemon sockets."
    )]
    runtime_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Liveness probe for a service.")]
    Ping {
        #[arg(
            default_value = "flowerlyd",
            help = "Service to ping (default: flowerlyd)."
        )]
        service: String,
    },
    #[command(about = "Fetch structured status from a service.")]
    Status {
        #[arg(
            default_value = "flowerlyd",
            help = "Service to query (default: flowerlyd)."
        )]
        service: String,
    },
    #[command(about = "Flow operations.")]
    Flow(FlowArgs),
    #[command(about = "Goal operations.")]
    Goal(GoalArgs),
    #[command(about = "Session operations.")]
    Session(SessionArgs),
    #[command(about = "Generation operations.")]
    Generation(GenerationArgs),
    #[command(about = "Provenance operations.")]
    Provenance(ProvenanceArgs),
    #[command(about = "Execution operations.")]
    Execution(ExecutionArgs),
    #[command(about = "Manage local AI model catalog metadata (prototype).")]
    Model(ModelArgs),
    #[command(about = "Inspect local device resource observations (advisory only).")]
    Device(DeviceArgs),
    #[command(about = "Inspect or reassign data ownership (Admin, audited).")]
    Ownership(OwnershipArgs),
    #[command(about = "List the capability catalog.")]
    Capability,
}

#[derive(Debug, Args)]
struct FlowArgs {
    #[command(subcommand)]
    action: FlowAction,
}

#[derive(Debug, Subcommand)]
enum FlowAction {
    #[command(about = "Create a Flow.")]
    Create {
        #[arg(help = "Flow name.")]
        name: String,
        #[arg(
            long,
            help = "Reuse a durable operation ID for safe retries within the retention window."
        )]
        operation_id: Option<String>,
    },
    #[command(about = "List Flows.")]
    List,
    #[command(about = "Activate a Flow (by id or name).")]
    Activate {
        #[arg(help = "Flow id or name.")]
        target: String,
    },
    #[command(about = "Archive a Flow (by id or name).")]
    Archive {
        #[arg(help = "Flow id or name.")]
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
    #[command(about = "Create a Goal.")]
    Create {
        #[arg(help = "Objective.")]
        objective: String,
        #[arg(long, help = "Priority.")]
        priority: Option<i32>,
        #[arg(
            long,
            help = "Reuse a durable operation ID for safe retries within the retention window."
        )]
        operation_id: Option<String>,
    },
    #[command(about = "Create a child Goal under one of your Goals.")]
    Child {
        #[arg(help = "Parent Goal id.")]
        parent: String,
        #[arg(help = "Objective.")]
        objective: String,
        #[arg(
            long,
            help = "Reuse a durable operation ID for safe retries within the retention window."
        )]
        operation_id: Option<String>,
    },
    #[command(about = "List Goals.")]
    List,
    #[command(about = "Show a Goal.")]
    Show {
        #[arg(help = "Goal id.")]
        id: String,
    },
    #[command(about = "Mark a Goal completed.")]
    Complete {
        #[arg(help = "Goal id.")]
        id: String,
    },
    #[command(about = "Abandon a Goal.")]
    Abandon {
        #[arg(help = "Goal id.")]
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
    #[command(about = "Register a session.")]
    Create {
        #[arg(
            long,
            default_value = "local-pty",
            help = "Session kind (local-pty, ssh, container, vm-console)."
        )]
        kind: String,
        #[arg(long, help = "Associate the session with a Flow id.")]
        flow_id: Option<String>,
        #[arg(long, help = "Known process id for a registered runtime.")]
        pid: Option<i32>,
        #[arg(long, help = "Working directory.")]
        cwd: Option<String>,
        #[arg(
            long,
            help = "Reuse a durable operation ID for safe retries within the retention window."
        )]
        operation_id: Option<String>,
    },
    #[command(about = "Spawn a guarded local PTY; never retry automatically after timeout.")]
    Spawn {
        #[arg(long, default_value = "/bin/sh", help = "Allowlisted shell path.")]
        shell: String,
        #[arg(
            long,
            help = "Stable durable reservation ID; required for safe retry admission."
        )]
        operation_id: String,
    },
    #[command(about = "Read a PTY spawn reservation without starting another process.")]
    SpawnLookup {
        #[arg(help = "Original spawn operation ID.")]
        operation_id: String,
    },
    #[command(about = "List registered sessions.")]
    List,
    #[command(about = "Mark a session exited.")]
    Terminate {
        #[arg(help = "Session id.")]
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
    #[command(about = "Create a candidate generation.")]
    Create {
        #[arg(long, help = "Parent generation id.")]
        parent: Option<String>,
        #[arg(long, help = "Source commit.")]
        source_commit: Option<String>,
        #[arg(long, help = "Resolved system manifest identity.")]
        system_manifest: Option<String>,
        #[arg(long, help = "Goal that motivated this generation.")]
        reason_goal: Option<String>,
        #[arg(long, help = "Experiment associated with this generation.")]
        experiment: Option<String>,
    },
    #[command(about = "List generations.")]
    List,
    #[command(about = "Inspect a generation.")]
    Inspect {
        #[arg(help = "Generation id.")]
        id: String,
    },
    #[command(about = "Activate a generation.")]
    Activate {
        #[arg(help = "Generation id.")]
        id: String,
    },
    #[command(
        about = "Promote a private candidate to a shared system Generation (Admin, audited)."
    )]
    Promote {
        #[arg(help = "Generation id.")]
        id: String,
        #[arg(long, help = "Reason recorded in the admin audit log.")]
        reason: String,
    },
}

#[derive(Debug, Args)]
struct ProvenanceArgs {
    #[command(subcommand)]
    action: ProvenanceAction,
}

#[derive(Debug, Subcommand)]
enum ProvenanceAction {
    #[command(about = "Add a provenance edge.")]
    Add {
        #[arg(long, help = "Source object reference.")]
        from: String,
        #[arg(long, help = "Relation name.")]
        relation: String,
        #[arg(long, help = "Destination object reference.")]
        to: String,
        #[arg(long, help = "Optional note.")]
        note: Option<String>,
    },
    #[command(about = "Query edges originating from an object.")]
    From {
        #[arg(help = "Object reference.")]
        reference: String,
    },
    #[command(about = "Query edges pointing to an object.")]
    To {
        #[arg(help = "Object reference.")]
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
    #[command(about = "Record an execution.")]
    Create {
        #[arg(long, help = "Command or operation executed.")]
        command: String,
        #[arg(long, help = "Reason for the execution.")]
        reason: Option<String>,
        #[arg(long, help = "Agent id.")]
        agent_id: Option<String>,
        #[arg(long, help = "Goal id.")]
        goal_id: Option<String>,
        #[arg(long, help = "Exit code.")]
        exit_code: Option<i32>,
        #[arg(long, help = "Execution duration in milliseconds.")]
        duration_ms: Option<i64>,
    },
    #[command(about = "Show an execution.")]
    Show {
        #[arg(help = "Execution id.")]
        id: String,
    },
    #[command(about = "List executions.")]
    List,
}

#[derive(Debug, Args)]
struct ModelArgs {
    #[command(subcommand)]
    action: ModelAction,
}

#[derive(Debug, Subcommand)]
enum ModelAction {
    #[command(about = "Register local model metadata, not a model executable.")]
    Register {
        #[arg(help = "Display name for this model record.")]
        name: String,
        #[arg(long)]
        family: Option<String>,
        #[arg(long)]
        architecture: Option<String>,
        #[arg(
            long,
            help = "Recorded execution backend (CPU, CUDA, VULKAN, ROCM, NPU)."
        )]
        backend: Option<String>,
        #[arg(long, help = "Recorded context window, if known.")]
        context_tokens: Option<u64>,
        #[arg(long, help = "Measured memory footprint in bytes, if known.")]
        measured_memory_bytes: Option<u64>,
        #[arg(long = "modality", help = "Modality label; repeat to record several.")]
        modalities: Vec<String>,
        #[arg(long)]
        license: Option<String>,
        #[arg(
            long,
            help = "Measurement source or evidence label, not inferred performance."
        )]
        evidence: Option<String>,
        #[arg(long, help = "Durable key for safe repeated registration requests.")]
        operation_id: Option<String>,
    },
    #[command(about = "List active model records (retired records remain inspectable).")]
    List,
    #[command(about = "Show a model record, including retired metadata.")]
    Show {
        #[arg(help = "Model id.")]
        id: String,
    },
    #[command(about = "Soft-retire a model record without deleting provenance.")]
    Retire {
        #[arg(help = "Model id.")]
        id: String,
    },
    #[command(about = "Check advisory compatibility against measured memory.")]
    Compatibility {
        #[arg(help = "Model id.")]
        id: String,
    },
}

#[derive(Debug, Args)]
struct DeviceArgs {
    #[command(subcommand)]
    action: DeviceAction,
}

#[derive(Debug, Args)]
struct OwnershipArgs {
    #[command(subcommand)]
    action: OwnershipAction,
}

#[derive(Debug, Subcommand)]
enum OwnershipAction {
    #[command(about = "Show an object's owner and its same-owner component (no content).")]
    Inspect {
        #[arg(help = "Object reference, e.g. goal-<id>.")]
        reference: String,
        #[arg(long, help = "Reason recorded in the admin audit log.")]
        reason: String,
    },
    #[command(about = "Give an object's whole component to a user.")]
    Claim {
        #[arg(help = "Object reference, e.g. goal-<id>.")]
        reference: String,
        #[arg(long, help = "Numeric uid of the new owner.")]
        to_uid: u32,
        #[arg(long, help = "Reason recorded in the admin audit log.")]
        reason: String,
        #[arg(
            long,
            help = "Component size reported by `ownership inspect`; a stale value moves nothing."
        )]
        expect: usize,
    },
    #[command(about = "Make an object's whole component unresolved (protected) again.")]
    Release {
        #[arg(help = "Object reference, e.g. goal-<id>.")]
        reference: String,
        #[arg(long, help = "Reason recorded in the admin audit log.")]
        reason: String,
        #[arg(
            long,
            help = "Component size reported by `ownership inspect`; a stale value moves nothing."
        )]
        expect: usize,
    },
}

#[derive(Debug, Subcommand)]
enum DeviceAction {
    #[command(about = "Read local memory observations and explicit enforcement limits.")]
    Budget,
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
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

async fn dispatch(runtime_dir: &std::path::Path, command: Command) -> Result<Value> {
    match command {
        Command::Ping { service } => {
            let response = call(&socket(runtime_dir, &service), Payload::Ping).await?;
            reply_value(response)
        }
        Command::Status { service } => {
            let response = call(&socket(runtime_dir, &service), Payload::Status).await?;
            reply_value(response)
        }
        Command::Flow(action) => dispatch_flow(runtime_dir, action.action).await,
        Command::Goal(action) => dispatch_goal(runtime_dir, action.action).await,
        Command::Session(action) => dispatch_session(runtime_dir, action.action).await,
        Command::Generation(action) => dispatch_generation(runtime_dir, action.action).await,
        Command::Provenance(action) => dispatch_provenance(runtime_dir, action.action).await,
        Command::Execution(action) => dispatch_execution(runtime_dir, action.action).await,
        Command::Model(action) => dispatch_model(runtime_dir, action.action).await,
        Command::Device(action) => dispatch_device(runtime_dir, action.action).await,
        Command::Ownership(action) => dispatch_ownership(runtime_dir, action.action).await,
        Command::Capability => {
            let response = call(
                &socket(runtime_dir, "capabilityd"),
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
        FlowAction::Create { name, operation_id } => Payload::Command {
            name: "flow.create".into(),
            args: serde_json::json!({ "name": name, "created_by": "agenticctl", "operation_id": operation_id }),
        },
        FlowAction::List => Payload::Command {
            name: "flow.list".into(),
            args: serde_json::json!({}),
        },
        FlowAction::Activate { target } => Payload::Command {
            name: "flow.activate".into(),
            args: flow_reference(&target),
        },
        FlowAction::Archive { target } => Payload::Command {
            name: "flow.archive".into(),
            args: flow_reference(&target),
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
            operation_id,
        } => Payload::Command {
            name: "goal.create".into(),
            args: serde_json::json!({
                "objective": objective,
                "priority": priority,
                "operation_id": operation_id,
                "created_by": "agenticctl",
            }),
        },
        GoalAction::Child {
            parent,
            objective,
            operation_id,
        } => Payload::Command {
            name: "goal.child".into(),
            args: serde_json::json!({
                "id": parent,
                "objective": objective,
                "operation_id": operation_id,
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
        SessionAction::Create {
            kind,
            flow_id,
            pid,
            cwd,
            operation_id,
        } => Payload::Command {
            name: "session.create".into(),
            args: serde_json::json!({
                "kind": kind,
                "flow_id": flow_id,
                "pid": pid,
                "cwd": cwd,
                "operation_id": operation_id
            }),
        },
        SessionAction::Spawn {
            shell,
            operation_id,
        } => Payload::Command {
            name: "session.spawn".into(),
            args: serde_json::json!({ "shell": shell, "operation_id": operation_id }),
        },
        SessionAction::SpawnLookup { operation_id } => Payload::Command {
            name: "session.spawn.lookup".into(),
            args: serde_json::json!({ "operation_id": operation_id }),
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
        GenerationAction::Create {
            parent,
            source_commit,
            system_manifest,
            reason_goal,
            experiment,
        } => Payload::Command {
            name: "generation.create".into(),
            args: serde_json::json!({
                "parent": parent,
                "source_commit": source_commit,
                "system_manifest": system_manifest,
                "reason_goal": reason_goal,
                "experiment": experiment,
            }),
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
        GenerationAction::Promote { id, reason } => Payload::Command {
            name: "generation.promote".into(),
            args: serde_json::json!({ "id": id, "reason": reason }),
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
        ProvenanceAction::To { reference } => Payload::Command {
            name: "provenance.to".into(),
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
            reason,
            agent_id,
            goal_id,
            exit_code,
            duration_ms,
        } => Payload::Command {
            name: "execution.create".into(),
            args: serde_json::json!({
                "command": command,
                "reason": reason,
                "agent_id": agent_id,
                "goal_id": goal_id,
                "exit_code": exit_code,
                "duration_ms": duration_ms,
            }),
        },
        ExecutionAction::Show { id } => Payload::Command {
            name: "execution.show".into(),
            args: serde_json::json!({ "id": id }),
        },
        ExecutionAction::List => Payload::Command {
            name: "execution.list".into(),
            args: serde_json::json!({}),
        },
    };
    reply_value(call(&socket, payload).await?)
}

async fn dispatch_model(runtime_dir: &std::path::Path, action: ModelAction) -> Result<Value> {
    let payload = match action {
        ModelAction::Register {
            name,
            family,
            architecture,
            backend,
            context_tokens,
            measured_memory_bytes,
            modalities,
            license,
            evidence,
            operation_id,
        } => Payload::Command {
            name: "model.register".into(),
            args: serde_json::json!({
                "name": name,
                "family": family,
                "architecture": architecture,
                "backend": backend,
                "context_tokens": context_tokens,
                "measured_memory_bytes": measured_memory_bytes,
                "modalities": modalities,
                "license": license,
                "evidence": evidence,
                "operation_id": operation_id,
            }),
        },
        ModelAction::List => Payload::Command {
            name: "model.list".into(),
            args: serde_json::json!({}),
        },
        ModelAction::Show { id } => Payload::Command {
            name: "model.show".into(),
            args: serde_json::json!({ "id": id }),
        },
        ModelAction::Retire { id } => Payload::Command {
            name: "model.retire".into(),
            args: serde_json::json!({ "id": id }),
        },
        ModelAction::Compatibility { id } => Payload::Command {
            name: "model.compatibility".into(),
            args: serde_json::json!({ "id": id }),
        },
    };
    reply_value(call(&socket(runtime_dir, "flowerlyd"), payload).await?)
}

async fn dispatch_device(runtime_dir: &std::path::Path, action: DeviceAction) -> Result<Value> {
    let payload = match action {
        DeviceAction::Budget => Payload::Command {
            name: "device.budget".into(),
            args: serde_json::json!({}),
        },
    };
    reply_value(call(&socket(runtime_dir, "flowerlyd"), payload).await?)
}

async fn dispatch_ownership(
    runtime_dir: &std::path::Path,
    action: OwnershipAction,
) -> Result<Value> {
    let payload = match action {
        OwnershipAction::Inspect { reference, reason } => Payload::Command {
            name: "ownership.inspect".into(),
            args: serde_json::json!({ "ref": reference, "reason": reason }),
        },
        OwnershipAction::Claim {
            reference,
            to_uid,
            reason,
            expect,
        } => Payload::Command {
            name: "ownership.claim".into(),
            args: serde_json::json!({
                "ref": reference,
                "to_uid": to_uid,
                "reason": reason,
                "expect": expect,
            }),
        },
        OwnershipAction::Release {
            reference,
            reason,
            expect,
        } => Payload::Command {
            name: "ownership.release".into(),
            args: serde_json::json!({ "ref": reference, "reason": reason, "expect": expect }),
        },
    };
    reply_value(call(&socket(runtime_dir, "flowerlyd"), payload).await?)
}

fn flow_reference(target: &str) -> Value {
    if target.starts_with("flow-") {
        serde_json::json!({ "id": target })
    } else {
        serde_json::json!({ "name": target })
    }
}

fn socket(runtime_dir: &std::path::Path, service: &str) -> PathBuf {
    let daemon = if service == "evolutiond" {
        "evolutiond"
    } else {
        "flowerlyd"
    };
    flowerly_core::ipc::socket_path(runtime_dir, daemon)
}

async fn call(socket: &std::path::Path, payload: Payload) -> Result<Envelope> {
    flowerly_core::ipc::call(socket, payload)
        .await
        .with_context(|| {
            format!(
                "cannot contact service at {} — is the daemon running?",
                socket.display()
            )
        })
}

fn reply_value(envelope: Envelope) -> Result<Value> {
    match envelope.payload {
        Payload::Ok { data } => Ok(data),
        Payload::Err { message, code } => match code {
            Some(code) => anyhow::bail!("service error ({code:?}): {message}"),
            None => anyhow::bail!("service error: {message}"),
        },
        other => Ok(serde_json::json!({ "type": format!("{other:?}") })),
    }
}
