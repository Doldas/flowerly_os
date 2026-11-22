//! The result document (`flowerly-lab-result/v1`): canonical JSON bound to the
//! plan, the serial logs and the reports by SHA-256. `lab verify` rehashes
//! every file and recomputes every check, metric and the decision.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::Path;

use flowerly_core::storage::experiment::{ExperimentArtifact, ExperimentLane};
use serde::{Deserialize, Serialize};

use super::checks::{self, CheckResult};
use super::plan::{Accel, Check, Plan, Subject, MAX_PLAN_BYTES};
use super::report::{ProbeReport, Received};
use super::sha256_hex;
use crate::benchmarks::{self, Metric};
use crate::promotion::{self, BootChecks, Decision};
use crate::vm::{self, Outcome};

pub const RESULT_SCHEMA: &str = "flowerly-lab-result/v1";
pub const RESULT_FILE: &str = "result.json";
pub const RESULT_DIGEST_FILE: &str = "result.sha256";
pub const PLAN_FILE: &str = "plan.json";
const MAX_RESULT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabResult {
    pub schema: String,
    /// The `evolutiond` version that ran the boots.
    pub evolutiond: String,
    pub plan_sha256: String,
    pub lane: ExperimentLane,
    pub candidate: String,
    pub baseline: Option<String>,
    pub accel: Accel,
    pub qemu: Binary,
    pub firmware: FirmwareHashes,
    pub disk: Disk,
    pub boots: Vec<BootResult>,
    pub metrics: Vec<Metric>,
    /// Every file of the output directory the result is bound to.
    pub artifacts: Vec<ExperimentArtifact>,
    pub decision: Decision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binary {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareHashes {
    pub code_sha256: String,
    /// The variable store template, before its private copy was booted.
    pub vars_sha256: String,
}

/// The candidate disk before the first boot (boots change it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Disk {
    pub sha256: String,
    pub bytes: u64,
    pub allocated_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Received,
    Missing,
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportState {
    pub status: ReportStatus,
    /// The decoded report file, when received.
    pub file: Option<String>,
    pub sha256: Option<String>,
    /// Why a report was refused.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootResult {
    pub label: String,
    pub subject: Subject,
    pub slot: String,
    pub serial: String,
    pub outcome: Outcome,
    pub report: ReportState,
    pub checks: BTreeMap<Check, CheckResult>,
}

/// File names of the `index`-th boot (from 1).
pub fn serial_name(index: usize, label: &str) -> String {
    format!("{index}-{label}.serial.log")
}

pub fn report_name(index: usize, label: &str) -> String {
    format!("{index}-{label}.report.json")
}

/// One boot's result from what the host saw and the final serial log.
pub fn boot_result(
    plan: &Plan,
    index: usize,
    outcome: Outcome,
    report: &Result<Option<Received>, String>,
) -> BootResult {
    let boot = &plan.boots[index - 1];
    let state = match report {
        Ok(Some(received)) => ReportState {
            status: ReportStatus::Received,
            file: Some(report_name(index, &boot.label)),
            sha256: Some(received.sha256.clone()),
            error: None,
        },
        Ok(None) => ReportState {
            status: ReportStatus::Missing,
            file: None,
            sha256: None,
            error: None,
        },
        Err(error) => ReportState {
            status: ReportStatus::Refused,
            file: None,
            sha256: None,
            error: Some(error.clone()),
        },
    };
    let probe = report
        .as_ref()
        .ok()
        .and_then(Option::as_ref)
        .map(|r| &r.report);
    let checks = checks::evaluate(plan, boot, &outcome, probe);
    BootResult {
        label: boot.label.clone(),
        subject: boot.subject,
        slot: boot.slot.clone(),
        serial: serial_name(index, &boot.label),
        outcome,
        report: state,
        checks,
    }
}

/// Metrics of every boot, then the disk.
pub fn metrics(boots: &[BootResult], reports: &[Option<&ProbeReport>], disk: &Disk) -> Vec<Metric> {
    let mut metrics: Vec<Metric> = boots
        .iter()
        .zip(reports)
        .flat_map(|(boot, report)| benchmarks::boot(&boot.label, &boot.outcome, *report))
        .collect();
    metrics.extend(benchmarks::disk(disk.bytes, disk.allocated_bytes));
    metrics
}

pub fn decide(plan: &Plan, boots: &[BootResult]) -> Decision {
    let views: Vec<BootChecks<'_>> = boots
        .iter()
        .map(|boot| BootChecks {
            label: &boot.label,
            subject: boot.subject,
            checks: &boot.checks,
        })
        .collect();
    promotion::decide(&plan.checks, &views)
}

/// Hash every file the result names, in order: the plan, then each boot's
/// serial log and received report.
pub fn artifacts(dir: &Path, boots: &[BootResult]) -> Result<Vec<ExperimentArtifact>, String> {
    let mut files = vec![("plan", PLAN_FILE.to_owned())];
    for boot in boots {
        files.push(("serial-log", boot.serial.clone()));
        if let Some(file) = &boot.report.file {
            files.push(("probe-report", file.clone()));
        }
    }
    files
        .into_iter()
        .map(|(kind, name)| {
            let (sha256, bytes) =
                hash_file(&dir.join(&name)).map_err(|e| format!("cannot hash {name}: {e}"))?;
            Ok(ExperimentArtifact {
                kind: kind.to_owned(),
                name,
                sha256,
                bytes,
            })
        })
        .collect()
}

/// SHA-256 and length of a file, streamed.
pub fn hash_file(path: &Path) -> std::io::Result<(String, u64)> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut bytes = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes += read as u64;
    }
    let digest = hasher.finalize();
    Ok((digest.iter().map(|b| format!("{b:02x}")).collect(), bytes))
}

/// Pretty JSON with sorted keys and a final newline; its SHA-256 is the
/// run's `evidence_sha256`.
pub fn canonical(result: &LabResult) -> Vec<u8> {
    let value = serde_json::to_value(result).expect("a lab result serializes");
    let mut bytes = serde_json::to_vec_pretty(&value).expect("a JSON value serializes");
    bytes.push(b'\n');
    bytes
}

pub fn digest_line(evidence_sha256: &str) -> String {
    format!("{evidence_sha256}  {RESULT_FILE}\n")
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if bytes.len() as u64 > limit {
        return Err(format!("{} is larger than {limit} bytes", path.display()));
    }
    Ok(bytes)
}

/// A result that `lab verify` found intact.
#[derive(Debug)]
pub struct Verified {
    pub plan: Plan,
    pub result: LabResult,
    pub evidence_sha256: String,
}

/// Rehash every file, re-read every report from its serial log, and recompute
/// every check, metric and the decision. Any difference refuses the result.
pub fn verify(dir: &Path) -> Result<Verified, String> {
    let bytes = read_bounded(&dir.join(RESULT_FILE), MAX_RESULT_BYTES)?;
    let result: LabResult =
        serde_json::from_slice(&bytes).map_err(|e| format!("{RESULT_FILE}: {e}"))?;
    if result.schema != RESULT_SCHEMA {
        return Err(format!("{RESULT_FILE}: schema must be {RESULT_SCHEMA}"));
    }
    if canonical(&result) != bytes {
        return Err(format!("{RESULT_FILE} is not in canonical form"));
    }
    let evidence_sha256 = sha256_hex(&bytes);
    let digest = read_bounded(&dir.join(RESULT_DIGEST_FILE), 256)?;
    if digest != digest_line(&evidence_sha256).as_bytes() {
        return Err(format!("{RESULT_DIGEST_FILE} does not match {RESULT_FILE}"));
    }

    let plan_bytes = read_bounded(&dir.join(PLAN_FILE), MAX_PLAN_BYTES)?;
    if sha256_hex(&plan_bytes) != result.plan_sha256 {
        return Err(format!("{PLAN_FILE} is not the plan the result names"));
    }
    let plan = Plan::parse(&plan_bytes)?;
    let expected_header = (
        plan.lane,
        plan.candidate.generation.as_str(),
        plan.baseline.as_deref(),
        plan.machine.accel,
        plan.candidate.disk_sha256.as_str(),
    );
    let header = (
        result.lane,
        result.candidate.as_str(),
        result.baseline.as_deref(),
        result.accel,
        result.disk.sha256.as_str(),
    );
    if header != expected_header {
        return Err(
            "the result's lane, Generations, accelerator or disk differ from its plan".to_owned(),
        );
    }
    if result.boots.len() != plan.boots.len() {
        return Err("the result does not have one entry per planned boot".to_owned());
    }

    let mut received = Vec::new();
    let mut boots = Vec::new();
    for (index, (stored, planned)) in result.boots.iter().zip(&plan.boots).enumerate() {
        let index = index + 1;
        // File names come from the validated plan, never from the result.
        let label = &planned.label;
        let report = vm::report_of(&dir.join(serial_name(index, label)))
            .map_err(|e| format!("cannot read the serial log of {label}: {e}"))?;
        let boot = boot_result(&plan, index, stored.outcome.clone(), &report);
        if &boot != stored {
            return Err(format!(
                "boot {label} does not match its serial log and plan"
            ));
        }
        if let Ok(Some(report)) = report {
            let file = read_bounded(
                &dir.join(report_name(index, label)),
                super::report::MAX_REPORT_BYTES as u64,
            )?;
            if file != report.body {
                return Err(format!(
                    "the report file of {label} differs from its serial log"
                ));
            }
            received.push(Some(report.report));
        } else {
            received.push(None);
        }
        boots.push(boot);
    }
    let reports: Vec<Option<&ProbeReport>> = received.iter().map(Option::as_ref).collect();
    if metrics(&boots, &reports, &result.disk) != result.metrics {
        return Err("the metrics differ from the recomputed ones".to_owned());
    }
    if artifacts(dir, &boots)? != result.artifacts {
        return Err("an artifact's content differs from the result".to_owned());
    }
    if decide(&plan, &boots) != result.decision {
        return Err("the decision differs from the recomputed one".to_owned());
    }
    Ok(Verified {
        plan,
        result,
        evidence_sha256,
    })
}
