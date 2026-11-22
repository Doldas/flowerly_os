//! The lab's promotion policy (ADR-0024 D3): fail closed, recommend TESTED or
//! REJECTED, and never activate. Activation stays an Admin decision made
//! outside the lab.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::lab::checks::CheckResult;
use crate::lab::plan::{Check, Subject};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Passed,
    Failed,
}

impl Verdict {
    /// The `generation.evidence` outcome.
    pub fn outcome(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
        }
    }
}

/// What the lab may ask flowerlyd for. There is no activation variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    Test,
    Reject,
}

impl Recommendation {
    pub fn command(self) -> &'static str {
        match self {
            Self::Test => "generation.test",
            Self::Reject => "generation.reject",
        }
    }
}

/// A baseline boot that followed a failed candidate boot: the loader fell
/// back (ADR-0024 scenario 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fallback {
    /// The failed candidate boot.
    pub after: String,
    /// The baseline boot after it.
    pub boot: String,
    /// The baseline's identity and root were the ones booted.
    pub booted: bool,
    /// Every check of the baseline boot passed.
    pub healthy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub verdict: Verdict,
    pub recommendation: Recommendation,
    /// Every failed check as `{label}: {check} failed: {detail}`.
    pub reasons: Vec<String>,
    pub fallback: Option<Fallback>,
}

/// One boot as the policy sees it.
pub struct BootChecks<'a> {
    pub label: &'a str,
    pub subject: Subject,
    pub checks: &'a BTreeMap<Check, CheckResult>,
}

fn passed(checks: &BTreeMap<Check, CheckResult>, check: Check) -> bool {
    checks.get(&check).is_some_and(|result| result.passed)
}

/// Passed only when every boot has every planned check, and each passed.
pub fn decide(planned: &[Check], boots: &[BootChecks<'_>]) -> Decision {
    let mut reasons = Vec::new();
    for boot in boots {
        for &check in planned {
            match boot.checks.get(&check) {
                Some(result) if result.passed => {}
                Some(result) => reasons.push(format!(
                    "{}: {} failed: {}",
                    boot.label,
                    check.name(),
                    result.detail
                )),
                None => reasons.push(format!("{}: {} was not run", boot.label, check.name())),
            }
        }
    }
    if boots.is_empty() {
        reasons.push("no boot was run".to_owned());
    }
    let fallback = boots.iter().enumerate().find_map(|(index, boot)| {
        let failed = boot.subject == Subject::Candidate
            && planned.iter().any(|&check| !passed(boot.checks, check));
        if !failed {
            return None;
        }
        let next = boots[index + 1..]
            .iter()
            .find(|later| later.subject == Subject::Baseline)?;
        Some(Fallback {
            after: boot.label.to_owned(),
            boot: next.label.to_owned(),
            booted: passed(next.checks, Check::Identity) && passed(next.checks, Check::Root),
            healthy: planned.iter().all(|&check| passed(next.checks, check)),
        })
    });
    let verdict = if reasons.is_empty() {
        Verdict::Passed
    } else {
        Verdict::Failed
    };
    Decision {
        verdict,
        recommendation: match verdict {
            Verdict::Passed => Recommendation::Test,
            Verdict::Failed => Recommendation::Reject,
        },
        reasons,
        fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checks(failing: &[Check]) -> BTreeMap<Check, CheckResult> {
        [Check::Identity, Check::Root, Check::Health]
            .into_iter()
            .map(|check| {
                let passed = !failing.contains(&check);
                (
                    check,
                    CheckResult {
                        passed,
                        detail: if passed { "ok" } else { "bad" }.to_owned(),
                    },
                )
            })
            .collect()
    }

    const PLANNED: [Check; 3] = [Check::Identity, Check::Root, Check::Health];

    #[test]
    fn every_check_passing_recommends_tested() {
        let all = checks(&[]);
        let decision = decide(
            &PLANNED,
            &[BootChecks {
                label: "candidate",
                subject: Subject::Candidate,
                checks: &all,
            }],
        );
        assert_eq!(decision.verdict, Verdict::Passed);
        assert_eq!(decision.recommendation.command(), "generation.test");
        assert!(decision.reasons.is_empty());
        assert_eq!(decision.fallback, None);
    }

    #[test]
    fn any_failure_or_missing_check_rejects() {
        let unhealthy = checks(&[Check::Health]);
        let decision = decide(
            &PLANNED,
            &[BootChecks {
                label: "candidate",
                subject: Subject::Candidate,
                checks: &unhealthy,
            }],
        );
        assert_eq!(decision.verdict, Verdict::Failed);
        assert_eq!(decision.recommendation, Recommendation::Reject);
        assert_eq!(decision.reasons, vec!["candidate: health failed: bad"]);

        let all = checks(&[]);
        let decision = decide(
            &[Check::Identity, Check::Shutdown],
            &[BootChecks {
                label: "candidate",
                subject: Subject::Candidate,
                checks: &all,
            }],
        );
        assert_eq!(decision.reasons, vec!["candidate: shutdown was not run"]);
        assert_eq!(decide(&PLANNED, &[]).verdict, Verdict::Failed);
    }

    #[test]
    fn a_healthy_baseline_after_failed_candidate_boots_is_a_recorded_fallback() {
        let unhealthy = checks(&[Check::Health]);
        let healthy = checks(&[]);
        let boots = [
            BootChecks {
                label: "candidate-1",
                subject: Subject::Candidate,
                checks: &unhealthy,
            },
            BootChecks {
                label: "candidate-2",
                subject: Subject::Candidate,
                checks: &unhealthy,
            },
            BootChecks {
                label: "fallback",
                subject: Subject::Baseline,
                checks: &healthy,
            },
        ];
        let decision = decide(&PLANNED, &boots);
        assert_eq!(decision.verdict, Verdict::Failed);
        assert_eq!(decision.reasons.len(), 2);
        assert_eq!(
            decision.fallback,
            Some(Fallback {
                after: "candidate-1".to_owned(),
                boot: "fallback".to_owned(),
                booted: true,
                healthy: true,
            })
        );
    }
}
