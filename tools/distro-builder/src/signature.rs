//! Detached package signature checks against an explicitly supplied keyring
//! (#49, proposed ADR-0020).
//!
//! `gpgv` runs with an empty, throwaway home and only the given keyring, and
//! only its machine-readable status output is read. A signature is accepted
//! when it is one good binary-document signature, made by a key whose
//! primary fingerprint is the manifest's `signer_fingerprint`, with a SHA-2
//! or SHA-3 hash. Which keyring deserves trust is policy and is not decided
//! here; the report names the keyring by digest so that policy can.

use std::path::Path;
use std::process::Command;

/// Reported when every signature verified against the supplied keyring.
pub const VERIFIED: &str = "verified_against_supplied_keyring";

/// Status keywords that reject a verification whatever else is reported.
const REJECTING: &[&str] = &[
    "BADSIG",
    "ERRSIG",
    "EXPSIG",
    "EXPKEYSIG",
    "REVKEYSIG",
    "KEYEXPIRED",
    "KEYREVOKED",
    "SIGEXPIRED",
    "NO_PUBKEY",
    "FAILURE",
    "ERROR",
    "BADARMOR",
    "NODATA",
    "UNEXPECTED",
];

/// OpenPGP hash algorithm ids: SHA-256/384/512/224 and SHA3-256/512.
const ACCEPTED_HASHES: &[u32] = &[8, 9, 10, 11, 12, 14];

/// Problems with one gpgv run's status output against the expected signer
/// (empty means accepted). Pure, so every rule is unit-tested.
pub fn check_status(status: &str, expected_primary: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut records = Vec::new();
    for (number, line) in status.lines().enumerate() {
        let Some(rest) = line.strip_prefix("[GNUPG:] ") else {
            return vec![format!(
                "status line {} is not a GnuPG status line",
                number + 1
            )];
        };
        let mut parts = rest.split(' ');
        let keyword = parts.next().unwrap_or_default();
        records.push((keyword, parts.collect::<Vec<_>>()));
    }
    for (keyword, _) in &records {
        if REJECTING.contains(keyword) {
            problems.push(format!("gpgv reported {keyword}"));
        }
    }
    for wanted in ["NEWSIG", "GOODSIG", "VALIDSIG"] {
        let count = records.iter().filter(|(k, _)| *k == wanted).count();
        if count != 1 {
            problems.push(format!("expected exactly one {wanted}, got {count}"));
        }
    }
    let validsigs: Vec<_> = records.iter().filter(|(k, _)| *k == "VALIDSIG").collect();
    if let [(_, args)] = validsigs.as_slice() {
        problems.extend(check_validsig(args, expected_primary));
        let signer = args.first().copied().unwrap_or_default();
        let goodsig_matches = records.iter().any(|(k, a)| {
            *k == "GOODSIG"
                && a.first()
                    .is_some_and(|id| id.len() == 16 && signer.ends_with(&id.to_uppercase()))
        });
        if !goodsig_matches {
            problems.push("GOODSIG key id does not match the VALIDSIG signer".into());
        }
    }
    problems
}

/// VALIDSIG <fpr> <date> <sig-ts> <expire-ts> <version> <reserved>
/// <pubkey-algo> <hash-algo> <sig-class> <primary-fpr>
fn check_validsig(args: &[&str], expected_primary: &str) -> Vec<String> {
    let [signer, _date, _created, _expires, _version, _reserved, _pk, hash, class, primary] = args
    else {
        return vec![format!("VALIDSIG has {} fields, expected 10", args.len())];
    };
    let mut problems = Vec::new();
    let fingerprint = |f: &str| f.len() == 40 && f.bytes().all(|b| b.is_ascii_hexdigit());
    if !fingerprint(signer) || !fingerprint(primary) {
        problems.push("VALIDSIG fingerprints are malformed".into());
    }
    if !primary.eq_ignore_ascii_case(expected_primary) {
        problems.push(format!(
            "signed by primary key {primary}, manifest names {expected_primary}"
        ));
    }
    if !hash
        .parse::<u32>()
        .is_ok_and(|id| ACCEPTED_HASHES.contains(&id))
    {
        problems.push(format!("signature hash algorithm {hash} is not accepted"));
    }
    if *class != "00" {
        problems.push(format!(
            "signature class {class} is not a binary document signature"
        ));
    }
    problems
}

/// Runs gpgv on one signature/data pair with only `keyring` available and
/// returns the problems (empty means accepted).
pub fn verify(
    keyring: &Path,
    signature: &Path,
    data: &Path,
    expected_primary: &str,
) -> Vec<String> {
    let home = match tempfile::Builder::new().prefix("flowerly-gpgv-").tempdir() {
        Ok(home) => home,
        Err(error) => return vec![format!("cannot create a gpgv home: {error}")],
    };
    let output = Command::new("gpgv")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .arg("--homedir")
        .arg(home.path())
        .arg("--keyring")
        .arg(keyring)
        .args(["--status-fd", "1", "--"])
        .arg(signature)
        .arg(data)
        .output();
    let output = match output {
        Ok(output) => output,
        Err(error) => return vec![format!("gpgv could not run: {error}")],
    };
    let mut problems = check_status(&String::from_utf8_lossy(&output.stdout), expected_primary);
    if !output.status.success() {
        problems.push(format!("gpgv exited with {}", output.status));
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGNER: &str = "AAAABBBBCCCCDDDDEEEEFFFF0000111122223333";
    const PRIMARY: &str = "0123456789ABCDEF0123456789ABCDEF01234567";

    fn status(validsig: &str) -> String {
        format!(
            "[GNUPG:] NEWSIG\n[GNUPG:] KEY_CONSIDERED {SIGNER} 0\n\
             [GNUPG:] GOODSIG 0000111122223333 Packager <p@example.org>\n\
             [GNUPG:] {validsig}\n"
        )
    }

    fn validsig(hash: &str, class: &str, primary: &str) -> String {
        format!("VALIDSIG {SIGNER} 2026-10-30 1793318400 0 4 0 22 {hash} {class} {primary}")
    }

    #[test]
    fn one_good_binary_signature_by_the_named_primary_is_accepted() {
        assert!(check_status(&status(&validsig("10", "00", PRIMARY)), PRIMARY).is_empty());
    }

    #[test]
    fn wrong_signer_hash_or_class_is_rejected() {
        let other = "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF";
        let cases = [
            (validsig("10", "00", other), "manifest names"),
            (validsig("2", "00", PRIMARY), "hash algorithm 2"),
            (validsig("8", "01", PRIMARY), "signature class 01"),
            (validsig("8", "00", "short"), "malformed"),
            ("VALIDSIG a b".to_owned(), "expected 10"),
        ];
        for (line, expected) in cases {
            let problems = check_status(&status(&line), PRIMARY).join("\n");
            assert!(problems.contains(expected), "{expected}: {problems}");
        }
    }

    #[test]
    fn rejecting_keywords_missing_or_repeated_records_fail() {
        let good = status(&validsig("8", "00", PRIMARY));
        for keyword in [
            "EXPKEYSIG 0000111122223333 x",
            "REVKEYSIG x",
            "BADSIG x",
            "ERRSIG x",
        ] {
            let problems = check_status(&format!("{good}[GNUPG:] {keyword}\n"), PRIMARY);
            assert!(!problems.is_empty(), "{keyword} accepted");
        }
        let twice = format!("{good}{good}");
        assert!(check_status(&twice, PRIMARY)
            .join("\n")
            .contains("exactly one GOODSIG"));
        assert!(check_status("", PRIMARY)
            .join("\n")
            .contains("exactly one VALIDSIG"));
        let forged = good.replace("GOODSIG 0000111122223333", "GOODSIG 9999999999999999");
        assert!(check_status(&forged, PRIMARY)
            .join("\n")
            .contains("does not match"));
    }

    #[test]
    fn non_status_output_is_rejected() {
        let mixed = format!(
            "gpgv: Good signature\n{}",
            status(&validsig("8", "00", PRIMARY))
        );
        assert!(check_status(&mixed, PRIMARY)[0].contains("not a GnuPG status line"));
    }
}
