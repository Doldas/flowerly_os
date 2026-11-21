//! The in-guest probe's report as it arrives on the serial console
//! (ADR-0024 D4, the Gate D wire format):
//!
//! ```text
//! FLOWERLY_LAB_REPORT part=i/n <base64>
//! FLOWERLY_LAB_REPORT_DONE parts=n sha256=<hex of the JSON>
//! ```
//!
//! Console noise may surround a line. A part that differs from an earlier
//! copy, a missing part, a second DONE line, an oversized report or a digest
//! mismatch is refused, never repaired.

use std::collections::BTreeMap;

use base64::Engine;
use serde::{Deserialize, Serialize};

use super::sha256_hex;

pub const REPORT_SCHEMA: &str = "flowerly-lab-report/v1";
const PART: &str = "FLOWERLY_LAB_REPORT part=";
const DONE: &str = "FLOWERLY_LAB_REPORT_DONE parts=";
/// Decoded JSON bound; the probe's report is a few KiB.
pub const MAX_REPORT_BYTES: usize = 256 * 1024;
const MAX_PARTS: usize = 4096;

/// What the probe saw inside the guest. Every field the probe could not
/// determine is `null`; the host judges, the probe does not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeReport {
    pub schema: String,
    pub facts: Facts,
    pub observations: Observations,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Facts {
    /// `root-a` or `root-b`, as the booted root's PARTUUID resolves.
    pub slot: Option<String>,
    pub root_partuuid: Option<String>,
    /// SHA-256 of the baked identity document.
    pub identity_sha256: Option<String>,
    pub generation: Option<String>,
    pub kernel_release: Option<String>,
    pub vmlinuz_sha256: Option<String>,
    /// SHA-256 of the sorted `name version` lines of the installed packages.
    pub packages_sha256: Option<String>,
    pub package_count: Option<u64>,
    #[serde(default)]
    pub binaries: BTreeMap<String, String>,
    /// Guest boot phases from `systemd-analyze`, in milliseconds.
    #[serde(default)]
    pub boot_phases_ms: BTreeMap<String, f64>,
    #[serde(default)]
    pub failed_units: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observations {
    /// Result of the boot health gate: `passed`, `failed` or `absent`.
    pub health_gate: Option<String>,
    pub flowerlyd_active: Option<bool>,
    /// `agenticctl status` answered.
    pub status_ok: Option<bool>,
    /// An Admin write/read round trip through `agenticctl` succeeded.
    pub admin_roundtrip: Option<bool>,
    /// The Generation `agenticctl generation current` resolved.
    pub generation_resolved: Option<String>,
    pub evolutiond_self_test: Option<bool>,
}

/// A report that arrived intact: its decoded bytes and their digest.
#[derive(Debug, Clone, PartialEq)]
pub struct Received {
    pub report: ProbeReport,
    pub body: Vec<u8>,
    pub sha256: String,
}

/// The report in `text`, `Ok(None)` while its DONE line has not arrived.
pub fn parse(text: &str) -> Result<Option<Received>, String> {
    let mut done = None;
    let mut parts: BTreeMap<usize, (usize, &str)> = BTreeMap::new();
    for line in text.lines() {
        if let Some(at) = line.find(DONE) {
            let fields = done_fields(&line[at + DONE.len()..])
                .ok_or_else(|| "malformed report DONE line".to_owned())?;
            if done.replace(fields).is_some() {
                return Err("report DONE line seen more than once".to_owned());
            }
        } else if let Some(at) = line.find(PART) {
            let (index, total, chunk) = part_fields(&line[at + PART.len()..])
                .ok_or_else(|| "malformed report part line".to_owned())?;
            match parts.get(&index) {
                Some(&(seen_total, seen)) if seen_total != total || seen != chunk => {
                    return Err(format!("report part {index} differs between copies"));
                }
                Some(_) => {}
                None => {
                    parts.insert(index, (total, chunk));
                }
            }
        }
    }
    let Some((count, digest)) = done else {
        return Ok(None);
    };
    if count == 0 || count > MAX_PARTS {
        return Err(format!("report claims {count} parts"));
    }
    if parts.values().any(|&(total, _)| total != count) {
        return Err("report parts disagree with the DONE line".to_owned());
    }
    if parts.keys().copied().ne(1..=count) {
        return Err("report is missing parts".to_owned());
    }
    let encoded: String = parts.values().map(|&(_, chunk)| chunk).collect();
    if encoded.len() > MAX_REPORT_BYTES.div_ceil(3) * 4 {
        return Err("report is too large".to_owned());
    }
    let body = base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .map_err(|e| format!("report is not valid base64: {e}"))?;
    if body.len() > MAX_REPORT_BYTES {
        return Err("report is too large".to_owned());
    }
    let sha256 = sha256_hex(&body);
    if sha256 != digest {
        return Err("report does not match its digest".to_owned());
    }
    let report: ProbeReport =
        serde_json::from_slice(&body).map_err(|e| format!("report is not a v1 report: {e}"))?;
    if report.schema != REPORT_SCHEMA {
        return Err(format!("report schema is `{}`", report.schema));
    }
    Ok(Some(Received {
        report,
        body,
        sha256,
    }))
}

fn number(text: &str) -> Option<usize> {
    if text.is_empty() || text.len() > 6 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// `n sha256=<hex>`; trailing console noise after the digest is refused.
fn done_fields(rest: &str) -> Option<(usize, String)> {
    let (count, rest) = rest.split_once(" sha256=")?;
    let digest = rest.trim_end();
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    Some((number(count)?, digest.to_owned()))
}

/// `i/n <base64>`.
fn part_fields(rest: &str) -> Option<(usize, usize, &str)> {
    let (position, chunk) = rest.split_once(' ')?;
    let (index, total) = position.split_once('/')?;
    let (index, total) = (number(index)?, number(total)?);
    let chunk = chunk.trim_end();
    if index == 0
        || index > total
        || chunk.is_empty()
        || !chunk
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
    {
        return None;
    }
    Some((index, total, chunk))
}

/// The console lines that carry `body` (for tests; the guest probe has its
/// own copy in Python).
#[cfg(test)]
pub fn lines(body: &[u8], chunk: usize) -> Vec<String> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(body);
    let chunks: Vec<&str> = encoded
        .as_bytes()
        .chunks(chunk.max(4))
        .map(|c| std::str::from_utf8(c).expect("base64 is ASCII"))
        .collect();
    let mut out: Vec<String> = chunks
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{PART}{}/{} {c}", i + 1, chunks.len()))
        .collect();
    out.push(format!(
        "{DONE}{} sha256={}",
        chunks.len(),
        sha256_hex(body)
    ));
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sample() -> ProbeReport {
        ProbeReport {
            schema: REPORT_SCHEMA.to_owned(),
            facts: Facts {
                slot: Some("root-b".to_owned()),
                root_partuuid: Some("0f0e0d0c-0b0a-4908-8706-050403020100".to_owned()),
                identity_sha256: Some("1".repeat(64)),
                generation: Some("gen-candidate".to_owned()),
                kernel_release: Some("6.17.1-arch1-1".to_owned()),
                vmlinuz_sha256: Some("2".repeat(64)),
                packages_sha256: Some("3".repeat(64)),
                package_count: Some(212),
                binaries: BTreeMap::from([("flowerlyd".to_owned(), "4".repeat(64))]),
                boot_phases_ms: BTreeMap::from([
                    ("kernel".to_owned(), 1200.0),
                    ("userspace".to_owned(), 3400.5),
                ]),
                failed_units: Vec::new(),
            },
            observations: Observations {
                health_gate: Some("passed".to_owned()),
                flowerlyd_active: Some(true),
                status_ok: Some(true),
                admin_roundtrip: Some(true),
                generation_resolved: Some("gen-candidate".to_owned()),
                evolutiond_self_test: Some(true),
            },
        }
    }

    pub(crate) fn console(report: &ProbeReport) -> String {
        let body = serde_json::to_vec(report).unwrap();
        let mut text = String::from("[    3.1] systemd[1]: Started probe.\r\n");
        for line in lines(&body, 40) {
            text.push_str("\x1b[0m");
            text.push_str(&line);
            text.push_str("\r\n");
        }
        text
    }

    #[test]
    fn a_report_surrounded_by_console_noise_is_read() {
        let report = sample();
        let received = parse(&console(&report)).unwrap().unwrap();
        assert_eq!(received.report, report);
        assert_eq!(received.sha256, sha256_hex(&received.body));
    }

    #[test]
    fn no_done_line_means_not_yet() {
        let text = console(&sample());
        let partial: String = text
            .lines()
            .filter(|l| !l.contains("_DONE"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(parse(&partial).unwrap(), None);
        assert_eq!(parse("").unwrap(), None);
    }

    #[test]
    fn identical_copies_of_a_part_are_accepted() {
        let text = console(&sample());
        let first_part = text.lines().find(|l| l.contains("part=1/")).unwrap();
        let doubled = format!("{first_part}\n{text}");
        assert!(parse(&doubled).unwrap().is_some());
    }

    #[test]
    fn damaged_reports_are_refused() {
        let text = console(&sample());
        let part_two = text.lines().find(|l| l.contains("part=2/")).unwrap();
        let done = text.lines().find(|l| l.contains("_DONE")).unwrap();
        let last = if part_two.ends_with('A') { "B" } else { "A" };
        let altered = format!("{}{last}", &part_two[..part_two.len() - 1]);
        let cases: Vec<(&str, String)> = vec![
            ("missing part", text.replace(part_two, "")),
            ("differing copy", format!("{text}\n{altered}")),
            ("second done", format!("{text}\n{done}")),
            (
                "bad base64",
                text.replace(part_two, &format!("{part_two}!")),
            ),
            (
                "noise after digest",
                text.replace(done, &format!("{done} x")),
            ),
        ];
        for (label, case) in cases {
            assert!(parse(&case).is_err(), "{label} must be refused");
        }
    }

    #[test]
    fn digest_mismatch_and_wrong_schema_are_refused() {
        let body = serde_json::to_vec(&sample()).unwrap();
        let mut forged = lines(&body, 40);
        let last = forged.pop().unwrap();
        let (head, _) = last.split_once(" sha256=").unwrap();
        forged.push(format!("{head} sha256={}", "0".repeat(64)));
        assert!(parse(&forged.join("\n")).unwrap_err().contains("digest"));

        let mut other = sample();
        other.schema = "flowerly-lab-report/v0".to_owned();
        assert!(parse(&console(&other)).unwrap_err().contains("schema"));

        let mut value = serde_json::to_value(sample()).unwrap();
        value["facts"]["extra"] = serde_json::json!(1);
        let text = lines(&serde_json::to_vec(&value).unwrap(), 40).join("\n");
        assert!(parse(&text).unwrap_err().contains("v1 report"));
    }

    #[test]
    fn oversized_and_inconsistent_part_counts_are_refused() {
        let big = vec![b'x'; MAX_REPORT_BYTES + 1];
        assert!(parse(&lines(&big, 4096).join("\n"))
            .unwrap_err()
            .contains("too large"));
        let text = "FLOWERLY_LAB_REPORT part=1/2 eyJ9\nFLOWERLY_LAB_REPORT_DONE parts=1 sha256="
            .to_owned()
            + &"0".repeat(64);
        assert!(parse(&text).unwrap_err().contains("disagree"));
        let zero = format!("FLOWERLY_LAB_REPORT_DONE parts=0 sha256={}", "0".repeat(64));
        assert!(parse(&zero).is_err());
        assert!(parse("FLOWERLY_LAB_REPORT part=3/2 eyJ9").is_err());
        assert!(parse("FLOWERLY_LAB_REPORT_DONE parts=1 sha256=xyz").is_err());
    }
}
