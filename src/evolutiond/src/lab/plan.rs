//! The lab plan (`flowerly-lab-plan/v1`): what was built, how to boot it and
//! what the host expects the guest to report. The build writes it; the lab
//! refuses anything outside these bounds before it starts QEMU.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::str::FromStr;

use flowerly_core::storage::experiment::{ExperimentLane, MAX_HYPOTHESIS};
use flowerly_core::types::GenerationId;
use serde::{Deserialize, Serialize};

pub const PLAN_SCHEMA: &str = "flowerly-lab-plan/v1";
pub const MAX_PLAN_BYTES: u64 = 64 * 1024;
/// Each boot adds up to [`crate::benchmarks::METRICS_PER_BOOT`] metrics and
/// two artifacts; eight keeps a run inside the Experiment bounds.
pub const MAX_BOOTS: usize = 8;
pub const MAX_LIMIT_SECONDS: u64 = 3600;
const MAX_BINARIES: usize = 32;
const MAX_LABEL: usize = 32;
pub const DEFAULT_HYPOTHESIS: &str =
    "the candidate boots, reports what was built, passes its checks and powers off";

/// The checks a plan may name. Every one is computed by the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    /// The baked identity document is the one that was built.
    Identity,
    /// The booted slot and root PARTUUID are the expected ones.
    Root,
    /// Kernel release and `vmlinuz` hash.
    Kernel,
    /// The installed package list (claimed only in the reproducibility lane).
    Packages,
    /// Flowerly binary hashes.
    Binaries,
    /// Health gate passed, `flowerlyd` active and answering, no failed unit.
    Health,
    /// Admin round trip, Generation resolution and `evolutiond --self-test`.
    Integration,
    /// The guest powered itself off in time after its report.
    Shutdown,
}

impl Check {
    pub fn name(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Root => "root",
            Self::Kernel => "kernel",
            Self::Packages => "packages",
            Self::Binaries => "binaries",
            Self::Health => "health",
            Self::Integration => "integration",
            Self::Shutdown => "shutdown",
        }
    }
}

/// Checks no plan may leave out: without them a run proves nothing about the
/// candidate it names.
pub const REQUIRED_CHECKS: [Check; 3] = [Check::Identity, Check::Root, Check::Health];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accel {
    Kvm,
    /// Software emulation; only on explicit request (`--allow-tcg`).
    Tcg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subject {
    /// A boot of the candidate Generation.
    Candidate,
    /// A boot that must land on the baseline (fallback after a bad candidate).
    Baseline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema: String,
    pub lane: ExperimentLane,
    #[serde(default)]
    pub hypothesis: Option<String>,
    pub candidate: Candidate,
    #[serde(default)]
    pub baseline: Option<String>,
    pub firmware: Firmware,
    pub machine: Machine,
    pub limits: Limits,
    /// What each slot holds, as built.
    pub expect: BTreeMap<String, SlotExpectation>,
    pub checks: Vec<Check>,
    pub boots: Vec<Boot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub generation: String,
    /// The two-slot disk; the run boots it in place and consumes it.
    pub disk: PathBuf,
    /// The disk as built; a disk that was already booted is refused.
    pub disk_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Firmware {
    pub code: PathBuf,
    /// Template; each run boots a private copy.
    pub vars: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    pub accel: Accel,
    pub smp: u32,
    pub memory_mib: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// From QEMU start to a complete report.
    pub boot_seconds: u64,
    /// From the report to QEMU's own exit.
    pub shutdown_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotExpectation {
    pub generation: String,
    pub partuuid: String,
    pub identity_sha256: String,
    /// The `build_id` the slot's identity document and its Generation's
    /// recorded root identity share.
    pub build_id: String,
    pub kernel_release: String,
    pub vmlinuz_sha256: String,
    /// Required in the reproducibility lane; the upstream lane records what
    /// the guest reports instead of claiming a frozen set.
    #[serde(default)]
    pub packages_sha256: Option<String>,
    #[serde(default)]
    pub binaries: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Boot {
    pub label: String,
    pub subject: Subject,
    /// The slot the loader must choose on this boot.
    pub slot: String,
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_label(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_LABEL
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

/// The bounds flowerlyd applies to a root identity's `build_id`.
fn is_build_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn is_partuuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

fn generation(text: &str, what: &str) -> Result<(), String> {
    let printable = text.len() <= 128 && text.bytes().all(|b| b.is_ascii_graphic());
    GenerationId::from_str(text)
        .ok()
        .filter(|_| printable)
        .map(drop)
        .ok_or_else(|| format!("{what} `{text}` is not a Generation id"))
}

/// A path QEMU takes inside `-drive file=`: absolute, UTF-8, no comma.
pub fn qemu_path(path: &std::path::Path, what: &str) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or_else(|| format!("{what} path is not UTF-8"))?;
    if !path.is_absolute() || text.contains(',') || text.chars().any(char::is_control) {
        return Err(format!(
            "{what} path must be absolute without commas or control characters"
        ));
    }
    Ok(text.to_owned())
}

impl Plan {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_PLAN_BYTES {
            return Err("plan is too large".to_owned());
        }
        let plan: Self =
            serde_json::from_slice(bytes).map_err(|e| format!("plan is not a v1 plan: {e}"))?;
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != PLAN_SCHEMA {
            return Err(format!("plan schema is `{}`", self.schema));
        }
        if let Some(text) = &self.hypothesis {
            if text.trim().is_empty()
                || text.len() > MAX_HYPOTHESIS
                || text.chars().any(char::is_control)
            {
                return Err("plan hypothesis is empty, too long or has control characters".into());
            }
        }
        generation(&self.candidate.generation, "candidate")?;
        if let Some(baseline) = &self.baseline {
            generation(baseline, "baseline")?;
            if *baseline == self.candidate.generation {
                return Err("the baseline must differ from the candidate".to_owned());
            }
        }
        if !is_sha256(&self.candidate.disk_sha256) {
            return Err("candidate disk_sha256 is not a SHA-256".to_owned());
        }
        qemu_path(&self.candidate.disk, "candidate disk")?;
        qemu_path(&self.firmware.code, "firmware code")?;
        qemu_path(&self.firmware.vars, "firmware vars")?;
        if !(1..=16).contains(&self.machine.smp)
            || !(512..=65536).contains(&self.machine.memory_mib)
        {
            return Err("machine smp must be 1..=16 and memory_mib 512..=65536".to_owned());
        }
        for (what, value) in [
            ("boot_seconds", self.limits.boot_seconds),
            ("shutdown_seconds", self.limits.shutdown_seconds),
        ] {
            if !(1..=MAX_LIMIT_SECONDS).contains(&value) {
                return Err(format!("limits.{what} must be 1..={MAX_LIMIT_SECONDS}"));
            }
        }
        self.validate_expectations()?;
        let checks: BTreeSet<Check> = self.checks.iter().copied().collect();
        if checks.len() != self.checks.len() {
            return Err("plan names a check twice".to_owned());
        }
        if let Some(missing) = REQUIRED_CHECKS.iter().find(|c| !checks.contains(c)) {
            return Err(format!(
                "plan leaves out the required check `{}`",
                missing.name()
            ));
        }
        self.validate_boots()
    }

    fn validate_expectations(&self) -> Result<(), String> {
        if self.expect.is_empty() || self.expect.len() > 2 {
            return Err("plan must describe one or two slots".to_owned());
        }
        for (slot, expected) in &self.expect {
            if slot != "root-a" && slot != "root-b" {
                return Err(format!("unknown slot `{slot}`"));
            }
            generation(&expected.generation, "slot generation")?;
            if !is_partuuid(&expected.partuuid) {
                return Err(format!("{slot} partuuid is not a lowercase UUID"));
            }
            if !is_build_id(&expected.build_id) {
                return Err(format!("{slot} build_id must be 1..64 of [A-Za-z0-9_.-]"));
            }
            let hashes = [
                ("identity_sha256", Some(&expected.identity_sha256)),
                ("vmlinuz_sha256", Some(&expected.vmlinuz_sha256)),
                ("packages_sha256", expected.packages_sha256.as_ref()),
            ];
            for (what, value) in hashes {
                if value.is_some_and(|v| !is_sha256(v)) {
                    return Err(format!("{slot} {what} is not a SHA-256"));
                }
            }
            if self.lane == ExperimentLane::Reproducibility && expected.packages_sha256.is_none() {
                return Err(format!(
                    "{slot} needs packages_sha256 in the reproducibility lane"
                ));
            }
            let release = &expected.kernel_release;
            if release.is_empty()
                || release.len() > 64
                || !release
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
            {
                return Err(format!("{slot} kernel_release is malformed"));
            }
            if expected.binaries.len() > MAX_BINARIES
                || expected
                    .binaries
                    .iter()
                    .any(|(name, hash)| !is_label(name) || !is_sha256(hash))
            {
                return Err(format!("{slot} binaries are malformed"));
            }
        }
        Ok(())
    }

    fn validate_boots(&self) -> Result<(), String> {
        if self.boots.is_empty() || self.boots.len() > MAX_BOOTS {
            return Err(format!("plan must have 1..={MAX_BOOTS} boots"));
        }
        let mut labels = BTreeSet::new();
        for boot in &self.boots {
            if !is_label(&boot.label) || !labels.insert(boot.label.as_str()) {
                return Err(format!(
                    "boot label `{}` is malformed or repeated",
                    boot.label
                ));
            }
            let expected = self
                .expect
                .get(&boot.slot)
                .ok_or_else(|| format!("boot {} names an undescribed slot", boot.label))?;
            let owner = match boot.subject {
                Subject::Candidate => Some(&self.candidate.generation),
                Subject::Baseline => self.baseline.as_ref(),
            };
            if owner != Some(&expected.generation) {
                return Err(format!(
                    "boot {}: slot {} does not hold the {:?} Generation",
                    boot.label, boot.slot, boot.subject
                ));
            }
        }
        if !self.boots.iter().any(|b| b.subject == Subject::Candidate) {
            return Err("plan never boots the candidate".to_owned());
        }
        Ok(())
    }

    pub fn hypothesis(&self) -> &str {
        self.hypothesis.as_deref().unwrap_or(DEFAULT_HYPOTHESIS)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::{json, Value};

    pub(crate) const CANDIDATE: &str = "gen-candidate";
    pub(crate) const BASELINE: &str = "gen-baseline";

    pub(crate) fn sample_value() -> Value {
        json!({
            "schema": PLAN_SCHEMA,
            "lane": "reproducibility",
            "candidate": {
                "generation": CANDIDATE,
                "disk": "/lab/candidate.img",
                "disk_sha256": "a".repeat(64)
            },
            "baseline": BASELINE,
            "firmware": {"code": "/fw/OVMF_CODE.fd", "vars": "/fw/OVMF_VARS.fd"},
            "machine": {"accel": "kvm", "smp": 2, "memory_mib": 2048},
            "limits": {"boot_seconds": 600, "shutdown_seconds": 120},
            "expect": {
                "root-a": {
                    "generation": BASELINE,
                    "partuuid": "00010203-0405-4607-8809-0a0b0c0d0e0f",
                    "identity_sha256": "5".repeat(64),
                    "build_id": "lab-a",
                    "kernel_release": "6.17.1-arch1-1",
                    "vmlinuz_sha256": "2".repeat(64),
                    "packages_sha256": "3".repeat(64),
                    "binaries": {"flowerlyd": "6".repeat(64)}
                },
                "root-b": {
                    "generation": CANDIDATE,
                    "partuuid": "0f0e0d0c-0b0a-4908-8706-050403020100",
                    "identity_sha256": "1".repeat(64),
                    "build_id": "lab-b",
                    "kernel_release": "6.17.1-arch1-1",
                    "vmlinuz_sha256": "2".repeat(64),
                    "packages_sha256": "3".repeat(64),
                    "binaries": {"flowerlyd": "4".repeat(64)}
                }
            },
            "checks": ["identity", "root", "kernel", "packages", "binaries",
                       "health", "integration", "shutdown"],
            "boots": [{"label": "candidate", "subject": "candidate", "slot": "root-b"}]
        })
    }

    pub(crate) fn sample() -> Plan {
        Plan::parse(&serde_json::to_vec(&sample_value()).unwrap()).unwrap()
    }

    #[test]
    fn the_sample_plan_is_valid() {
        let plan = sample();
        assert_eq!(plan.hypothesis(), DEFAULT_HYPOTHESIS);
        assert_eq!(plan.checks.len(), 8);
    }

    #[test]
    fn plans_outside_their_bounds_are_refused() {
        type Mutation = fn(&mut Value);
        let cases: &[(&str, Mutation)] = &[
            ("schema", |v| v["schema"] = json!("flowerly-lab-plan/v2")),
            ("unknown field", |v| v["extra"] = json!(1)),
            ("candidate id", |v| {
                v["candidate"]["generation"] = json!("exp-x")
            }),
            ("baseline is candidate", |v| {
                v["baseline"] = json!(CANDIDATE)
            }),
            ("disk hash", |v| {
                v["candidate"]["disk_sha256"] = json!("A".repeat(64))
            }),
            ("relative disk", |v| {
                v["candidate"]["disk"] = json!("candidate.img")
            }),
            ("comma in path", |v| {
                v["firmware"]["code"] = json!("/fw/a,b.fd")
            }),
            ("accel", |v| v["machine"]["accel"] = json!("hvf")),
            ("smp", |v| v["machine"]["smp"] = json!(0)),
            ("memory", |v| v["machine"]["memory_mib"] = json!(128)),
            ("boot limit", |v| v["limits"]["boot_seconds"] = json!(0)),
            ("shutdown limit", |v| {
                v["limits"]["shutdown_seconds"] = json!(3601)
            }),
            ("slot name", |v| {
                let slot = v["expect"]["root-a"].take();
                v["expect"]["root-c"] = slot;
            }),
            ("partuuid", |v| {
                v["expect"]["root-b"]["partuuid"] = json!("not-a-uuid")
            }),
            ("build id", |v| {
                v["expect"]["root-b"]["build_id"] = json!("../b")
            }),
            ("packages in repro lane", |v| {
                v["expect"]["root-b"]
                    .as_object_mut()
                    .unwrap()
                    .remove("packages_sha256");
            }),
            ("kernel release", |v| {
                v["expect"]["root-b"]["kernel_release"] = json!("6.1 x")
            }),
            ("binary name", |v| {
                v["expect"]["root-b"]["binaries"] = json!({"Bad": "4".repeat(64)})
            }),
            ("unknown check", |v| {
                v["checks"] = json!(["identity", "root", "health", "fitness"])
            }),
            ("repeated check", |v| {
                v["checks"] = json!(["identity", "root", "health", "root"])
            }),
            ("missing health", |v| {
                v["checks"] = json!(["identity", "root", "shutdown"])
            }),
            ("no boots", |v| v["boots"] = json!([])),
            ("repeated label", |v| {
                v["boots"] = json!([
                    {"label": "b", "subject": "candidate", "slot": "root-b"},
                    {"label": "b", "subject": "candidate", "slot": "root-b"}
                ]);
            }),
            ("wrong slot owner", |v| {
                v["boots"] = json!([{"label": "b", "subject": "candidate", "slot": "root-a"}]);
            }),
            ("baseline only", |v| {
                v["boots"] = json!([{"label": "a", "subject": "baseline", "slot": "root-a"}]);
            }),
            ("too many boots", |v| {
                let boots: Vec<Value> = (0..=MAX_BOOTS)
                    .map(|i| json!({"label": format!("b{i}"), "subject": "candidate", "slot": "root-b"}))
                    .collect();
                v["boots"] = json!(boots);
            }),
        ];
        for (label, mutate) in cases {
            let mut value = sample_value();
            mutate(&mut value);
            assert!(
                Plan::parse(&serde_json::to_vec(&value).unwrap()).is_err(),
                "{label} must be refused"
            );
        }
    }

    #[test]
    fn the_upstream_lane_may_leave_the_package_set_open() {
        let mut value = sample_value();
        value["lane"] = json!("upstream");
        for slot in ["root-a", "root-b"] {
            value["expect"][slot]
                .as_object_mut()
                .unwrap()
                .remove("packages_sha256");
        }
        assert!(Plan::parse(&serde_json::to_vec(&value).unwrap()).is_ok());
    }

    /// What `scripts/lab-candidate.py prepare --scenario rollback` writes for
    /// its test fixture; `scripts/test-lab-candidate.py` proves it still does.
    const PREPARED_ROLLBACK: &str =
        include_str!("../../../../testbed/evolution-world/fixtures/lab-plan-rollback.json");

    /// What the same tool boots first on each run's copy: slot A alone, as
    /// the machine's first boot, before slot B is armed. Never recorded.
    const PREPARED_INITIAL: &str =
        include_str!("../../../../testbed/evolution-world/fixtures/lab-plan-initial.json");

    #[test]
    fn the_candidate_tools_initial_plan_is_valid() {
        let plan = Plan::parse(PREPARED_INITIAL.as_bytes()).unwrap();
        assert_eq!(plan.lane, ExperimentLane::Reproducibility);
        assert_eq!(plan.baseline, None);
        assert_eq!(plan.candidate.generation, BASELINE);
        assert_eq!(plan.expect.keys().collect::<Vec<_>>(), ["root-a"]);
        assert_ne!(plan.hypothesis(), DEFAULT_HYPOTHESIS);
        let boots: Vec<_> = plan
            .boots
            .iter()
            .map(|b| (b.label.as_str(), b.subject, b.slot.as_str()))
            .collect();
        assert_eq!(boots, [("initial", Subject::Candidate, "root-a")]);
    }

    #[test]
    fn the_candidate_tools_rollback_plan_is_valid() {
        let plan = Plan::parse(PREPARED_ROLLBACK.as_bytes()).unwrap();
        assert_eq!(plan.lane, ExperimentLane::Reproducibility);
        assert_eq!(plan.checks.len(), 8);
        assert_ne!(plan.hypothesis(), DEFAULT_HYPOTHESIS);
        let boots: Vec<_> = plan
            .boots
            .iter()
            .map(|b| (b.label.as_str(), b.subject, b.slot.as_str()))
            .collect();
        assert_eq!(
            boots,
            [
                ("candidate-1", Subject::Candidate, "root-b"),
                ("candidate-2", Subject::Candidate, "root-b"),
                ("fallback", Subject::Baseline, "root-a"),
            ]
        );
    }
}
