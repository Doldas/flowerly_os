//! The Evolution Lab (ADR-0024): `evolutiond lab run|verify|record`, in the
//! foreground, as the invoking user. It boots a candidate under QEMU/KVM as a
//! plan describes, decides from what the host saw and the guest reported, and
//! records TESTED or REJECTED through flowerlyd. It never escalates and never
//! activates; the daemon's `evolution.run_experiment` stays unavailable.

pub mod checks;
pub mod plan;
mod record;
pub mod report;
pub mod result;

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use clap::{Args, Subcommand};
use flowerly_core::default_runtime_dir;

use crate::promotion::Verdict;
use crate::vm::{self, Deadlines, Spec};
use plan::{qemu_path, Accel, Plan, MAX_PLAN_BYTES};
use report::ProbeReport;
use result::{Binary, Disk, FirmwareHashes, LabResult, PLAN_FILE, RESULT_SCHEMA};

/// The run passed, or the evidence is intact and passed.
pub const EXIT_PASSED: i32 = 0;
/// The command could not do its work, or the evidence is not intact.
pub const EXIT_ERROR: i32 = 1;
/// The run finished and its verdict is failed.
pub const EXIT_FAILED: i32 = 3;

const QEMU: &str = "qemu-system-x86_64";
const VARS_COPY: &str = "ovmf-vars.fd";
const POLL: Duration = Duration::from_millis(200);

#[derive(Debug, Args)]
pub struct LabArgs {
    #[command(subcommand)]
    command: LabCommand,
}

#[derive(Debug, Subcommand)]
enum LabCommand {
    /// Boot a candidate as its plan describes and write a verifiable result.
    Run {
        /// A `flowerly-lab-plan/v1` document.
        #[arg(long)]
        plan: PathBuf,
        /// A new directory for the result, serial logs and reports.
        #[arg(long)]
        out: PathBuf,
        /// The QEMU binary (default: `qemu-system-x86_64` from PATH).
        #[arg(long, env = "FLOWERLY_LAB_QEMU")]
        qemu: Option<PathBuf>,
        /// Accept a plan that boots with TCG; such a run is never evidence.
        #[arg(long)]
        allow_tcg: bool,
        /// Record the result through flowerlyd when the run ends.
        #[arg(long)]
        record: bool,
        #[arg(long, env = "AGENTIC_RUNTIME_DIR")]
        runtime_dir: Option<PathBuf>,
    },
    /// Rehash a result's files and recompute its checks and decision.
    Verify {
        #[arg(long)]
        dir: PathBuf,
    },
    /// Verify a KVM result, then record it as Experiment evidence and
    /// TESTED or REJECTED.
    Record {
        #[arg(long)]
        dir: PathBuf,
        #[arg(long, env = "AGENTIC_RUNTIME_DIR")]
        runtime_dir: Option<PathBuf>,
    },
}

/// Lowercase hex SHA-256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn exit_for(verdict: Verdict) -> i32 {
    match verdict {
        Verdict::Passed => EXIT_PASSED,
        Verdict::Failed => EXIT_FAILED,
    }
}

/// Run a lab command; the value is the process exit status.
pub async fn main(args: LabArgs) -> i32 {
    match dispatch(args).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("evolutiond lab: {error:#}");
            EXIT_ERROR
        }
    }
}

async fn dispatch(args: LabArgs) -> anyhow::Result<i32> {
    match args.command {
        LabCommand::Run {
            plan,
            out,
            qemu,
            allow_tcg,
            record,
            runtime_dir,
        } => {
            let qemu = resolve_qemu(qemu)?;
            let (out, result) = run(&plan, &out, &qemu, allow_tcg)?;
            summarize(&out, &result);
            if record {
                let runtime_dir = runtime_dir.unwrap_or_else(default_runtime_dir);
                recorded(record::record(&out, &runtime_dir).await?);
            }
            Ok(exit_for(result.decision.verdict))
        }
        LabCommand::Verify { dir } => {
            let verified = result::verify(&dir).map_err(|e| anyhow!("{}: {e}", dir.display()))?;
            println!("intact: evidence {}", verified.evidence_sha256);
            summarize(&dir, &verified.result);
            Ok(exit_for(verified.result.decision.verdict))
        }
        LabCommand::Record { dir, runtime_dir } => {
            let runtime_dir = runtime_dir.unwrap_or_else(default_runtime_dir);
            let verdict = result::verify(&dir)
                .map_err(|e| anyhow!("{}: {e}", dir.display()))?
                .result
                .decision
                .verdict;
            recorded(record::record(&dir, &runtime_dir).await?);
            Ok(exit_for(verdict))
        }
    }
}

fn summarize(dir: &Path, result: &LabResult) {
    let decision = &result.decision;
    println!(
        "{}: {} → {} ({})",
        result.candidate,
        decision.verdict.outcome(),
        decision.recommendation.command(),
        dir.display()
    );
    for reason in &decision.reasons {
        println!("  {reason}");
    }
    if let Some(fallback) = &decision.fallback {
        println!(
            "  fallback after {} to {}: booted={} healthy={}",
            fallback.after, fallback.boot, fallback.booted, fallback.healthy
        );
    }
}

fn recorded(recorded: record::Recorded) {
    println!(
        "recorded: Experiment {}, Generation {}",
        recorded.experiment, recorded.status
    );
}

/// The QEMU binary as an absolute UTF-8 path: the running process is
/// compared with it byte for byte.
fn resolve_qemu(qemu: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let found = match qemu {
        Some(path) => path,
        None => std::env::var_os("PATH")
            .and_then(|paths| {
                std::env::split_paths(&paths)
                    .map(|dir| dir.join(QEMU))
                    .find(|candidate| candidate.is_file())
            })
            .ok_or_else(|| anyhow!("{QEMU} was not found in PATH; pass --qemu"))?,
    };
    let path = found
        .canonicalize()
        .with_context(|| format!("QEMU binary {}", found.display()))?;
    if path.to_str().is_none() {
        bail!("QEMU path {} is not UTF-8", path.display());
    }
    Ok(path)
}

fn read_bounded(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() as u64 > limit {
        bail!("{} is larger than {limit} bytes", path.display());
    }
    Ok(bytes)
}

fn create_new(path: &Path, mode: u32) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
}

fn write_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file =
        create_new(path, 0o600).with_context(|| format!("creating {}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("writing {}", path.display()))
}

/// Boot every planned boot and write the result. The disk must still be the
/// one the plan names: a booted disk has changed and is refused.
fn run(
    plan_path: &Path,
    out: &Path,
    qemu: &Path,
    allow_tcg: bool,
) -> anyhow::Result<(PathBuf, LabResult)> {
    let plan_bytes = read_bounded(plan_path, MAX_PLAN_BYTES)?;
    let plan = Plan::parse(&plan_bytes).map_err(|e| anyhow!("{}: {e}", plan_path.display()))?;
    if plan.machine.accel == Accel::Tcg && !allow_tcg {
        bail!("the plan boots with TCG; pass --allow-tcg to run it (never evidence)");
    }
    let firmware_code = qemu_path(&plan.firmware.code, "firmware code").map_err(|e| anyhow!(e))?;
    let disk_path = qemu_path(&plan.candidate.disk, "candidate disk").map_err(|e| anyhow!(e))?;
    let (disk_sha256, bytes) =
        result::hash_file(&plan.candidate.disk).with_context(|| format!("hashing {disk_path}"))?;
    if disk_sha256 != plan.candidate.disk_sha256 {
        bail!("{disk_path} is not the disk the plan names (already booted, or another build)");
    }
    let allocated_bytes = fs::metadata(&plan.candidate.disk)?.blocks() * 512;
    let (code_sha256, _) = result::hash_file(&plan.firmware.code)
        .with_context(|| format!("hashing {firmware_code}"))?;
    let (qemu_sha256, _) =
        result::hash_file(qemu).with_context(|| format!("hashing {}", qemu.display()))?;

    DirBuilder::new()
        .mode(0o700)
        .create(out)
        .with_context(|| format!("creating the new output directory {}", out.display()))?;
    let out = out.canonicalize()?;
    write_new(&out.join(PLAN_FILE), &plan_bytes)?;
    let vars_copy = out.join(VARS_COPY);
    let firmware_vars = qemu_path(&vars_copy, "output directory").map_err(|e| anyhow!(e))?;
    let vars_template = read_bounded(&plan.firmware.vars, 64 * 1024 * 1024)?;
    write_new(&vars_copy, &vars_template)?;

    let spec = Spec {
        qemu: qemu.to_owned(),
        accel: plan.machine.accel,
        smp: plan.machine.smp,
        memory_mib: plan.machine.memory_mib,
        firmware_code,
        firmware_vars,
        disk: disk_path,
    };
    let deadlines = Deadlines {
        report: Duration::from_secs(plan.limits.boot_seconds),
        shutdown: Duration::from_secs(plan.limits.shutdown_seconds),
        poll: POLL,
    };
    let mut boots = Vec::new();
    let mut received: Vec<Option<ProbeReport>> = Vec::new();
    for (index, boot) in plan.boots.iter().enumerate() {
        let index = index + 1;
        eprintln!(
            "lab: boot {index}/{} `{}` ({:?} on {})",
            plan.boots.len(),
            boot.label,
            boot.subject,
            boot.slot
        );
        let log = out.join(result::serial_name(index, &boot.label));
        let (outcome, _) = vm::boot(&spec, &log, &deadlines)
            .with_context(|| format!("running boot `{}`", boot.label))?;
        // The final log decides, exactly as `lab verify` will read it.
        let report = vm::report_of(&log)?;
        if let Ok(Some(report)) = &report {
            write_new(
                &out.join(result::report_name(index, &boot.label)),
                &report.body,
            )?;
        }
        boots.push(result::boot_result(&plan, index, outcome, &report));
        received.push(report.ok().flatten().map(|r| r.report));
    }

    let disk = Disk {
        sha256: disk_sha256,
        bytes,
        allocated_bytes,
    };
    let reports: Vec<Option<&ProbeReport>> = received.iter().map(Option::as_ref).collect();
    let metrics = result::metrics(&boots, &reports, &disk);
    let artifacts = result::artifacts(&out, &boots).map_err(|e| anyhow!(e))?;
    let decision = result::decide(&plan, &boots);
    let lab_result = LabResult {
        schema: RESULT_SCHEMA.to_owned(),
        evolutiond: env!("CARGO_PKG_VERSION").to_owned(),
        plan_sha256: sha256_hex(&plan_bytes),
        lane: plan.lane,
        candidate: plan.candidate.generation.clone(),
        baseline: plan.baseline.clone(),
        accel: plan.machine.accel,
        qemu: Binary {
            path: qemu.to_string_lossy().into_owned(),
            sha256: qemu_sha256,
        },
        firmware: FirmwareHashes {
            code_sha256,
            vars_sha256: sha256_hex(&vars_template),
        },
        disk,
        boots,
        metrics,
        artifacts,
        decision,
    };
    let bytes = result::canonical(&lab_result);
    write_new(&out.join(result::RESULT_FILE), &bytes)?;
    write_new(
        &out.join(result::RESULT_DIGEST_FILE),
        result::digest_line(&sha256_hex(&bytes)).as_bytes(),
    )?;
    Ok((out, lab_result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lab::plan::tests::sample_value;
    use crate::promotion::Recommendation;
    use crate::vm::tests::fake_qemu;
    use serde_json::Value;

    struct Lab {
        root: tempfile::TempDir,
        plan: PathBuf,
    }

    /// A TCG plan over a small disk and firmware files in a fresh directory.
    fn lab(edit: impl FnOnce(&mut Value)) -> Lab {
        let root = tempfile::tempdir().unwrap();
        let disk = root.path().join("candidate.img");
        fs::write(&disk, b"candidate disk").unwrap();
        fs::write(root.path().join("code.fd"), b"code").unwrap();
        fs::write(root.path().join("vars.fd"), b"vars").unwrap();
        let mut value = sample_value();
        value["candidate"]["disk"] = Value::from(disk.to_str().unwrap());
        value["candidate"]["disk_sha256"] = Value::from(sha256_hex(b"candidate disk"));
        value["firmware"]["code"] = Value::from(root.path().join("code.fd").to_str().unwrap());
        value["firmware"]["vars"] = Value::from(root.path().join("vars.fd").to_str().unwrap());
        value["machine"]["accel"] = Value::from("tcg");
        value["limits"] = serde_json::json!({ "boot_seconds": 1, "shutdown_seconds": 2 });
        edit(&mut value);
        let plan = root.path().join("plan.json");
        fs::write(&plan, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        Lab { root, plan }
    }

    impl Lab {
        /// A fake QEMU running `body` after the console lines in `console`.
        fn qemu(&self, console: &str, body: &str) -> PathBuf {
            let bin = self.root.path().join("bin");
            fs::create_dir_all(&bin).unwrap();
            fs::write(bin.join("console.txt"), console).unwrap();
            let console = bin.join("console.txt");
            fake_qemu(&bin, &format!("cat {}\n{body}", console.display()))
        }

        fn run(&self, qemu: &Path) -> anyhow::Result<(PathBuf, LabResult)> {
            run(&self.plan, &self.root.path().join("out"), qemu, true)
        }
    }

    fn good_console() -> String {
        report::tests::console(&report::tests::sample())
    }

    #[test]
    fn a_good_candidate_is_tested_and_its_result_verifies() {
        let lab = lab(|_| {});
        let qemu = lab.qemu(&good_console(), "exit 0");
        let (out, result) = lab.run(&qemu).unwrap();
        assert_eq!(
            result.decision.verdict,
            Verdict::Passed,
            "{:?}",
            result.decision
        );
        assert_eq!(result.decision.recommendation, Recommendation::Test);
        assert_eq!(
            result.boots[0].report.status,
            result::ReportStatus::Received
        );
        let kinds: Vec<&str> = result.artifacts.iter().map(|a| a.kind.as_str()).collect();
        assert_eq!(kinds, ["plan", "serial-log", "probe-report"]);
        assert!(result
            .metrics
            .iter()
            .any(|m| m.name == "candidate.boot_to_report_s"));
        assert_eq!(
            fs::metadata(out.join(VARS_COPY)).unwrap().mode() & 0o777,
            0o600
        );

        let verified = result::verify(&out).unwrap();
        assert_eq!(verified.result, result);
        let digest = fs::read_to_string(out.join(result::RESULT_DIGEST_FILE)).unwrap();
        assert!(digest.starts_with(&verified.evidence_sha256));

        let details = record::details(&verified).unwrap();
        assert_eq!(details.test_plan.len(), 8);
        assert_eq!(details.artifacts, result.artifacts);
        assert!(record::reason(&verified).contains(&verified.evidence_sha256));
    }

    #[test]
    fn any_edit_of_the_evidence_is_refused_by_verify() {
        let lab = lab(|_| {});
        let qemu = lab.qemu(&good_console(), "exit 0");
        let (out, _) = lab.run(&qemu).unwrap();
        let pristine = lab.root.path().join("pristine");
        fs::create_dir(&pristine).unwrap();
        for entry in fs::read_dir(&out).unwrap() {
            let entry = entry.unwrap();
            fs::copy(entry.path(), pristine.join(entry.file_name())).unwrap();
        }
        let restore = |name: &str| {
            fs::write(out.join(name), fs::read(pristine.join(name)).unwrap()).unwrap();
        };
        let append = |name: &str, extra: &str| {
            let mut bytes = fs::read(pristine.join(name)).unwrap();
            bytes.extend_from_slice(extra.as_bytes());
            fs::write(out.join(name), bytes).unwrap();
        };

        // A changed verdict, even with a matching digest file.
        let text = fs::read_to_string(pristine.join("result.json")).unwrap();
        let forged = text.replace("\"passed\"", "\"failed\"");
        fs::write(out.join("result.json"), &forged).unwrap();
        fs::write(
            out.join("result.sha256"),
            result::digest_line(&sha256_hex(forged.as_bytes())),
        )
        .unwrap();
        assert!(result::verify(&out).is_err());
        restore("result.json");
        assert!(result::verify(&out).unwrap_err().contains("result.sha256"));
        restore("result.sha256");
        result::verify(&out).unwrap();

        for (name, extra) in [
            ("1-candidate.serial.log", "late console noise\n"),
            ("1-candidate.report.json", " "),
            ("plan.json", "\n"),
        ] {
            append(name, extra);
            assert!(result::verify(&out).is_err(), "{name}");
            restore(name);
        }
        fs::remove_file(out.join("1-candidate.report.json")).unwrap();
        assert!(result::verify(&out).is_err());
        restore("1-candidate.report.json");
        result::verify(&out).unwrap();
    }

    #[test]
    fn a_crash_a_silent_guest_and_a_corrupt_report_are_rejected() {
        for (body, console, expect) in [
            ("exit 3", "firmware\n".to_owned(), "code Some(3)"),
            (
                "exit 0",
                "firmware\n".to_owned(),
                "the guest sent no report",
            ),
            (
                "exit 0",
                good_console().replace("part=2/", "part=9/"),
                "no usable report",
            ),
        ] {
            let lab = lab(|_| {});
            let qemu = lab.qemu(&console, body);
            let (out, result) = lab.run(&qemu).unwrap();
            assert_eq!(result.decision.verdict, Verdict::Failed, "{body}");
            assert_eq!(result.decision.recommendation, Recommendation::Reject);
            assert!(
                result.decision.reasons.iter().any(|r| r.contains(expect)),
                "{body}: {:?}",
                result.decision.reasons
            );
            result::verify(&out).unwrap();
        }
    }

    #[test]
    fn a_hang_is_stopped_at_the_report_deadline() {
        let lab = lab(|_| {});
        let qemu = lab.qemu("firmware\n", "exec sleep 30");
        let started = std::time::Instant::now();
        let (_, result) = lab.run(&qemu).unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(result.boots[0].outcome.timed_out, Some(vm::Phase::Report));
        assert!(!result.boots[0].checks[&plan::Check::Shutdown].passed);
    }

    #[test]
    fn runs_that_cannot_be_evidence_are_refused_before_booting() {
        let lab = lab(|_| {});
        let qemu = lab.qemu(&good_console(), "exit 0");
        let err = run(&lab.plan, &lab.root.path().join("out"), &qemu, false).unwrap_err();
        assert!(err.to_string().contains("--allow-tcg"));
        assert!(!lab.root.path().join("out").exists());

        fs::write(lab.root.path().join("candidate.img"), b"booted disk").unwrap();
        let err = lab.run(&qemu).unwrap_err();
        assert!(err.to_string().contains("not the disk"), "{err}");
        assert!(!lab.root.path().join("out").exists());

        fs::write(lab.root.path().join("candidate.img"), b"candidate disk").unwrap();
        fs::create_dir(lab.root.path().join("out")).unwrap();
        assert!(lab.run(&qemu).is_err());
    }

    #[test]
    fn a_tcg_result_is_not_recorded() {
        let lab = lab(|_| {});
        let qemu = lab.qemu(&good_console(), "exit 0");
        let (out, _) = lab.run(&qemu).unwrap();
        let runtime = lab.root.path().join("runtime");
        let err = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(record::record(&out, &runtime))
            .unwrap_err();
        assert!(err.to_string().contains("only a KVM run"), "{err}");
    }
}
