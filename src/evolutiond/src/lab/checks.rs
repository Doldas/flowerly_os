//! The host's checks of one boot: what the guest reported compared with what
//! was built, and what the host saw of the QEMU process (ADR-0024 D4).

use std::collections::BTreeMap;

use flowerly_core::storage::experiment::ExperimentLane;
use serde::{Deserialize, Serialize};

use super::plan::{Accel, Boot, Check, Plan, SlotExpectation};
use super::report::ProbeReport;
use crate::vm::{Outcome, Phase};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckResult {
    pub passed: bool,
    pub detail: String,
}

fn pass(detail: impl Into<String>) -> CheckResult {
    CheckResult {
        passed: true,
        detail: detail.into(),
    }
}

fn fail(detail: impl Into<String>) -> CheckResult {
    CheckResult {
        passed: false,
        detail: detail.into(),
    }
}

/// `expected` and `actual` agree; `None` reported is a failure.
fn same(what: &str, expected: &str, actual: Option<&str>) -> Result<(), String> {
    match actual {
        Some(actual) if actual == expected => Ok(()),
        Some(actual) => Err(format!("{what} is `{actual}`, expected `{expected}`")),
        None => Err(format!("{what} was not reported")),
    }
}

fn all(results: impl IntoIterator<Item = Result<(), String>>, ok: &str) -> CheckResult {
    let failures: Vec<String> = results.into_iter().filter_map(Result::err).collect();
    if failures.is_empty() {
        pass(ok)
    } else {
        fail(failures.join("; "))
    }
}

/// Every check the plan names, for one boot. A check the host cannot compute
/// fails; nothing here is taken from the guest's own opinion.
pub fn evaluate(
    plan: &Plan,
    boot: &Boot,
    outcome: &Outcome,
    report: Option<&ProbeReport>,
) -> BTreeMap<Check, CheckResult> {
    let expected = &plan.expect[&boot.slot];
    let unverified = plan.machine.accel == Accel::Kvm && outcome.process.is_none();
    plan.checks
        .iter()
        .map(|&check| {
            let result = if unverified {
                fail("the QEMU process was not verified as the approved KVM boot")
            } else if check == Check::Shutdown {
                // The host's own view; it cannot pass without a report.
                shutdown(outcome)
            } else {
                match report {
                    None => fail(match &outcome.stopped {
                        Some(reason) => format!("no usable report: {reason}"),
                        None => "the guest sent no report".to_owned(),
                    }),
                    Some(report) => one(plan, check, boot, expected, outcome, report),
                }
            };
            (check, result)
        })
        .collect()
}

fn one(
    plan: &Plan,
    check: Check,
    boot: &Boot,
    expected: &SlotExpectation,
    outcome: &Outcome,
    report: &ProbeReport,
) -> CheckResult {
    let facts = &report.facts;
    let seen = &report.observations;
    match check {
        Check::Identity => all(
            [
                same(
                    "identity",
                    &expected.identity_sha256,
                    facts.identity_sha256.as_deref(),
                ),
                same(
                    "generation",
                    &expected.generation,
                    facts.generation.as_deref(),
                ),
            ],
            "identity and Generation are the ones built",
        ),
        Check::Root => all(
            [
                same("slot", &boot.slot, facts.slot.as_deref()),
                same(
                    "root PARTUUID",
                    &expected.partuuid,
                    facts.root_partuuid.as_deref(),
                ),
            ],
            "booted the expected root",
        ),
        Check::Kernel => all(
            [
                same(
                    "kernel release",
                    &expected.kernel_release,
                    facts.kernel_release.as_deref(),
                ),
                same(
                    "vmlinuz",
                    &expected.vmlinuz_sha256,
                    facts.vmlinuz_sha256.as_deref(),
                ),
            ],
            "kernel as built",
        ),
        Check::Packages => match (plan.lane, &expected.packages_sha256) {
            (ExperimentLane::Reproducibility, Some(frozen)) => all(
                [same(
                    "package set",
                    frozen,
                    facts.packages_sha256.as_deref(),
                )],
                "the frozen package set",
            ),
            (ExperimentLane::Reproducibility, None) => fail("the plan claims no package set"),
            (ExperimentLane::Upstream, _) => match &facts.packages_sha256 {
                Some(resolved) => pass(format!("recorded, not claimed: {resolved}")),
                None => fail("package set was not reported"),
            },
        },
        Check::Binaries => {
            if expected.binaries.is_empty() {
                fail("the plan expects no binaries")
            } else {
                all(
                    expected.binaries.iter().map(|(name, hash)| {
                        same(name, hash, facts.binaries.get(name).map(String::as_str))
                    }),
                    "Flowerly binaries as built",
                )
            }
        }
        Check::Health => all(
            [
                same("health gate", "passed", seen.health_gate.as_deref()),
                flag("flowerlyd active", seen.flowerlyd_active),
                flag("status", seen.status_ok),
                if facts.failed_units.is_empty() {
                    Ok(())
                } else {
                    Err(format!("failed units: {}", facts.failed_units.join(", ")))
                },
            ],
            "healthy",
        ),
        Check::Integration => all(
            [
                flag("Admin round trip", seen.admin_roundtrip),
                same(
                    "resolved Generation",
                    &expected.generation,
                    seen.generation_resolved.as_deref(),
                ),
                flag("evolutiond self-test", seen.evolutiond_self_test),
            ],
            "integrated",
        ),
        Check::Shutdown => shutdown(outcome),
    }
}

fn shutdown(outcome: &Outcome) -> CheckResult {
    if outcome.clean_exit() {
        pass("powered off after its report")
    } else if let Some(reason) = &outcome.stopped {
        fail(format!("stopped: {reason}"))
    } else if let Some(phase) = outcome.timed_out {
        fail(match phase {
            Phase::Report => "stopped at the report deadline",
            Phase::Shutdown => "stopped at the shutdown deadline",
        })
    } else if outcome.to_report.is_none() {
        fail(format!(
            "QEMU ended without a report (code {:?}, signal {:?})",
            outcome.exit_code, outcome.signal
        ))
    } else {
        fail(format!(
            "QEMU ended with code {:?}, signal {:?}",
            outcome.exit_code, outcome.signal
        ))
    }
}

fn flag(what: &str, value: Option<bool>) -> Result<(), String> {
    match value {
        Some(true) => Ok(()),
        Some(false) => Err(format!("{what} failed")),
        None => Err(format!("{what} was not reported")),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::lab::plan::tests::sample as plan;
    use crate::lab::report::tests::sample as report;
    use crate::vm::ProcessEvidence;

    pub(crate) fn clean() -> Outcome {
        Outcome {
            exit_code: Some(0),
            signal: None,
            timed_out: None,
            stopped: None,
            to_report: Some(10.0),
            to_exit: Some(1.0),
            process: Some(ProcessEvidence {
                kvm_fd: true,
                cmdline_sha256: "9".repeat(64),
            }),
        }
    }

    fn failed(results: &BTreeMap<Check, CheckResult>) -> Vec<Check> {
        results
            .iter()
            .filter(|(_, r)| !r.passed)
            .map(|(&c, _)| c)
            .collect()
    }

    #[test]
    fn a_good_candidate_passes_every_check() {
        let plan = plan();
        let results = evaluate(&plan, &plan.boots[0], &clean(), Some(&report()));
        assert_eq!(results.len(), 8);
        assert_eq!(failed(&results), vec![]);
    }

    #[test]
    fn each_fact_that_differs_fails_its_own_check() {
        let plan = plan();
        type Mutation = fn(&mut ProbeReport);
        let cases: &[(Check, Mutation)] = &[
            (Check::Identity, |r| {
                r.facts.identity_sha256 = Some("0".repeat(64))
            }),
            (Check::Identity, |r| r.facts.generation = None),
            (Check::Root, |r| r.facts.slot = Some("root-a".to_owned())),
            (Check::Root, |r| r.facts.root_partuuid = None),
            (Check::Kernel, |r| {
                r.facts.kernel_release = Some("6.1".to_owned())
            }),
            (Check::Kernel, |r| r.facts.vmlinuz_sha256 = None),
            (Check::Packages, |r| {
                r.facts.packages_sha256 = Some("0".repeat(64))
            }),
            (Check::Binaries, |r| r.facts.binaries.clear()),
            (Check::Health, |r| {
                r.observations.health_gate = Some("failed".to_owned())
            }),
            (Check::Health, |r| {
                r.observations.flowerlyd_active = Some(false)
            }),
            (Check::Health, |r| r.observations.status_ok = None),
            (Check::Health, |r| {
                r.facts.failed_units = vec!["flowerlyd.service".to_owned()]
            }),
            (Check::Integration, |r| {
                r.observations.admin_roundtrip = Some(false)
            }),
            (Check::Integration, |r| {
                r.observations.generation_resolved = Some("gen-baseline".to_owned());
            }),
            (Check::Integration, |r| {
                r.observations.evolutiond_self_test = None
            }),
        ];
        for (check, mutate) in cases {
            let mut report = report();
            mutate(&mut report);
            let results = evaluate(&plan, &plan.boots[0], &clean(), Some(&report));
            assert_eq!(failed(&results), vec![*check], "{check:?} {report:?}");
        }
    }

    #[test]
    fn no_report_or_an_unverified_kvm_process_fails_everything() {
        let plan = plan();
        let mut stopped = clean();
        stopped.stopped = Some("report does not match its digest".to_owned());
        let results = evaluate(&plan, &plan.boots[0], &stopped, None);
        assert_eq!(failed(&results).len(), 8);
        assert!(results[&Check::Health].detail.contains("digest"));

        let mut unverified = clean();
        unverified.process = None;
        let results = evaluate(&plan, &plan.boots[0], &unverified, Some(&report()));
        assert_eq!(failed(&results).len(), 8);
    }

    #[test]
    fn shutdown_needs_a_clean_exit_after_the_report() {
        let plan = plan();
        let mut late = clean();
        late.timed_out = Some(crate::vm::Phase::Shutdown);
        late.to_exit = None;
        late.exit_code = None;
        late.signal = Some(9);
        let results = evaluate(&plan, &plan.boots[0], &late, Some(&report()));
        assert_eq!(failed(&results), vec![Check::Shutdown]);
        let mut crashed = clean();
        crashed.exit_code = Some(1);
        let results = evaluate(&plan, &plan.boots[0], &crashed, Some(&report()));
        assert_eq!(failed(&results), vec![Check::Shutdown]);
    }

    #[test]
    fn the_upstream_lane_records_the_package_set_without_claiming_it() {
        let mut plan = plan();
        plan.lane = ExperimentLane::Upstream;
        let mut report = report();
        report.facts.packages_sha256 = Some("7".repeat(64));
        let results = evaluate(&plan, &plan.boots[0], &clean(), Some(&report));
        assert!(results[&Check::Packages].passed);
        assert!(results[&Check::Packages].detail.contains("not claimed"));
        report.facts.packages_sha256 = None;
        let results = evaluate(&plan, &plan.boots[0], &clean(), Some(&report));
        assert!(!results[&Check::Packages].passed);
    }
}
