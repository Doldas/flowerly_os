#![forbid(unsafe_code)]

//! The guest's hash-chained boot ledger (ADR-0023 R1, R2) and what it proves
//! about a pending activation.
//!
//! Each boot appends a `witness` record (slot, root and baked identity) before
//! `flowerlyd` starts; the health gate appends a `health` record for the same
//! boot after the service-account IPC Ping and before blessing. Every record
//! names the SHA-256 of the previous line, so the reader refuses a gap, a
//! reorder, a rewrite or a truncation instead of guessing. The decision only
//! reads: it never trusts a caller, the slot tool or the boot entries.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const LEDGER_SCHEMA: &str = "flowerly-generation-boot/v2";
/// The head of an empty ledger.
pub const ZERO_HEAD: &str = "0000000000000000000000000000000000000000000000000000000000000000";
pub const MAX_LEDGER: u64 = 2 * 1024 * 1024;
pub const MAX_RECORD: usize = 2048;

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("no witness ledger")]
    Missing,
    #[error("witness ledger refused: {0}")]
    Refused(String),
    #[error("reading the witness ledger: {0}")]
    Io(#[from] std::io::Error),
}

/// Who must own the ledger file and which group may read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerPolicy {
    pub owner_uid: u32,
    pub reader_gid: u32,
}

/// One witnessed boot and whether its health gate passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootRecord {
    pub sequence: u64,
    pub boot_id: String,
    pub slot: String,
    pub root_partuuid: String,
    pub loader_entry: String,
    pub source_commit: String,
    pub input_manifest_sha256: String,
    pub build_id: String,
    pub healthy: bool,
}

/// A verified ledger: every record chained from the zero head.
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    boots: Vec<BootRecord>,
    hashes: Vec<String>,
}

impl Ledger {
    #[must_use]
    pub fn last_sequence(&self) -> u64 {
        self.hashes.len() as u64
    }

    /// SHA-256 of the newest record line, or [`ZERO_HEAD`].
    #[must_use]
    pub fn head(&self) -> &str {
        self.hashes.last().map_or(ZERO_HEAD, String::as_str)
    }

    /// The head as it was after record `sequence` (0 is the empty ledger).
    #[must_use]
    pub fn head_at(&self, sequence: u64) -> Option<&str> {
        match sequence {
            0 => Some(ZERO_HEAD),
            n => self
                .hashes
                .get(usize::try_from(n - 1).ok()?)
                .map(String::as_str),
        }
    }

    #[must_use]
    pub fn witnesses(&self) -> &[BootRecord] {
        &self.boots
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecord {
    schema: String,
    kind: String,
    sequence: u64,
    prev_sha256: String,
    boot_id: String,
    #[serde(default)]
    slot: Option<String>,
    #[serde(default)]
    root_partuuid: Option<String>,
    #[serde(default)]
    loader_entry: Option<String>,
    #[serde(default)]
    source_commit: Option<String>,
    #[serde(default)]
    input_manifest_sha256: Option<String>,
    #[serde(default)]
    build_id: Option<String>,
    /// Present (possibly null) on a witness record, absent on a health record.
    #[serde(default, deserialize_with = "present")]
    user_data_sha256: Option<Option<String>>,
    #[serde(default)]
    status: Option<String>,
}

fn present<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                matches!(b, b'0'..=b'9' | b'a'..=b'f')
            }
        })
}

fn is_counter(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 2
        && text.bytes().all(|b| b.is_ascii_digit())
        && !(text.len() == 2 && text.starts_with('0'))
}

/// `flowerly-<letter>[+left[-done]][.conf]`, the systemd-boot counting names.
fn loader_slot(entry: &str) -> Option<char> {
    let rest = entry.strip_prefix("flowerly-")?;
    let rest = rest.strip_suffix(".conf").unwrap_or(rest);
    let mut chars = rest.chars();
    let letter = chars.next().filter(|c| matches!(c, 'a' | 'b'))?;
    let counter = chars.as_str();
    if counter.is_empty() {
        return Some(letter);
    }
    let counter = counter.strip_prefix('+')?;
    let ok = match counter.split_once('-') {
        Some((left, done)) => is_counter(left) && is_counter(done),
        None => is_counter(counter),
    };
    ok.then_some(letter)
}

/// Verify a whole ledger. Nothing partial is ever returned.
pub fn parse_ledger(bytes: &[u8]) -> Result<Ledger, LedgerError> {
    let refuse = |n: usize, why: String| LedgerError::Refused(format!("record {n}: {why}"));
    if bytes.len() as u64 > MAX_LEDGER {
        return Err(LedgerError::Refused("larger than 2 MiB".into()));
    }
    let mut ledger = Ledger::default();
    if bytes.is_empty() {
        return Ok(ledger);
    }
    let Some(body) = bytes.strip_suffix(b"\n") else {
        return Err(LedgerError::Refused(
            "the last record is truncated (power loss?)".into(),
        ));
    };
    for (index, line) in body.split(|b| *b == b'\n').enumerate() {
        let n = index + 1;
        if line.is_empty() {
            return Err(refuse(n, "empty record".into()));
        }
        if line.len() > MAX_RECORD {
            return Err(refuse(n, "record too long".into()));
        }
        let raw: RawRecord = serde_json::from_slice(line).map_err(|e| refuse(n, e.to_string()))?;
        if raw.schema != LEDGER_SCHEMA {
            return Err(refuse(n, format!("unknown schema {}", raw.schema)));
        }
        if raw.sequence != n as u64 {
            return Err(refuse(
                n,
                format!("sequence {} where {n} was expected", raw.sequence),
            ));
        }
        if raw.prev_sha256 != ledger.head() {
            return Err(refuse(n, "does not continue the chain".into()));
        }
        if !is_uuid(&raw.boot_id) {
            return Err(refuse(n, "invalid boot_id".into()));
        }
        match raw.kind.as_str() {
            "witness" => {
                let boot = witness_record(&raw).map_err(|why| refuse(n, why))?;
                if ledger.boots.iter().any(|b| b.boot_id == boot.boot_id) {
                    return Err(refuse(n, "duplicate boot_id".into()));
                }
                ledger.boots.push(boot);
            }
            "health" => {
                for (field, set) in [
                    ("slot", raw.slot.is_some()),
                    ("root_partuuid", raw.root_partuuid.is_some()),
                    ("loader_entry", raw.loader_entry.is_some()),
                    ("source_commit", raw.source_commit.is_some()),
                    ("input_manifest_sha256", raw.input_manifest_sha256.is_some()),
                    ("build_id", raw.build_id.is_some()),
                    ("user_data_sha256", raw.user_data_sha256.is_some()),
                ] {
                    if set {
                        return Err(refuse(n, format!("a health record carries {field}")));
                    }
                }
                if raw.status.as_deref() != Some("PASSED") {
                    return Err(refuse(n, "health status must be PASSED".into()));
                }
                match ledger.boots.last_mut() {
                    Some(boot) if boot.boot_id == raw.boot_id && !boot.healthy => {
                        boot.healthy = true;
                    }
                    Some(boot) if boot.boot_id == raw.boot_id => {
                        return Err(refuse(n, "second health record for this boot".into()));
                    }
                    _ => {
                        return Err(refuse(
                            n,
                            "health record does not follow the witness of its boot".into(),
                        ));
                    }
                }
            }
            other => return Err(refuse(n, format!("unknown kind {other}"))),
        }
        ledger.hashes.push(sha256_hex(line));
    }
    Ok(ledger)
}

fn witness_record(raw: &RawRecord) -> Result<BootRecord, String> {
    let need = |field: &str, value: &Option<String>| {
        value
            .clone()
            .ok_or_else(|| format!("a witness record lacks {field}"))
    };
    if raw.status.is_some() {
        return Err("a witness record carries status".into());
    }
    let slot = need("slot", &raw.slot)?;
    let letter = match slot.as_str() {
        "root-a" => 'a',
        "root-b" => 'b',
        _ => return Err("invalid slot".into()),
    };
    let loader_entry = need("loader_entry", &raw.loader_entry)?;
    if loader_slot(&loader_entry) != Some(letter) {
        return Err("loader entry does not match the slot".into());
    }
    let root_partuuid = need("root_partuuid", &raw.root_partuuid)?;
    if !is_uuid(&root_partuuid) {
        return Err("invalid root_partuuid".into());
    }
    let source_commit = need("source_commit", &raw.source_commit)?;
    if !is_hex(&source_commit, 40) {
        return Err("invalid source_commit".into());
    }
    let input_manifest_sha256 = need("input_manifest_sha256", &raw.input_manifest_sha256)?;
    if !is_hex(&input_manifest_sha256, 64) {
        return Err("invalid input_manifest_sha256".into());
    }
    let build_id = need("build_id", &raw.build_id)?;
    if build_id.is_empty()
        || build_id.len() > 64
        || !build_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    {
        return Err("invalid build_id".into());
    }
    match &raw.user_data_sha256 {
        None => return Err("a witness record lacks user_data_sha256".into()),
        Some(Some(hash)) if !is_hex(hash, 64) => return Err("invalid user_data_sha256".into()),
        Some(_) => {}
    }
    Ok(BootRecord {
        sequence: raw.sequence,
        boot_id: raw.boot_id.clone(),
        slot,
        root_partuuid,
        loader_entry,
        source_commit,
        input_manifest_sha256,
        build_id,
        healthy: false,
    })
}

/// Read and verify the ledger file: a regular file (never a symlink), owned
/// as the policy says, writable only by its owner, readable at most by the
/// reader group, at most 2 MiB.
pub fn read_ledger(path: &Path, policy: &LedgerPolicy) -> Result<Ledger, LedgerError> {
    let link = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(LedgerError::Missing),
        Err(e) => return Err(e.into()),
    };
    if !link.file_type().is_file() {
        return Err(LedgerError::Refused("not a regular file".into()));
    }
    let file = File::open(path)?;
    let meta = file.metadata()?;
    if meta.dev() != link.dev() || meta.ino() != link.ino() {
        return Err(LedgerError::Refused("replaced while it was opened".into()));
    }
    if meta.uid() != policy.owner_uid {
        return Err(LedgerError::Refused(format!(
            "owner uid {} is not {}",
            meta.uid(),
            policy.owner_uid
        )));
    }
    let mode = meta.mode() & 0o7777;
    if mode & 0o7137 != 0 {
        return Err(LedgerError::Refused(format!(
            "mode {mode:o} allows more than owner write and group read"
        )));
    }
    if mode & 0o040 != 0 && meta.gid() != policy.reader_gid && meta.gid() != 0 {
        return Err(LedgerError::Refused(format!(
            "group {} may read it",
            meta.gid()
        )));
    }
    if meta.len() > MAX_LEDGER {
        return Err(LedgerError::Refused("larger than 2 MiB".into()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_LEDGER + 1).read_to_end(&mut bytes)?;
    parse_ledger(&bytes)
}

/// What a pending activation expects to see witnessed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expectation {
    pub slot: String,
    pub root_partuuid: String,
    pub source_commit: String,
    pub input_manifest_sha256: String,
    pub build_id: String,
    /// Ledger position and head recorded with the request.
    pub ledger_sequence: u64,
    pub ledger_head: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing to decide yet; try again after the next boot or health record.
    Wait(String),
    Confirm {
        boot_id: String,
        sequence: u64,
        detail: String,
    },
    FellBack {
        boot_id: String,
        sequence: u64,
        detail: String,
    },
    /// The target slot booted something else: the activation cannot succeed.
    Abandon(String),
    /// The ledger cannot prove anything about this request; leave it pending.
    Unresolved(String),
}

/// Decide a pending activation from the verified ledger and this kernel's
/// boot id. Only records after the request position, chained from the head
/// seen at request time, count; the newest must be the live boot.
#[must_use]
pub fn decide(ledger: &Ledger, expectation: &Expectation, live_boot_id: &str) -> Decision {
    if ledger.last_sequence() < expectation.ledger_sequence {
        return Decision::Unresolved(
            "the ledger is shorter than at request time: it was rolled back or replaced".into(),
        );
    }
    if ledger.head_at(expectation.ledger_sequence) != Some(expectation.ledger_head.as_str()) {
        return Decision::Unresolved(
            "the ledger does not continue the chain seen at request time".into(),
        );
    }
    let after: Vec<&BootRecord> = ledger
        .boots
        .iter()
        .filter(|boot| boot.sequence > expectation.ledger_sequence)
        .collect();
    let Some(newest) = after.last() else {
        return Decision::Wait("no boot has been witnessed since the request".into());
    };
    if newest.boot_id != live_boot_id {
        return Decision::Unresolved(format!(
            "the newest witnessed boot {} is not this boot {live_boot_id}",
            newest.boot_id
        ));
    }
    match after.iter().position(|boot| boot.slot == expectation.slot) {
        Some(first) => {
            if let Some(back) = after[first..]
                .iter()
                .find(|boot| boot.slot != expectation.slot)
            {
                return Decision::FellBack {
                    boot_id: back.boot_id.clone(),
                    sequence: back.sequence,
                    detail: format!(
                        "boot {} ran {} after {} was tried",
                        back.boot_id, back.slot, expectation.slot
                    ),
                };
            }
        }
        None => {
            return Decision::FellBack {
                boot_id: newest.boot_id.clone(),
                sequence: newest.sequence,
                detail: format!(
                    "{} never reached its witness; {} came back",
                    expectation.slot, newest.slot
                ),
            };
        }
    }
    for (field, booted, wanted) in [
        (
            "root_partuuid",
            &newest.root_partuuid,
            &expectation.root_partuuid,
        ),
        (
            "source_commit",
            &newest.source_commit,
            &expectation.source_commit,
        ),
        (
            "input_manifest_sha256",
            &newest.input_manifest_sha256,
            &expectation.input_manifest_sha256,
        ),
        ("build_id", &newest.build_id, &expectation.build_id),
    ] {
        if booted != wanted {
            return Decision::Abandon(format!(
                "the booted {field} {booted} is not the Generation's {wanted}"
            ));
        }
    }
    if !newest.healthy {
        return Decision::Wait("this boot has not passed the health gate yet".into());
    }
    Decision::Confirm {
        boot_id: newest.boot_id.clone(),
        sequence: newest.sequence,
        detail: format!(
            "boot {} of {} passed the health gate",
            newest.boot_id, expectation.slot
        ),
    }
}

/// Correctly chained ledgers for tests, which then damage them on purpose.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub(crate) const BOOT_1: &str = "aaaaaaaa-0000-0000-0000-000000000001";
    pub(crate) const BOOT_2: &str = "aaaaaaaa-0000-0000-0000-000000000002";
    pub(crate) const BOOT_3: &str = "aaaaaaaa-0000-0000-0000-000000000003";
    pub(crate) const BOOT_4: &str = "aaaaaaaa-0000-0000-0000-000000000004";
    pub(crate) const PART_A: &str = "11111111-1111-1111-1111-111111111111";
    pub(crate) const PART_B: &str = "11111111-2222-3333-4444-555555555555";
    pub(crate) const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    pub(crate) fn sha(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Builds a correctly chained ledger; tests then damage it on purpose.
    #[derive(Default)]
    pub(crate) struct Chain {
        pub(crate) lines: Vec<String>,
    }

    impl Chain {
        pub(crate) fn head(&self) -> String {
            self.lines
                .last()
                .map_or_else(|| ZERO_HEAD.to_owned(), |line| sha(line.as_bytes()))
        }

        pub(crate) fn push(&mut self, mut record: serde_json::Value) -> &mut Self {
            record["schema"] = LEDGER_SCHEMA.into();
            record["sequence"] = (self.lines.len() as u64 + 1).into();
            record["prev_sha256"] = self.head().into();
            self.lines.push(record.to_string());
            self
        }

        pub(crate) fn witness(&mut self, boot: &str, slot: &str) -> &mut Self {
            let (letter, part) = if slot == "root-a" {
                ("a", PART_A)
            } else {
                ("b", PART_B)
            };
            self.push(witness_json(boot, slot, letter, part))
        }

        pub(crate) fn health(&mut self, boot: &str) -> &mut Self {
            self.push(serde_json::json!({"kind": "health", "boot_id": boot, "status": "PASSED"}))
        }

        pub(crate) fn bytes(&self) -> Vec<u8> {
            self.lines
                .iter()
                .map(|l| format!("{l}\n"))
                .collect::<String>()
                .into_bytes()
        }

        pub(crate) fn parse(&self) -> Ledger {
            parse_ledger(&self.bytes()).expect("valid ledger")
        }
    }

    pub(crate) fn witness_json(
        boot: &str,
        slot: &str,
        letter: &str,
        part: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "kind": "witness",
            "boot_id": boot,
            "slot": slot,
            "root_partuuid": part,
            "loader_entry": format!("flowerly-{letter}+2-1.conf"),
            "source_commit": COMMIT,
            "input_manifest_sha256": "d".repeat(64),
            "build_id": "build-1",
            "user_data_sha256": "e".repeat(64),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn refused(bytes: &[u8], needle: &str) {
        let error = parse_ledger(bytes).expect_err(needle).to_string();
        assert!(error.contains(needle), "{error} should contain {needle}");
    }

    // --- reader -----------------------------------------------------------

    #[test]
    fn an_empty_ledger_has_the_zero_head() {
        let ledger = parse_ledger(b"").unwrap();
        assert_eq!(ledger.last_sequence(), 0);
        assert_eq!(ledger.head(), ZERO_HEAD);
    }

    #[test]
    fn a_chained_ledger_is_read_with_its_head() {
        let mut chain = Chain::default();
        chain
            .witness(BOOT_1, "root-a")
            .health(BOOT_1)
            .witness(BOOT_2, "root-b");
        let ledger = chain.parse();
        assert_eq!(ledger.last_sequence(), 3);
        assert_eq!(ledger.head(), chain.head());
        assert_eq!(ledger.head_at(1).unwrap(), sha(chain.lines[0].as_bytes()));
        let boots = ledger.witnesses();
        assert_eq!(boots.len(), 2);
        assert!(boots[0].healthy && !boots[1].healthy);
        assert_eq!((boots[1].sequence, boots[1].slot.as_str()), (3, "root-b"));
    }

    #[test]
    fn a_torn_or_padded_ledger_is_refused() {
        let mut chain = Chain::default();
        chain.witness(BOOT_1, "root-a");
        let bytes = chain.bytes();
        refused(&bytes[..bytes.len() - 1], "truncated");
        refused(&bytes[..bytes.len() / 2], "truncated");
        let mut padded = bytes.clone();
        padded.extend_from_slice(b"\n");
        refused(&padded, "empty record");
    }

    #[test]
    fn a_broken_chain_is_refused() {
        let mut chain = Chain::default();
        chain
            .witness(BOOT_1, "root-a")
            .health(BOOT_1)
            .witness(BOOT_2, "root-b");
        // Tampering with an earlier record breaks the next record's link.
        let mut tampered = chain.lines.clone();
        tampered[0] = tampered[0].replace("build-1", "build-9");
        refused(
            &tampered
                .iter()
                .map(|l| format!("{l}\n"))
                .collect::<String>()
                .into_bytes(),
            "chain",
        );
        // Reordered records.
        let mut reordered = chain.lines.clone();
        reordered.swap(1, 2);
        refused(
            &reordered
                .iter()
                .map(|l| format!("{l}\n"))
                .collect::<String>()
                .into_bytes(),
            "sequence",
        );
        // A record removed from the middle.
        let gap = [chain.lines[0].clone(), chain.lines[2].clone()];
        refused(
            &gap.iter()
                .map(|l| format!("{l}\n"))
                .collect::<String>()
                .into_bytes(),
            "sequence",
        );
    }

    #[test]
    fn a_chain_must_start_at_one_from_the_zero_head() {
        let mut chain = Chain::default();
        chain.witness(BOOT_1, "root-a").witness(BOOT_2, "root-a");
        // Dropping the first record leaves a valid-looking tail.
        refused(format!("{}\n", chain.lines[1]).as_bytes(), "sequence");
    }

    #[test]
    fn a_replayed_boot_or_a_second_health_record_is_refused() {
        let mut chain = Chain::default();
        chain.witness(BOOT_1, "root-a").witness(BOOT_1, "root-a");
        refused(&chain.bytes(), "duplicate boot");
        let mut chain = Chain::default();
        chain
            .witness(BOOT_1, "root-a")
            .health(BOOT_1)
            .health(BOOT_1);
        refused(&chain.bytes(), "health");
    }

    #[test]
    fn health_must_belong_to_the_newest_witnessed_boot() {
        let mut chain = Chain::default();
        chain.health(BOOT_1);
        refused(&chain.bytes(), "health");
        let mut chain = Chain::default();
        chain
            .witness(BOOT_1, "root-a")
            .witness(BOOT_2, "root-b")
            .health(BOOT_1);
        refused(&chain.bytes(), "health");
    }

    #[test]
    fn malformed_records_are_refused() {
        let cases: Vec<(serde_json::Value, &str)> = vec![
            (
                {
                    let mut w = witness_json(BOOT_1, "root-a", "a", PART_A);
                    w["extra"] = 1.into();
                    w
                },
                "unknown field",
            ),
            (
                {
                    let mut w = witness_json(BOOT_1, "root-a", "a", PART_A);
                    w.as_object_mut().unwrap().remove("build_id");
                    w
                },
                "build_id",
            ),
            (
                {
                    let mut w = witness_json(BOOT_1, "root-a", "a", PART_A);
                    w["kind"] = "boot".into();
                    w
                },
                "kind",
            ),
            (
                {
                    let mut w = witness_json(BOOT_1, "root-a", "a", PART_A);
                    w["status"] = "PASSED".into();
                    w
                },
                "status",
            ),
            (witness_json(BOOT_1, "root-a", "b", PART_A), "loader entry"),
            (witness_json(BOOT_1, "root-c", "a", PART_A), "slot"),
            (
                witness_json(
                    "AAAAAAAA-0000-0000-0000-000000000001",
                    "root-a",
                    "a",
                    PART_A,
                ),
                "boot_id",
            ),
            (
                witness_json(BOOT_1, "root-a", "a", "not-a-uuid"),
                "root_partuuid",
            ),
            (
                {
                    let mut w = witness_json(BOOT_1, "root-a", "a", PART_A);
                    w["source_commit"] = "abc".into();
                    w
                },
                "source_commit",
            ),
            (
                {
                    let mut w = witness_json(BOOT_1, "root-a", "a", PART_A);
                    w["build_id"] = "a b".into();
                    w
                },
                "build_id",
            ),
            (
                {
                    let mut w = witness_json(BOOT_1, "root-a", "a", PART_A);
                    w["user_data_sha256"] = "x".into();
                    w
                },
                "user_data_sha256",
            ),
            (
                serde_json::json!({"kind": "health", "boot_id": BOOT_1, "status": "FAILED"}),
                "status",
            ),
            (
                serde_json::json!({"kind": "health", "boot_id": BOOT_1, "status": "PASSED", "slot": "root-a"}),
                "slot",
            ),
        ];
        for (record, needle) in cases {
            let mut chain = Chain::default();
            chain.push(record);
            refused(&chain.bytes(), needle);
        }
        refused(b"not json\n", "record 1");
        refused(b"\xff\xfe\n", "record 1");
        let mut chain = Chain::default();
        chain.witness(BOOT_1, "root-a");
        let v1 = chain.lines[0].replace(LEDGER_SCHEMA, "flowerly-generation-boot/v1");
        refused(format!("{v1}\n").as_bytes(), "schema");
        let mut big = witness_json(BOOT_1, "root-a", "a", PART_A);
        big["build_id"] = "b".repeat(MAX_RECORD).into();
        let mut chain = Chain::default();
        chain.push(big);
        refused(&chain.bytes(), "too long");
    }

    // --- file checks --------------------------------------------------------

    fn own_policy() -> LedgerPolicy {
        let meta = std::fs::metadata(".").unwrap();
        LedgerPolicy {
            owner_uid: std::os::unix::fs::MetadataExt::uid(&meta),
            reader_gid: std::os::unix::fs::MetadataExt::gid(&meta),
        }
    }

    fn write_ledger(dir: &std::path::Path, mode: u32) -> std::path::PathBuf {
        let path = dir.join("boot-witness.jsonl");
        let mut chain = Chain::default();
        chain.witness(BOOT_1, "root-a");
        std::fs::write(&path, chain.bytes()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn the_file_must_be_owned_and_protected_as_the_policy_says() {
        let dir = tempfile::tempdir().unwrap();
        let file_meta = |p: &std::path::Path| std::fs::metadata(p).unwrap();
        let path = write_ledger(dir.path(), 0o640);
        let meta = file_meta(&path);
        let policy = LedgerPolicy {
            owner_uid: std::os::unix::fs::MetadataExt::uid(&meta),
            reader_gid: std::os::unix::fs::MetadataExt::gid(&meta),
        };
        assert_eq!(read_ledger(&path, &policy).unwrap().last_sequence(), 1);

        let other_owner = LedgerPolicy {
            owner_uid: policy.owner_uid + 1,
            ..policy
        };
        assert!(read_ledger(&path, &other_owner)
            .unwrap_err()
            .to_string()
            .contains("owner"));
        for mode in [0o660, 0o644, 0o604, 0o4640] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            let error = read_ledger(&path, &policy).unwrap_err().to_string();
            assert!(error.contains("mode"), "{mode:o}: {error}");
        }
        // Group-readable only by the expected reader group (or root).
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let other_group = LedgerPolicy {
            reader_gid: policy.reader_gid + 1,
            ..policy
        };
        if policy.reader_gid != 0 {
            assert!(read_ledger(&path, &other_group)
                .unwrap_err()
                .to_string()
                .contains("group"));
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_ledger(&path, &other_group).is_ok());
    }

    #[test]
    fn a_symlink_directory_missing_or_oversized_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let policy = own_policy();
        let path = write_ledger(dir.path(), 0o600);
        let link = dir.path().join("link.jsonl");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_ledger(&link, &policy)
            .unwrap_err()
            .to_string()
            .contains("regular"));
        assert!(read_ledger(dir.path(), &policy)
            .unwrap_err()
            .to_string()
            .contains("regular"));
        assert!(matches!(
            read_ledger(&dir.path().join("missing"), &policy),
            Err(LedgerError::Missing)
        ));
        let big = dir.path().join("big.jsonl");
        std::fs::write(&big, vec![b'\n'; MAX_LEDGER as usize + 1]).unwrap();
        std::fs::set_permissions(&big, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_ledger(&big, &policy)
            .unwrap_err()
            .to_string()
            .contains("2 MiB"));
    }

    // --- decision -----------------------------------------------------------

    fn expect_b(at: &Ledger) -> Expectation {
        Expectation {
            slot: "root-b".to_owned(),
            root_partuuid: PART_B.to_owned(),
            source_commit: COMMIT.to_owned(),
            input_manifest_sha256: "d".repeat(64),
            build_id: "build-1".to_owned(),
            ledger_sequence: at.last_sequence(),
            ledger_head: at.head().to_owned(),
        }
    }

    /// The ledger at request time: slot A booted and healthy.
    fn requested() -> (Chain, Expectation) {
        let mut chain = Chain::default();
        chain.witness(BOOT_1, "root-a").health(BOOT_1);
        let expectation = expect_b(&chain.parse());
        (chain, expectation)
    }

    #[test]
    fn no_boot_since_the_request_waits() {
        let (chain, exp) = requested();
        assert!(matches!(
            decide(&chain.parse(), &exp, BOOT_1),
            Decision::Wait(_)
        ));
    }

    #[test]
    fn a_healthy_target_boot_confirms() {
        let (mut chain, exp) = requested();
        chain.witness(BOOT_2, "root-b").health(BOOT_2);
        match decide(&chain.parse(), &exp, BOOT_2) {
            Decision::Confirm {
                boot_id, sequence, ..
            } => assert_eq!((boot_id.as_str(), sequence), (BOOT_2, 3)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_target_boot_without_health_waits() {
        let (mut chain, exp) = requested();
        chain.witness(BOOT_2, "root-b");
        assert!(matches!(
            decide(&chain.parse(), &exp, BOOT_2),
            Decision::Wait(_)
        ));
    }

    #[test]
    fn counting_boots_of_the_target_then_health_confirm() {
        let (mut chain, exp) = requested();
        chain
            .witness(BOOT_2, "root-b")
            .witness(BOOT_3, "root-b")
            .health(BOOT_3);
        assert!(matches!(
            decide(&chain.parse(), &exp, BOOT_3),
            Decision::Confirm { .. }
        ));
    }

    #[test]
    fn the_previous_slot_after_the_target_is_a_fall_back() {
        let (mut chain, exp) = requested();
        chain
            .witness(BOOT_2, "root-b")
            .witness(BOOT_3, "root-a")
            .health(BOOT_3);
        match decide(&chain.parse(), &exp, BOOT_3) {
            Decision::FellBack {
                boot_id, sequence, ..
            } => assert_eq!((boot_id.as_str(), sequence), (BOOT_3, 4)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_fall_back_is_not_noise_even_if_the_target_comes_back() {
        let (mut chain, exp) = requested();
        chain
            .witness(BOOT_2, "root-b")
            .witness(BOOT_3, "root-a")
            .witness(BOOT_4, "root-b")
            .health(BOOT_4);
        match decide(&chain.parse(), &exp, BOOT_4) {
            Decision::FellBack { sequence, .. } => assert_eq!(sequence, 4),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_target_that_never_reached_its_witness_falls_back() {
        let (mut chain, exp) = requested();
        chain.witness(BOOT_2, "root-a");
        assert!(matches!(
            decide(&chain.parse(), &exp, BOOT_2),
            Decision::FellBack { .. }
        ));
    }

    #[test]
    fn a_copied_or_stale_ledger_cannot_resolve() {
        let (mut chain, exp) = requested();
        chain.witness(BOOT_2, "root-b").health(BOOT_2);
        // The live kernel is a different boot than the newest record.
        assert!(matches!(
            decide(&chain.parse(), &exp, BOOT_3),
            Decision::Unresolved(_)
        ));
    }

    #[test]
    fn a_rolled_back_or_replaced_ledger_cannot_resolve() {
        let (chain, exp) = requested();
        // Shorter than at request time.
        let mut short = Chain::default();
        short.witness(BOOT_1, "root-a");
        assert!(matches!(
            decide(&short.parse(), &exp, BOOT_1),
            Decision::Unresolved(_)
        ));
        // Same length, different history: the head no longer matches.
        let mut other = Chain::default();
        other
            .witness(BOOT_3, "root-a")
            .health(BOOT_3)
            .witness(BOOT_2, "root-b")
            .health(BOOT_2);
        assert!(matches!(
            decide(&other.parse(), &exp, BOOT_2),
            Decision::Unresolved(_)
        ));
        let _ = chain;
    }

    #[test]
    fn a_target_boot_of_another_image_is_abandoned() {
        for field in [
            "root_partuuid",
            "source_commit",
            "input_manifest_sha256",
            "build_id",
        ] {
            let (mut chain, mut exp) = requested();
            chain.witness(BOOT_2, "root-b").health(BOOT_2);
            match field {
                "root_partuuid" => exp.root_partuuid = PART_A.to_owned(),
                "source_commit" => exp.source_commit = "f".repeat(40),
                "input_manifest_sha256" => exp.input_manifest_sha256 = "f".repeat(64),
                _ => exp.build_id = "build-2".to_owned(),
            }
            match decide(&chain.parse(), &exp, BOOT_2) {
                Decision::Abandon(detail) => assert!(detail.contains(field), "{detail}"),
                other => panic!("{field}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_request_on_an_empty_ledger_starts_from_the_zero_head() {
        let empty = Chain::default();
        let exp = expect_b(&parse_ledger(b"").unwrap());
        assert_eq!(exp.ledger_head, ZERO_HEAD);
        let mut chain = empty;
        chain.witness(BOOT_2, "root-b").health(BOOT_2);
        assert!(matches!(
            decide(&chain.parse(), &exp, BOOT_2),
            Decision::Confirm { .. }
        ));
    }
}
