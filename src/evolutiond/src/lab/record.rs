//! `lab record`: a verified KVM result becomes the candidate's Experiment
//! evidence, then `generation.test` or `generation.reject`, through flowerlyd
//! as the invoking Admin (ADR-0024 D3, D5). Nothing here can activate.

use std::path::Path;
use std::str::FromStr;

use anyhow::{anyhow, bail, Context};
use flowerly_core::ipc::{self, Payload};
use flowerly_core::storage::experiment::ExperimentDetails;
use flowerly_core::types::GenerationId;
use serde_json::{json, Value};

use super::plan::Accel;
use super::result::{self, Verified};
use crate::promotion::Verdict;

/// `reason` bounds of flowerlyd's audited Generation commands.
const MAX_REASON: usize = 512;

/// What the Experiment records about this run.
pub fn details(verified: &Verified) -> anyhow::Result<ExperimentDetails> {
    let plan = &verified.plan;
    let baseline = plan
        .baseline
        .as_deref()
        .map(GenerationId::from_str)
        .transpose()
        .map_err(|e| anyhow!("baseline: {e}"))?;
    let details = ExperimentDetails {
        hypothesis: plan.hypothesis().to_owned(),
        baseline,
        test_plan: plan
            .checks
            .iter()
            .map(|check| check.name().to_owned())
            .collect(),
        metrics: verified
            .result
            .metrics
            .iter()
            .map(|metric| (metric.name.clone(), metric.value))
            .collect(),
        artifacts: verified.result.artifacts.clone(),
        lane: Some(plan.lane),
    };
    details
        .validate()
        .context("the Experiment details are out of bounds")?;
    Ok(details)
}

/// One line for the audit log, without control characters, within bounds.
pub fn reason(verified: &Verified) -> String {
    let decision = &verified.result.decision;
    let text = match decision.verdict {
        Verdict::Passed => format!(
            "evolution lab: every planned check passed on {} boot(s); evidence {}",
            verified.result.boots.len(),
            verified.evidence_sha256
        ),
        Verdict::Failed => format!(
            "evolution lab: {} failure(s), first: {}",
            decision.reasons.len(),
            decision.reasons.first().map_or("none", String::as_str)
        ),
    };
    let mut line: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if line.len() > MAX_REASON {
        let mut end = MAX_REASON;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.truncate(end);
    }
    line
}

/// What flowerlyd recorded.
#[derive(Debug)]
pub struct Recorded {
    pub experiment: String,
    pub status: String,
}

async fn command(socket: &Path, name: &str, args: Value) -> anyhow::Result<Value> {
    let reply = ipc::call(
        socket,
        Payload::Command {
            name: name.to_owned(),
            args,
        },
    )
    .await
    .with_context(|| format!("calling {name} on {}", socket.display()))?;
    match reply.payload {
        Payload::Ok { data } => Ok(data),
        Payload::Err { message, code } => bail!("{name} refused ({code:?}): {message}"),
        other => bail!("{name}: unexpected reply {other:?}"),
    }
}

/// Verify `dir` again, then record it. Only a KVM run is evidence.
pub async fn record(dir: &Path, runtime_dir: &Path) -> anyhow::Result<Recorded> {
    let verified = result::verify(dir).map_err(|e| anyhow!("{}: {e}", dir.display()))?;
    if verified.result.accel != Accel::Kvm {
        bail!("only a KVM run is evidence; this result was booted with TCG");
    }
    let details = details(&verified)?;
    let reason = reason(&verified);
    let id = &verified.result.candidate;
    let socket = ipc::socket_path(runtime_dir, "flowerlyd");
    let decision = &verified.result.decision;

    let evidence = command(
        &socket,
        "generation.evidence",
        json!({
            "id": id,
            "outcome": decision.verdict.outcome(),
            "evidence_sha256": verified.evidence_sha256,
            "reason": reason,
            "experiment": details,
        }),
    )
    .await?;
    let experiment = evidence
        .get("experiment")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("generation.evidence returned no Experiment"))?
        .to_owned();

    // `generation.test` and `generation.reject` take the same arguments here;
    // a rejection names the Experiment so its decision becomes `rejected`.
    let finished = command(
        &socket,
        decision.recommendation.command(),
        json!({ "id": id, "experiment": experiment, "reason": reason }),
    )
    .await?;
    let status = finished
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    Ok(Recorded { experiment, status })
}
