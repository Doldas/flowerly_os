//! One QEMU process per boot (ADR-0024 D1): OVMF firmware with a private
//! copy of its variables, the candidate disk, no network, no monitor, no host
//! sharing, the serial console captured to a file, a hard deadline and a kill
//! of the whole process group when it passes.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::lab::plan::Accel;
use crate::lab::report::{self, Received};
use crate::lab::sha256_hex;

/// A guest that floods its console is stopped rather than filling the disk.
pub const MAX_SERIAL_BYTES: u64 = 64 * 1024 * 1024;
/// Options a firmware boot must never carry: direct kernel boot or a network.
const FORBIDDEN: [&str; 4] = ["-kernel", "-initrd", "-nic", "-netdev"];

/// What one boot runs. Paths are absolute and comma-free (checked by the plan).
#[derive(Debug, Clone)]
pub struct Spec {
    pub qemu: PathBuf,
    pub accel: Accel,
    pub smp: u32,
    pub memory_mib: u32,
    pub firmware_code: String,
    pub firmware_vars: String,
    pub disk: String,
}

/// The argv of one boot; `argv[0]` is the QEMU binary.
pub fn argv(spec: &Spec) -> Vec<String> {
    let (machine, cpu) = match spec.accel {
        Accel::Kvm => ("q35,accel=kvm", "host"),
        Accel::Tcg => ("q35,accel=tcg", "max"),
    };
    let mut argv = vec![spec.qemu.to_string_lossy().into_owned()];
    argv.extend(
        [
            "-machine",
            machine,
            "-cpu",
            cpu,
            "-smp",
            &spec.smp.to_string(),
            "-m",
            &spec.memory_mib.to_string(),
            "-nodefaults",
            "-display",
            "none",
            "-serial",
            "stdio",
            "-monitor",
            "none",
            "-net",
            "none",
            "-no-reboot",
            "-drive",
            &format!(
                "if=pflash,format=raw,readonly=on,file={}",
                spec.firmware_code
            ),
            "-drive",
            &format!("if=pflash,format=raw,file={}", spec.firmware_vars),
            "-drive",
            &format!("if=virtio,format=raw,cache=none,file={}", spec.disk),
        ]
        .map(str::to_owned),
    );
    argv
}

#[derive(Debug, Clone, Copy)]
pub struct Deadlines {
    /// From spawn to a complete report.
    pub report: Duration,
    /// From the report to QEMU's own exit.
    pub shutdown: Duration,
    pub poll: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Report,
    Shutdown,
}

/// Host evidence that the running process is the approved KVM boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessEvidence {
    pub kvm_fd: bool,
    pub cmdline_sha256: String,
}

/// What the host observed about one boot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// The deadline that stopped the boot, if one did.
    pub timed_out: Option<Phase>,
    /// Why the host stopped the boot early (corrupt report, flood, refused
    /// process); `None` when it ran to its own end or a deadline.
    pub stopped: Option<String>,
    /// Seconds from spawn to a complete report.
    pub to_report: Option<f64>,
    /// Seconds from the report to QEMU's own exit.
    pub to_exit: Option<f64>,
    /// `None` when not checked (TCG) or not obtained (see `stopped`).
    pub process: Option<ProcessEvidence>,
}

impl Outcome {
    /// QEMU ended on its own with status 0 after the report.
    pub fn clean_exit(&self) -> bool {
        self.exit_code == Some(0)
            && self.timed_out.is_none()
            && self.stopped.is_none()
            && self.to_exit.is_some()
    }
}

fn seconds(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 1000.0).round() / 1000.0
}

/// Read the log as text; the length is bounded by [`MAX_SERIAL_BYTES`].
fn read_log(path: &Path) -> io::Result<(u64, String)> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_SERIAL_BYTES + 1)
        .read_to_end(&mut bytes)?;
    Ok((
        bytes.len() as u64,
        String::from_utf8_lossy(&bytes).into_owned(),
    ))
}

/// The report state of a finished boot, as `lab verify` recomputes it.
pub fn report_of(log: &Path) -> io::Result<Result<Option<Received>, String>> {
    let (length, text) = read_log(log)?;
    if length > MAX_SERIAL_BYTES {
        return Ok(Err("serial log is too large".to_owned()));
    }
    Ok(report::parse(&text))
}

/// Kill every process of the boot's group; a group already gone is fine.
fn kill_group(pid: u32) {
    let Ok(pgid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // SAFETY: killpg only sends a signal; `pgid` is the group this process
    // created for the child with `process_group(0)`, so it names no other
    // process group. Failure (the group is gone) is deliberately ignored.
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

/// Run one boot to its end, a deadline, or the first refusal.
///
/// The serial log is created new (never appended to) at `log`.
pub fn boot(
    spec: &Spec,
    log: &Path,
    deadlines: &Deadlines,
) -> io::Result<(Outcome, Option<Received>)> {
    let argv = argv(spec);
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(log)?;
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .process_group(0)
        .spawn()?;
    let pid = child.id();
    let started = Instant::now();
    let mut outcome = Outcome {
        exit_code: None,
        signal: None,
        timed_out: None,
        stopped: None,
        to_report: None,
        to_exit: None,
        process: None,
    };
    let mut received = None;
    let mut report_at = None;
    let status = loop {
        let (length, text) = read_log(log)?;
        if length > MAX_SERIAL_BYTES {
            outcome.stopped = Some("serial log is too large".to_owned());
            break None;
        }
        if spec.accel == Accel::Kvm && outcome.process.is_none() && length > 0 {
            // Once the firmware writes, the exec has completed (Gate D run 4).
            match verify_process(pid, &argv) {
                Ok(evidence) => outcome.process = Some(evidence),
                Err(reason) => {
                    outcome.stopped = Some(reason);
                    break None;
                }
            }
        }
        if report_at.is_none() {
            match report::parse(&text) {
                Ok(Some(report)) => {
                    received = Some(report);
                    report_at = Some(Instant::now());
                    outcome.to_report = Some(seconds(started.elapsed()));
                }
                Ok(None) => {}
                Err(reason) => {
                    outcome.stopped = Some(reason);
                    break None;
                }
            }
        }
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        match report_at {
            None if started.elapsed() > deadlines.report => {
                outcome.timed_out = Some(Phase::Report);
                break None;
            }
            Some(at) if at.elapsed() > deadlines.shutdown => {
                outcome.timed_out = Some(Phase::Shutdown);
                break None;
            }
            _ => std::thread::sleep(deadlines.poll),
        }
    };
    // Also on a clean exit: nothing QEMU started may outlive the boot.
    kill_group(pid);
    let status = match status {
        Some(status) => status,
        None => child.wait()?,
    };
    let exited_at = Instant::now();
    outcome.exit_code = status.code();
    outcome.signal = status.signal();
    if report_at.is_none() && outcome.stopped.is_none() {
        // The guest may write its report and exit between two polls.
        match report_of(log)? {
            Ok(Some(report)) => {
                received = Some(report);
                report_at = Some(exited_at);
                outcome.to_report = Some(seconds(exited_at - started));
            }
            Ok(None) => {}
            Err(reason) => outcome.stopped = Some(reason),
        }
    }
    if let (Some(at), None, None) = (report_at, outcome.timed_out, &outcome.stopped) {
        outcome.to_exit = Some(seconds(exited_at.saturating_duration_since(at)));
    }
    Ok((outcome, received))
}

/// Read the running process, not the requested configuration (Gate D).
fn verify_process(pid: u32, argv: &[String]) -> Result<ProcessEvidence, String> {
    let proc = PathBuf::from("/proc").join(pid.to_string());
    let cmdline = std::fs::read(proc.join("cmdline"))
        .map_err(|e| format!("cannot read the QEMU command line: {e}"))?;
    let fds = std::fs::read_dir(proc.join("fd"))
        .map_err(|e| format!("cannot list the QEMU descriptors: {e}"))?
        .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
        .collect::<Vec<_>>();
    process_evidence(&cmdline, &fds, argv)
}

/// The checks of [`verify_process`] on what it read.
pub fn process_evidence(
    cmdline: &[u8],
    fds: &[PathBuf],
    argv: &[String],
) -> Result<ProcessEvidence, String> {
    let actual: Vec<&[u8]> = cmdline
        .split(|&b| b == 0)
        .filter(|a| !a.is_empty())
        .collect();
    if actual.len() != argv.len() || actual.iter().zip(argv).any(|(a, b)| *a != b.as_bytes()) {
        return Err("running QEMU command differs from the approved KVM firmware boot".to_owned());
    }
    if argv.iter().any(|a| FORBIDDEN.contains(&a.as_str())) {
        return Err("direct-kernel or networked QEMU is not a firmware boot".to_owned());
    }
    if !fds.iter().any(|fd| fd == Path::new("/dev/kvm")) {
        return Err("no open /dev/kvm descriptor in the running QEMU".to_owned());
    }
    Ok(ProcessEvidence {
        kvm_fd: true,
        cmdline_sha256: sha256_hex(cmdline),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn spec(qemu: PathBuf, accel: Accel) -> Spec {
        Spec {
            qemu,
            accel,
            smp: 2,
            memory_mib: 2048,
            firmware_code: "/fw/CODE.fd".to_owned(),
            firmware_vars: "/run/lab/VARS.fd".to_owned(),
            disk: "/lab/disk.img".to_owned(),
        }
    }

    #[test]
    fn the_kvm_argv_is_the_approved_firmware_boot() {
        let argv = argv(&spec(
            PathBuf::from("/usr/bin/qemu-system-x86_64"),
            Accel::Kvm,
        ));
        let expected = "/usr/bin/qemu-system-x86_64 -machine q35,accel=kvm -cpu host -smp 2 \
            -m 2048 -nodefaults -display none -serial stdio -monitor none -net none -no-reboot \
            -drive if=pflash,format=raw,readonly=on,file=/fw/CODE.fd \
            -drive if=pflash,format=raw,file=/run/lab/VARS.fd \
            -drive if=virtio,format=raw,cache=none,file=/lab/disk.img";
        assert_eq!(
            argv.join(" "),
            expected.split_whitespace().collect::<Vec<_>>().join(" ")
        );
        assert!(!argv.iter().any(|a| FORBIDDEN.contains(&a.as_str())));
    }

    #[test]
    fn tcg_changes_only_the_accelerator_and_cpu() {
        let kvm = argv(&spec(PathBuf::from("/q"), Accel::Kvm));
        let tcg = argv(&spec(PathBuf::from("/q"), Accel::Tcg));
        let differ: Vec<(&String, &String)> =
            kvm.iter().zip(&tcg).filter(|(a, b)| a != b).collect();
        assert_eq!(
            differ,
            vec![
                (&"q35,accel=kvm".to_owned(), &"q35,accel=tcg".to_owned()),
                (&"host".to_owned(), &"max".to_owned())
            ]
        );
    }

    #[test]
    fn process_evidence_needs_the_exact_argv_and_a_kvm_descriptor() {
        let argv = argv(&spec(PathBuf::from("/q"), Accel::Kvm));
        let mut cmdline = argv.join("\0").into_bytes();
        cmdline.push(0);
        let kvm = vec![PathBuf::from("/dev/null"), PathBuf::from("/dev/kvm")];
        let evidence = process_evidence(&cmdline, &kvm, &argv).unwrap();
        assert!(evidence.kvm_fd);
        assert_eq!(evidence.cmdline_sha256, sha256_hex(&cmdline));

        assert!(process_evidence(&cmdline, &kvm[..1], &argv)
            .unwrap_err()
            .contains("/dev/kvm"));
        let mut extra = argv.clone();
        extra.push("-netdev".to_owned());
        let mut extra_cmdline = extra.join("\0").into_bytes();
        extra_cmdline.push(0);
        assert!(process_evidence(&extra_cmdline, &kvm, &argv)
            .unwrap_err()
            .contains("differs"));
        assert!(process_evidence(&extra_cmdline, &kvm, &extra)
            .unwrap_err()
            .contains("firmware"));
        assert!(process_evidence(b"", &kvm, &argv).is_err());
    }

    /// A stand-in for QEMU: a shell script with the lab's argv.
    ///
    /// `cp` writes the executable, never this process: a test thread that
    /// forks while this one holds a write descriptor would make the exec fail
    /// with ETXTBSY.
    pub(crate) fn fake_qemu(dir: &Path, body: &str) -> PathBuf {
        let source = dir.join("fake-qemu.sh");
        let path = dir.join("fake-qemu");
        std::fs::write(&source, format!("#!/bin/sh\n{body}\n")).unwrap();
        let copied = Command::new("cp").arg(&source).arg(&path).status().unwrap();
        assert!(copied.success(), "cp {}: {copied}", source.display());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn deadlines(report: u64) -> Deadlines {
        Deadlines {
            report: Duration::from_millis(report),
            shutdown: Duration::from_millis(1500),
            poll: Duration::from_millis(20),
        }
    }

    fn report_script(dir: &Path, after: &str) -> PathBuf {
        let body = serde_json::to_vec(&crate::lab::report::tests::sample()).unwrap();
        let lines = report::lines(&body, 64).join("\n");
        std::fs::write(dir.join("report.txt"), lines + "\n").unwrap();
        fake_qemu(
            dir,
            &format!(
                "echo firmware\ncat {}\n{after}",
                dir.join("report.txt").display()
            ),
        )
    }

    #[test]
    fn a_guest_that_reports_and_powers_off_is_a_clean_boot() {
        let dir = tempfile::tempdir().unwrap();
        let qemu = report_script(dir.path(), "exit 0");
        let (outcome, received) = boot(
            &spec(qemu, Accel::Tcg),
            &dir.path().join("boot.log"),
            &deadlines(5000),
        )
        .unwrap();
        assert!(outcome.clean_exit(), "{outcome:?}");
        assert!(received.is_some());
        assert!(outcome.to_report.is_some() && outcome.process.is_none());
    }

    #[test]
    fn a_crash_after_the_report_is_not_a_clean_exit() {
        let dir = tempfile::tempdir().unwrap();
        let qemu = report_script(dir.path(), "exit 3");
        let (outcome, received) = boot(
            &spec(qemu, Accel::Tcg),
            &dir.path().join("boot.log"),
            &deadlines(5000),
        )
        .unwrap();
        assert_eq!(outcome.exit_code, Some(3));
        assert!(received.is_some() && !outcome.clean_exit());
    }

    #[test]
    fn a_guest_that_exits_without_a_report_has_none() {
        let dir = tempfile::tempdir().unwrap();
        let qemu = fake_qemu(dir.path(), "echo booting; exit 0");
        let (outcome, received) = boot(
            &spec(qemu, Accel::Tcg),
            &dir.path().join("boot.log"),
            &deadlines(5000),
        )
        .unwrap();
        assert!(received.is_none());
        assert_eq!(
            (outcome.exit_code, outcome.to_report, outcome.to_exit),
            (Some(0), None, None)
        );
    }

    #[test]
    fn the_report_deadline_kills_the_whole_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("child.pid");
        let qemu = fake_qemu(
            dir.path(),
            &format!("sleep 60 &\necho $! > {}\nwait", pidfile.display()),
        );
        let started = Instant::now();
        let (outcome, received) = boot(
            &spec(qemu, Accel::Tcg),
            &dir.path().join("boot.log"),
            &deadlines(300),
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(outcome.timed_out, Some(Phase::Report));
        assert_eq!(outcome.signal, Some(libc::SIGKILL));
        assert!(received.is_none());
        let child: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // The background `sleep` was in the group and is gone (or a zombie
        // already reaped by init); signal 0 probes existence.
        std::thread::sleep(Duration::from_millis(100));
        // SAFETY: signal 0 sends nothing; it only reports whether `child` exists.
        let alive = unsafe { libc::kill(child, 0) } == 0;
        let zombie = std::fs::read_to_string(format!("/proc/{child}/stat"))
            .map(|s| s.contains(") Z "))
            .unwrap_or(true);
        assert!(!alive || zombie, "the guest's child outlived the boot");
    }

    #[test]
    fn a_guest_that_never_powers_off_hits_the_shutdown_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let qemu = report_script(dir.path(), "exec sleep 60");
        let (outcome, received) = boot(
            &spec(qemu, Accel::Tcg),
            &dir.path().join("boot.log"),
            &deadlines(5000),
        )
        .unwrap();
        assert!(received.is_some());
        assert_eq!(outcome.timed_out, Some(Phase::Shutdown));
        assert!(!outcome.clean_exit() && outcome.to_exit.is_none());
    }

    #[test]
    fn a_corrupt_report_stops_the_boot_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let line = format!("FLOWERLY_LAB_REPORT_DONE parts=1 sha256={}", "0".repeat(64));
        let qemu = fake_qemu(
            dir.path(),
            &format!("echo 'FLOWERLY_LAB_REPORT part=1/1 eyJ9'\necho '{line}'\nexec sleep 60"),
        );
        let started = Instant::now();
        let (outcome, received) = boot(
            &spec(qemu, Accel::Tcg),
            &dir.path().join("boot.log"),
            &deadlines(5000),
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(received.is_none());
        assert!(outcome.stopped.as_deref().unwrap().contains("digest"));
    }

    #[test]
    fn an_existing_serial_log_is_never_reused() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("boot.log");
        std::fs::write(&log, "old").unwrap();
        let qemu = fake_qemu(dir.path(), "exit 0");
        assert!(boot(&spec(qemu, Accel::Tcg), &log, &deadlines(1000)).is_err());
    }
}
