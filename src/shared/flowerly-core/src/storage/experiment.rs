//! Complete Experiment records (ADR-0024 D5).
//!
//! An Experiment is written by `generation.evidence`
//! ([`Store::record_evidence`]); this module holds the optional details an
//! Evolution Lab run adds (hypothesis, baseline, plan, metrics, artifacts),
//! their validation, and the read side. Artifacts are content-addressed: each
//! becomes an `artifacts` row with the URI `sha256:<hex>` and an
//! `experiment produced artifact` provenance edge whose note carries the file
//! name and size. The bytes stay with the lab run's output directory.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{insert_provenance_row, Result, StorageError, Store};
use crate::types::{
    ArtifactId, ExperimentId, GenerationId, ObjectRef, ProvenanceEdge, ProvenanceRelation,
    Timestamp,
};

pub const MAX_HYPOTHESIS: usize = 512;
pub const MAX_PLAN_CHECKS: usize = 64;
pub const MAX_METRICS: usize = 64;
pub const MAX_ARTIFACTS: usize = 64;
pub const MAX_ARTIFACT_NAME: usize = 128;
pub const MAX_EXPERIMENT_LIST: usize = 200;
const MAX_NAME: usize = 64;
const MAX_KIND: usize = 32;

/// The hypothesis of an Experiment recorded without details.
pub const DEFAULT_HYPOTHESIS: &str = "generation acceptance";

/// Which inputs a lab run used (ADR-0024 D6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentLane {
    /// The frozen package set; a changed result means a changed Flowerly.
    Reproducibility,
    /// Packages resolved from the current Arch repositories on the run day.
    Upstream,
}

impl ExperimentLane {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reproducibility => "reproducibility",
            Self::Upstream => "upstream",
        }
    }
}

/// One file a run produced, named by its content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentArtifact {
    pub kind: String,
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
}

/// What an Evolution Lab run adds to the Experiment it records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentDetails {
    pub hypothesis: String,
    #[serde(default)]
    pub baseline: Option<GenerationId>,
    pub test_plan: Vec<String>,
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    #[serde(default)]
    pub artifacts: Vec<ExperimentArtifact>,
    #[serde(default)]
    pub lane: Option<ExperimentLane>,
}

/// A stored artifact of an Experiment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactRecord {
    pub id: ArtifactId,
    pub kind: String,
    pub uri: String,
    pub name: Option<String>,
    pub bytes: Option<u64>,
}

/// An Experiment as stored, with its artifacts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExperimentRecord {
    pub id: ExperimentId,
    pub goal: Option<String>,
    pub hypothesis: String,
    pub candidate: Option<String>,
    pub baseline: Option<String>,
    pub metrics: Value,
    pub test_plan: Value,
    pub result: Option<String>,
    pub decision: Option<String>,
    pub created_at: Timestamp,
    pub artifacts: Vec<ArtifactRecord>,
}

/// One row of [`Store::experiments`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExperimentSummary {
    pub id: ExperimentId,
    pub hypothesis: String,
    pub candidate: Option<String>,
    pub baseline: Option<String>,
    pub result: Option<String>,
    pub decision: Option<String>,
    pub created_at: Timestamp,
}

fn invalid(message: impl Into<String>) -> StorageError {
    StorageError::InvalidOperation(message.into())
}

fn is_name(text: &str, max: usize) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= max
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A relative path inside the run's output directory: no absolute path, no
/// `..`, no empty component, nothing but `[A-Za-z0-9._/-]`.
fn is_artifact_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_ARTIFACT_NAME
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        && text
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

impl ExperimentDetails {
    /// Refuse anything outside the documented bounds before a transaction.
    pub fn validate(&self) -> Result<()> {
        let hypothesis = self.hypothesis.as_str();
        if hypothesis.trim().is_empty()
            || hypothesis.len() > MAX_HYPOTHESIS
            || hypothesis.chars().any(char::is_control)
        {
            return Err(invalid(format!(
                "hypothesis must be 1..{MAX_HYPOTHESIS} bytes, not blank, without control characters"
            )));
        }
        if let Some(baseline) = &self.baseline {
            GenerationId::from_str(baseline.as_str())
                .map_err(|error| invalid(format!("baseline: {error}")))?;
        }
        if self.test_plan.is_empty() || self.test_plan.len() > MAX_PLAN_CHECKS {
            return Err(invalid(format!(
                "test_plan must name 1..{MAX_PLAN_CHECKS} checks"
            )));
        }
        let mut checks = BTreeSet::new();
        for check in &self.test_plan {
            if !is_name(check, MAX_NAME) {
                return Err(invalid(format!(
                    "check `{check}` must be 1..{MAX_NAME} of [a-z0-9._-], starting with a letter or digit"
                )));
            }
            if !checks.insert(check) {
                return Err(invalid(format!("check `{check}` is listed twice")));
            }
        }
        if self.metrics.len() > MAX_METRICS {
            return Err(invalid(format!("at most {MAX_METRICS} metrics")));
        }
        for (name, value) in &self.metrics {
            if !is_name(name, MAX_NAME) {
                return Err(invalid(format!(
                    "metric `{name}` must be 1..{MAX_NAME} of [a-z0-9._-], starting with a letter or digit"
                )));
            }
            if !value.is_finite() {
                return Err(invalid(format!("metric `{name}` must be a finite number")));
            }
        }
        if self.artifacts.len() > MAX_ARTIFACTS {
            return Err(invalid(format!("at most {MAX_ARTIFACTS} artifacts")));
        }
        let mut names = BTreeSet::new();
        for artifact in &self.artifacts {
            if !is_name(&artifact.kind, MAX_KIND) {
                return Err(invalid(format!(
                    "artifact kind `{}` must be 1..{MAX_KIND} of [a-z0-9._-]",
                    artifact.kind
                )));
            }
            if !is_artifact_name(&artifact.name) {
                return Err(invalid(format!(
                    "artifact name `{}` must be a relative path of 1..{MAX_ARTIFACT_NAME} bytes of [A-Za-z0-9._/-] without `..`",
                    artifact.name
                )));
            }
            if !is_sha256(&artifact.sha256) {
                return Err(invalid(format!(
                    "artifact `{}` sha256 must be 64 lowercase hex digits",
                    artifact.name
                )));
            }
            if artifact.bytes > i64::MAX as u64 {
                return Err(invalid(format!(
                    "artifact `{}` is too large",
                    artifact.name
                )));
            }
            if !names.insert(&artifact.name) {
                return Err(invalid(format!(
                    "artifact `{}` is listed twice",
                    artifact.name
                )));
            }
        }
        Ok(())
    }

    /// SHA-256 of the canonical JSON of these details: a retry with the same
    /// evidence must carry the same details.
    #[must_use]
    pub fn digest(&self) -> String {
        let canonical = serde_json::to_vec(self).expect("details serialize");
        Sha256::digest(canonical)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// The `metrics` document of an Experiment.
pub(super) fn metrics_json(evidence_sha256: &str, details: Option<&ExperimentDetails>) -> Value {
    let mut metrics = json!({ "evidence_sha256": evidence_sha256 });
    if let Some(details) = details {
        metrics["details_sha256"] = json!(details.digest());
        metrics["values"] = json!(details.metrics);
        if let Some(lane) = details.lane {
            metrics["lane"] = json!(lane.as_str());
        }
    }
    metrics
}

/// Write the artifacts of a new Experiment inside the caller's transaction.
pub(super) fn insert_artifacts(
    tx: &Connection,
    experiment: &ExperimentId,
    artifacts: &[ExperimentArtifact],
    now: Timestamp,
) -> Result<()> {
    for artifact in artifacts {
        let id = ArtifactId::new();
        tx.execute(
            "INSERT INTO artifacts (id, kind, uri, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                id.as_str(),
                artifact.kind,
                format!("sha256:{}", artifact.sha256),
                now.as_millis(),
            ],
        )?;
        insert_provenance_row(
            tx,
            &ProvenanceEdge {
                from: ObjectRef::from(experiment),
                relation: ProvenanceRelation::Produced,
                to: ObjectRef::from(&id),
                created_at: now,
                note: Some(json!({ "name": artifact.name, "bytes": artifact.bytes }).to_string()),
            },
        )?;
    }
    Ok(())
}

fn parse_json(text: &str, column: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|_| invalid(format!("invalid stored experiment {column}")))
}

fn load_artifacts(conn: &Connection, experiment: &ExperimentId) -> Result<Vec<ArtifactRecord>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.kind, a.uri, e.note
           FROM provenance_edges AS e JOIN artifacts AS a ON a.id = e.to_ref
          WHERE e.from_ref = ?1 AND e.relation = 'produced'
          ORDER BY e.id",
    )?;
    let rows = stmt.query_map(params![experiment.as_str()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut artifacts = Vec::new();
    for row in rows {
        let (id, kind, uri, note) = row?;
        let note: Value = note
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or(Value::Null);
        artifacts.push(ArtifactRecord {
            id: ArtifactId::from_raw(id),
            kind,
            uri,
            name: note.get("name").and_then(Value::as_str).map(str::to_owned),
            bytes: note.get("bytes").and_then(Value::as_u64),
        });
    }
    Ok(artifacts)
}

impl Store {
    /// One Experiment with its artifacts, or `None`.
    pub fn experiment(&self, id: &ExperimentId) -> Result<Option<ExperimentRecord>> {
        let row = self
            .conn
            .query_row(
                "SELECT goal_id, hypothesis, candidate, baseline, metrics, test_plan, result,
                        decision, created_at
                   FROM experiments WHERE id = ?1",
                params![id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, i64>(8)?,
                    ))
                },
            )
            .optional()?;
        let Some((goal, hypothesis, candidate, baseline, metrics, plan, result, decision, at)) =
            row
        else {
            return Ok(None);
        };
        Ok(Some(ExperimentRecord {
            id: id.clone(),
            goal,
            hypothesis,
            candidate,
            baseline,
            metrics: parse_json(&metrics, "metrics")?,
            test_plan: parse_json(&plan, "test_plan")?,
            result,
            decision,
            created_at: Timestamp::from_millis(at),
            artifacts: load_artifacts(&self.conn, id)?,
        }))
    }

    /// Experiments, newest first, optionally only those about `candidate`.
    pub fn experiments(
        &self,
        candidate: Option<&GenerationId>,
        limit: usize,
    ) -> Result<Vec<ExperimentSummary>> {
        if limit == 0 || limit > MAX_EXPERIMENT_LIST {
            return Err(invalid(format!("limit must be 1..{MAX_EXPERIMENT_LIST}")));
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, hypothesis, candidate, baseline, result, decision, created_at
               FROM experiments
              WHERE ?1 IS NULL OR candidate = ?1
              ORDER BY created_at DESC, id DESC
              LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![candidate.map(GenerationId::as_str), limit as i64],
            |row| {
                Ok(ExperimentSummary {
                    id: ExperimentId::from_raw(row.get::<_, String>(0)?),
                    hypothesis: row.get(1)?,
                    candidate: row.get(2)?,
                    baseline: row.get(3)?,
                    result: row.get(4)?,
                    decision: row.get(5)?,
                    created_at: Timestamp::from_millis(row.get(6)?),
                })
            },
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(StorageError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::lifecycle::{EvidenceOutcome, RootIdentity};
    use crate::types::{Caller, Generation, GenerationStatus, Owner};

    const ADMIN: Caller = Caller::new(0);
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn store() -> Store {
        let mut store = Store::open_in_memory().expect("open store");
        store.migrate().expect("migrate");
        store
    }

    fn hash(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn candidate(store: &Store) -> GenerationId {
        let generation = Generation {
            id: GenerationId::new(),
            parent: None,
            source_commit: None,
            system_manifest: None,
            reason_goal: None,
            experiment: None,
            status: GenerationStatus::Candidate,
            created_at: Timestamp::now(),
            owner: Owner::System,
        };
        store.insert_generation(&generation).expect("insert");
        generation.id
    }

    fn with_identity(store: &mut Store, id: &GenerationId) {
        let identity = RootIdentity {
            rootfs_sha256: hash('a'),
            kernel_sha256: hash('b'),
            initramfs_sha256: hash('c'),
            input_manifest_sha256: hash('d'),
            build_id: "build-1".to_owned(),
        };
        store
            .set_root_identity(ADMIN, id, COMMIT, &identity, "built")
            .unwrap();
    }

    fn details(baseline: Option<GenerationId>) -> ExperimentDetails {
        ExperimentDetails {
            hypothesis: "the candidate boots and answers".to_owned(),
            baseline,
            test_plan: vec!["identity.root".to_owned(), "health.flowerlyd".to_owned()],
            metrics: BTreeMap::from([
                ("boot.report_ms".to_owned(), 8125.0),
                ("image.root_bytes".to_owned(), 1_048_576.0),
            ]),
            artifacts: vec![
                ExperimentArtifact {
                    kind: "result".to_owned(),
                    name: "result.json".to_owned(),
                    sha256: hash('e'),
                    bytes: 2048,
                },
                ExperimentArtifact {
                    kind: "serial-log".to_owned(),
                    name: "boot-1/serial.log".to_owned(),
                    sha256: hash('f'),
                    bytes: 65536,
                },
            ],
            lane: Some(ExperimentLane::Reproducibility),
        }
    }

    fn is_invalid<T: std::fmt::Debug>(result: Result<T>) -> bool {
        matches!(result, Err(StorageError::InvalidOperation(_)))
    }

    fn is_conflict<T: std::fmt::Debug>(result: Result<T>) -> bool {
        matches!(result, Err(StorageError::OperationConflict(_)))
    }

    fn artifact_rows(store: &Store) -> i64 {
        store
            .conn
            .query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))
            .unwrap()
    }

    type Mutation = Box<dyn Fn(&mut ExperimentDetails)>;

    #[test]
    fn details_outside_their_bounds_are_refused() {
        let base = details(None);
        assert!(base.validate().is_ok());
        let mutations: Vec<(&str, Mutation)> = vec![
            (
                "blank hypothesis",
                Box::new(|d| d.hypothesis = " ".to_owned()),
            ),
            (
                "long hypothesis",
                Box::new(|d| d.hypothesis = "h".repeat(MAX_HYPOTHESIS + 1)),
            ),
            (
                "control character",
                Box::new(|d| d.hypothesis = "boots\nfine".to_owned()),
            ),
            (
                "baseline prefix",
                Box::new(|d| d.baseline = Some(GenerationId::from_raw("goal-x".to_owned()))),
            ),
            ("empty plan", Box::new(|d| d.test_plan.clear())),
            (
                "long plan",
                Box::new(|d| {
                    d.test_plan = (0..=MAX_PLAN_CHECKS).map(|i| format!("c{i}")).collect();
                }),
            ),
            (
                "check name",
                Box::new(|d| d.test_plan.push("Health".to_owned())),
            ),
            (
                "check starts with a dot",
                Box::new(|d| d.test_plan.push(".hidden".to_owned())),
            ),
            (
                "duplicate check",
                Box::new(|d| d.test_plan.push("identity.root".to_owned())),
            ),
            (
                "metric name",
                Box::new(|d| {
                    d.metrics.insert("boot time".to_owned(), 1.0);
                }),
            ),
            (
                "metric not finite",
                Box::new(|d| {
                    d.metrics.insert("boot.ms".to_owned(), f64::NAN);
                }),
            ),
            (
                "too many metrics",
                Box::new(|d| {
                    d.metrics = (0..=MAX_METRICS).map(|i| (format!("m{i}"), 1.0)).collect();
                }),
            ),
            (
                "artifact kind",
                Box::new(|d| d.artifacts[0].kind = "Result".to_owned()),
            ),
            (
                "absolute artifact name",
                Box::new(|d| d.artifacts[0].name = "/etc/shadow".to_owned()),
            ),
            (
                "parent artifact name",
                Box::new(|d| d.artifacts[0].name = "boot/../../x".to_owned()),
            ),
            (
                "empty artifact component",
                Box::new(|d| d.artifacts[0].name = "boot//x".to_owned()),
            ),
            (
                "artifact name character",
                Box::new(|d| d.artifacts[0].name = "result json".to_owned()),
            ),
            (
                "long artifact name",
                Box::new(|d| d.artifacts[0].name = "a".repeat(MAX_ARTIFACT_NAME + 1)),
            ),
            (
                "artifact hash case",
                Box::new(|d| d.artifacts[0].sha256 = "E".repeat(64)),
            ),
            (
                "artifact hash length",
                Box::new(|d| d.artifacts[0].sha256 = "e".repeat(63)),
            ),
            (
                "artifact size",
                Box::new(|d| d.artifacts[0].bytes = u64::MAX),
            ),
            (
                "duplicate artifact",
                Box::new(|d| d.artifacts[1].name = "result.json".to_owned()),
            ),
            (
                "too many artifacts",
                Box::new(|d| {
                    d.artifacts = (0..=MAX_ARTIFACTS)
                        .map(|i| ExperimentArtifact {
                            kind: "log".to_owned(),
                            name: format!("f{i}"),
                            sha256: hash('e'),
                            bytes: 1,
                        })
                        .collect();
                }),
            ),
        ];
        for (label, mutate) in mutations {
            let mut bad = base.clone();
            mutate(&mut bad);
            assert!(is_invalid(bad.validate()), "{label}");
        }
    }

    #[test]
    fn unknown_detail_fields_are_refused() {
        let mut value = serde_json::to_value(details(None)).unwrap();
        value["fitness"] = json!(1.0);
        assert!(serde_json::from_value::<ExperimentDetails>(value).is_err());
        let mut value = serde_json::to_value(details(None)).unwrap();
        value["artifacts"][0]["path"] = json!("/tmp/x");
        assert!(serde_json::from_value::<ExperimentDetails>(value).is_err());
        let value = json!({ "hypothesis": "h", "test_plan": ["a"], "lane": "rolling" });
        assert!(serde_json::from_value::<ExperimentDetails>(value).is_err());
    }

    #[test]
    fn the_digest_binds_every_detail() {
        let base = details(None);
        assert_eq!(base.digest(), details(None).digest());
        assert_eq!(base.digest().len(), 64);
        let mut other = base.clone();
        other.metrics.insert("boot.report_ms".to_owned(), 8126.0);
        assert_ne!(base.digest(), other.digest());
        let mut other = base.clone();
        other.lane = Some(ExperimentLane::Upstream);
        assert_ne!(base.digest(), other.digest());
    }

    #[test]
    fn evidence_with_details_is_a_complete_record() {
        let mut store = store();
        let baseline = candidate(&store);
        let id = candidate(&store);
        with_identity(&mut store, &id);
        let experiment = store
            .record_evidence(
                ADMIN,
                &id,
                EvidenceOutcome::Passed,
                &hash('9'),
                Some(&details(Some(baseline.clone()))),
                "lab run",
            )
            .unwrap();
        let record = store.experiment(&experiment).unwrap().expect("stored");
        assert_eq!(record.hypothesis, "the candidate boots and answers");
        assert_eq!(record.candidate.as_deref(), Some(id.as_str()));
        assert_eq!(record.baseline.as_deref(), Some(baseline.as_str()));
        assert_eq!(record.result.as_deref(), Some("passed"));
        assert_eq!(record.decision, None);
        assert_eq!(
            record.test_plan,
            json!(["identity.root", "health.flowerlyd"])
        );
        assert_eq!(record.metrics["evidence_sha256"], hash('9'));
        assert_eq!(
            record.metrics["details_sha256"],
            details(Some(baseline)).digest()
        );
        assert_eq!(record.metrics["values"]["boot.report_ms"], 8125.0);
        assert_eq!(record.metrics["lane"], "reproducibility");
        let artifacts: Vec<_> = record
            .artifacts
            .iter()
            .map(|a| (a.kind.as_str(), a.uri.clone(), a.name.as_deref(), a.bytes))
            .collect();
        assert_eq!(
            artifacts,
            vec![
                (
                    "result",
                    format!("sha256:{}", hash('e')),
                    Some("result.json"),
                    Some(2048)
                ),
                (
                    "serial-log",
                    format!("sha256:{}", hash('f')),
                    Some("boot-1/serial.log"),
                    Some(65536)
                ),
            ]
        );

        store
            .declare_tested(ADMIN, &id, &ObjectRef::from(&experiment), "passed")
            .unwrap();
        let record = store.experiment(&experiment).unwrap().unwrap();
        assert_eq!(record.decision.as_deref(), Some("tested"));
    }

    #[test]
    fn evidence_without_details_keeps_the_0_0_5_record() {
        let mut store = store();
        let id = candidate(&store);
        let experiment = store
            .record_evidence(ADMIN, &id, EvidenceOutcome::Failed, &hash('9'), None, "run")
            .unwrap();
        let record = store.experiment(&experiment).unwrap().unwrap();
        assert_eq!(record.hypothesis, DEFAULT_HYPOTHESIS);
        assert_eq!(record.baseline, None);
        assert_eq!(record.test_plan, json!([]));
        assert_eq!(record.metrics, json!({ "evidence_sha256": hash('9') }));
        assert!(record.artifacts.is_empty());
    }

    #[test]
    fn a_retry_must_carry_the_same_details() {
        let mut store = store();
        let id = candidate(&store);
        let first = store
            .record_evidence(
                ADMIN,
                &id,
                EvidenceOutcome::Passed,
                &hash('9'),
                Some(&details(None)),
                "run",
            )
            .unwrap();
        let retry = store
            .record_evidence(
                ADMIN,
                &id,
                EvidenceOutcome::Passed,
                &hash('9'),
                Some(&details(None)),
                "retry",
            )
            .unwrap();
        assert_eq!(first, retry);
        assert_eq!(artifact_rows(&store), 2, "a retry writes no artifacts");
        assert!(is_conflict(store.record_evidence(
            ADMIN,
            &id,
            EvidenceOutcome::Passed,
            &hash('9'),
            None,
            "without details",
        )));
        let mut changed = details(None);
        changed.artifacts[0].sha256 = hash('0');
        assert!(is_conflict(store.record_evidence(
            ADMIN,
            &id,
            EvidenceOutcome::Passed,
            &hash('9'),
            Some(&changed),
            "other details",
        )));
        assert_eq!(artifact_rows(&store), 2);
    }

    #[test]
    fn the_baseline_must_exist_and_differ_from_the_candidate() {
        let mut store = store();
        let id = candidate(&store);
        let missing = GenerationId::new();
        assert!(matches!(
            store.record_evidence(
                ADMIN,
                &id,
                EvidenceOutcome::Passed,
                &hash('9'),
                Some(&details(Some(missing))),
                "run",
            ),
            Err(StorageError::NotFound(_))
        ));
        assert!(is_invalid(store.record_evidence(
            ADMIN,
            &id,
            EvidenceOutcome::Passed,
            &hash('9'),
            Some(&details(Some(id.clone()))),
            "run",
        )));
        assert_eq!(artifact_rows(&store), 0, "a refused record leaves nothing");
        assert!(store.experiments(None, 10).unwrap().is_empty());
    }

    #[test]
    fn a_rejection_decides_the_named_experiment() {
        let mut store = store();
        let id = candidate(&store);
        let experiment = store
            .record_evidence(ADMIN, &id, EvidenceOutcome::Failed, &hash('9'), None, "run")
            .unwrap();
        let rejected = store
            .reject_generation(ADMIN, &id, Some(&experiment), "health failed")
            .unwrap();
        assert_eq!(rejected.status, GenerationStatus::Rejected);
        let record = store.experiment(&experiment).unwrap().unwrap();
        assert_eq!(record.decision.as_deref(), Some("rejected"));
    }

    #[test]
    fn a_rejection_refuses_a_foreign_missing_or_decided_experiment() {
        let mut store = store();
        let id = candidate(&store);
        let other = candidate(&store);
        let foreign = store
            .record_evidence(
                ADMIN,
                &other,
                EvidenceOutcome::Failed,
                &hash('9'),
                None,
                "run",
            )
            .unwrap();
        assert!(is_conflict(store.reject_generation(
            ADMIN,
            &id,
            Some(&foreign),
            "x"
        )));
        assert!(matches!(
            store.reject_generation(ADMIN, &id, Some(&ExperimentId::new()), "x"),
            Err(StorageError::NotFound(_))
        ));
        let unchanged = store.get_generation(&id).unwrap().unwrap();
        assert_eq!(unchanged.status, GenerationStatus::Candidate, "rolled back");
        assert_eq!(store.experiment(&foreign).unwrap().unwrap().decision, None);

        with_identity(&mut store, &id);
        let passed = store
            .record_evidence(ADMIN, &id, EvidenceOutcome::Passed, &hash('8'), None, "run")
            .unwrap();
        store
            .declare_tested(ADMIN, &id, &ObjectRef::from(&passed), "passed")
            .unwrap();
        assert!(is_conflict(store.reject_generation(
            ADMIN,
            &id,
            Some(&passed),
            "x"
        )));
        let unchanged = store.get_generation(&id).unwrap().unwrap();
        assert_eq!(unchanged.status, GenerationStatus::Tested);
        store
            .reject_generation(ADMIN, &id, None, "superseded by a fix")
            .unwrap();
    }

    #[test]
    fn experiments_are_listed_newest_first_and_filtered() {
        let mut store = store();
        let a = candidate(&store);
        let b = candidate(&store);
        let mut recorded = Vec::new();
        for (n, id) in [&a, &b, &a].into_iter().enumerate() {
            let experiment = store
                .record_evidence(
                    ADMIN,
                    id,
                    EvidenceOutcome::Failed,
                    &hash(char::from(b'1' + n as u8)),
                    None,
                    "run",
                )
                .unwrap();
            store
                .conn
                .execute(
                    "UPDATE experiments SET created_at = ?2 WHERE id = ?1",
                    params![experiment.as_str(), 1000 + n as i64],
                )
                .unwrap();
            recorded.push(experiment);
        }
        let all: Vec<_> = store
            .experiments(None, 10)
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(
            all,
            vec![
                recorded[2].clone(),
                recorded[1].clone(),
                recorded[0].clone()
            ]
        );
        let only_a: Vec<_> = store
            .experiments(Some(&a), 10)
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(only_a, vec![recorded[2].clone(), recorded[0].clone()]);
        assert_eq!(store.experiments(None, 1).unwrap().len(), 1);
        assert!(is_invalid(store.experiments(None, 0)));
        assert!(is_invalid(store.experiments(None, MAX_EXPERIMENT_LIST + 1)));
        assert_eq!(store.experiment(&ExperimentId::new()).unwrap(), None);
    }
}
