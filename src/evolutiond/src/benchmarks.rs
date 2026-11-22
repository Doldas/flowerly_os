//! Measurements of a lab run, recorded with their units and never judged
//! beyond the plan's fixed deadlines (ADR-0024: no fitness scoring).

use serde::{Deserialize, Serialize};

use crate::lab::report::ProbeReport;
use crate::vm::Outcome;

/// Guest boot phases taken from the probe's `systemd-analyze` facts.
pub const PHASES: [&str; 5] = ["firmware", "loader", "kernel", "initrd", "userspace"];
/// Host timings plus guest phases.
pub const METRICS_PER_BOOT: usize = 2 + PHASES.len();
// Every metric of the largest plan fits one Experiment record.
const _: () = assert!(
    crate::lab::plan::MAX_BOOTS * METRICS_PER_BOOT + 2
        <= flowerly_core::storage::experiment::MAX_METRICS
);
/// A phase longer than a day is not a measurement.
const MAX_PHASE_MS: f64 = 86_400_000.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metric {
    /// `[a-z0-9._-]`, ending in its unit (`_s`, `_ms`, `_bytes`).
    pub name: String,
    pub value: f64,
    pub unit: String,
}

fn metric(name: String, value: f64, unit: &str) -> Metric {
    Metric {
        name,
        value: (value * 1000.0).round() / 1000.0,
        unit: unit.to_owned(),
    }
}

/// Host-measured times of one boot and the guest's phases, when known.
pub fn boot(label: &str, outcome: &Outcome, report: Option<&ProbeReport>) -> Vec<Metric> {
    let mut metrics = Vec::new();
    if let Some(seconds) = outcome.to_report {
        metrics.push(metric(format!("{label}.boot_to_report_s"), seconds, "s"));
    }
    if let Some(seconds) = outcome.to_exit {
        metrics.push(metric(format!("{label}.shutdown_s"), seconds, "s"));
    }
    if let Some(report) = report {
        for phase in PHASES {
            match report.facts.boot_phases_ms.get(phase) {
                Some(&ms) if ms.is_finite() && (0.0..=MAX_PHASE_MS).contains(&ms) => {
                    metrics.push(metric(format!("{label}.phase.{phase}_ms"), ms, "ms"));
                }
                _ => {}
            }
        }
    }
    metrics
}

/// The candidate disk as built: apparent and allocated size.
pub fn disk(bytes: u64, allocated: u64) -> Vec<Metric> {
    vec![
        metric("disk.bytes".to_owned(), bytes as f64, "bytes"),
        metric("disk.allocated_bytes".to_owned(), allocated as f64, "bytes"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lab::report::tests::sample;

    fn outcome() -> Outcome {
        Outcome {
            exit_code: Some(0),
            signal: None,
            timed_out: None,
            stopped: None,
            to_report: Some(12.3456),
            to_exit: Some(2.5),
            process: None,
        }
    }

    #[test]
    fn a_boot_records_host_times_and_known_guest_phases() {
        let mut report = sample();
        report.facts.boot_phases_ms.insert("bogus".to_owned(), 5.0);
        report
            .facts
            .boot_phases_ms
            .insert("loader".to_owned(), f64::NAN);
        report
            .facts
            .boot_phases_ms
            .insert("initrd".to_owned(), -1.0);
        let names: Vec<(String, f64, String)> = boot("candidate", &outcome(), Some(&report))
            .into_iter()
            .map(|m| (m.name, m.value, m.unit))
            .collect();
        assert_eq!(
            names,
            vec![
                (
                    "candidate.boot_to_report_s".to_owned(),
                    12.346,
                    "s".to_owned()
                ),
                ("candidate.shutdown_s".to_owned(), 2.5, "s".to_owned()),
                (
                    "candidate.phase.kernel_ms".to_owned(),
                    1200.0,
                    "ms".to_owned()
                ),
                (
                    "candidate.phase.userspace_ms".to_owned(),
                    3400.5,
                    "ms".to_owned()
                ),
            ]
        );
        assert!(names.len() <= METRICS_PER_BOOT);
    }

    #[test]
    fn a_boot_without_a_report_records_only_what_the_host_saw() {
        let mut timed_out = outcome();
        timed_out.to_report = None;
        timed_out.to_exit = None;
        assert!(boot("b", &timed_out, None).is_empty());
        assert_eq!(disk(10, 4).len(), 2);
    }
}
