//! Generation input manifest (#49, proposed ADR-0020).
//!
//! A manifest freezes the exact Arch package archives a candidate Generation
//! is built from. This module only checks the manifest's shape and canonical
//! form and verifies a local content-addressed cache against it. It does not
//! fetch anything and does not verify package signatures: a passing check
//! establishes byte identity, never publisher trust.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::signature;

pub const SCHEMA: &str = "flowerly.generation-inputs.v1";

/// Signature trust is reported explicitly so no caller can mistake a cache
/// hash match for a verified publisher signature.
pub const SIGNATURE_TRUST: &str = "not_verified";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: String,
    pub source: Source,
    pub requested: Vec<String>,
    pub packages: Vec<Package>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Dated repository snapshot root, e.g.
    /// `https://archive.archlinux.org/repos/2026/10/30/`.
    pub snapshot_url: String,
    /// `YYYY-MM-DD`; must match the date path in `snapshot_url`.
    pub snapshot_date: String,
    pub architecture: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub repository: String,
    pub name: String,
    pub version: String,
    pub architecture: String,
    pub filename: String,
    pub size: u64,
    pub sha256: String,
    pub signature_sha256: String,
    pub signer_fingerprint: String,
}

impl Package {
    fn sort_key(&self) -> (&str, &str, &str, &str, &str) {
        (
            &self.repository,
            &self.architecture,
            &self.name,
            &self.version,
            &self.sha256,
        )
    }
}

/// A manifest that passed [`check`], with the digest of its canonical bytes.
#[derive(Debug)]
pub struct Checked {
    pub manifest: Manifest,
    pub sha256: String,
}

/// Canonical bytes: pretty JSON with object keys sorted at every level plus
/// a final newline. A stored manifest must equal these bytes exactly, so its
/// file digest is the manifest identity and cannot vary with formatting or
/// with the field order of these Rust types.
pub fn canonical_bytes(manifest: &Manifest) -> Vec<u8> {
    let mut value = serde_json::to_value(manifest).expect("manifest serializes");
    sort_keys(&mut value);
    let mut bytes = serde_json::to_vec_pretty(&value).expect("manifest serializes");
    bytes.push(b'\n');
    bytes
}

fn sort_keys(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.sort_keys();
            map.values_mut().for_each(sort_keys);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(sort_keys),
        _ => {}
    }
}

pub fn check(bytes: &[u8]) -> Result<Checked, Vec<String>> {
    let manifest: Manifest = serde_json::from_slice(bytes)
        .map_err(|error| vec![format!("manifest does not parse: {error}")])?;
    let mut errors = validate(&manifest);
    if errors.is_empty() && canonical_bytes(&manifest) != bytes {
        errors.push(
            "manifest is not in canonical form (pretty JSON, sorted keys, final newline)".into(),
        );
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Checked {
        sha256: hex(&Sha256::digest(bytes)),
        manifest,
    })
}

pub fn validate(manifest: &Manifest) -> Vec<String> {
    let mut errors = Vec::new();
    if manifest.schema != SCHEMA {
        errors.push(format!(
            "unsupported manifest schema {:?}; this builder only accepts {SCHEMA:?}",
            manifest.schema
        ));
        return errors;
    }
    validate_source(&manifest.source, &mut errors);

    if manifest.requested.is_empty() {
        errors.push("requested package list is empty".into());
    }
    if !strictly_increasing(&manifest.requested) {
        errors.push("requested packages must be sorted and unique".into());
    }
    if manifest.packages.is_empty() {
        errors.push("resolved package list is empty".into());
    }

    let mut names = BTreeSet::new();
    let mut digests = BTreeSet::new();
    for (index, package) in manifest.packages.iter().enumerate() {
        let label = format!("packages[{index}] {:?}", package.name);
        validate_package(package, &manifest.source.architecture, &label, &mut errors);
        if !names.insert(package.name.as_str()) {
            errors.push(format!("{label}: package name is resolved more than once"));
        }
        for digest in [&package.sha256, &package.signature_sha256] {
            if !digests.insert(digest.as_str()) {
                errors.push(format!(
                    "{label}: digest {digest} is shared with another artifact"
                ));
            }
        }
    }
    if !manifest
        .packages
        .windows(2)
        .all(|pair| pair[0].sort_key() < pair[1].sort_key())
    {
        errors.push(
            "packages must be strictly sorted by repository, architecture, name, version, sha256"
                .into(),
        );
    }
    for name in &manifest.requested {
        if !names.contains(name.as_str()) {
            errors.push(format!(
                "requested package {name:?} is not in the resolved set"
            ));
        }
    }
    errors
}

fn validate_source(source: &Source, errors: &mut Vec<String>) {
    if !is_token(&source.architecture, |c| {
        c.is_ascii_alphanumeric() || c == '_'
    }) || source.architecture == "any"
    {
        errors.push(format!(
            "source.architecture {:?} is not a concrete architecture",
            source.architecture
        ));
    }
    let date = &source.snapshot_date;
    let date_ok = date.len() == 10
        && date.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            _ => b.is_ascii_digit(),
        });
    if !date_ok {
        errors.push(format!("source.snapshot_date {date:?} is not YYYY-MM-DD"));
    }
    let url = &source.snapshot_url;
    match url.strip_prefix("https://") {
        None => errors.push(format!("source.snapshot_url {url:?} must use https")),
        Some(rest) => {
            if rest.contains(['@', '?', '#', '\\'])
                || rest.chars().any(|c| c.is_whitespace() || c.is_control())
                || rest.split('/').any(|segment| segment == "..")
                || !url.ends_with('/')
                || rest.starts_with('/')
            {
                errors.push(format!(
                    "source.snapshot_url {url:?} must be a plain https directory URL without credentials, query or fragment"
                ));
            }
            // A live mirror has no date in its path. Requiring the snapshot
            // date there rejects mutable repositories instead of trusting a
            // label.
            if date_ok && !url.contains(&format!("/{}/", date.replace('-', "/"))) {
                errors.push(format!(
                    "source.snapshot_url {url:?} does not contain the dated path for {date}"
                ));
            }
        }
    }
}

fn validate_package(package: &Package, source_arch: &str, label: &str, errors: &mut Vec<String>) {
    if !is_token(&package.repository, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
    }) {
        errors.push(format!(
            "{label}: invalid repository {:?}",
            package.repository
        ));
    }
    // pacman package names: lowercase alphanumerics and @._+-, not starting
    // with a hyphen or dot.
    if !is_token(&package.name, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || "@._+-".contains(c)
    }) || package.name.starts_with(['-', '.'])
    {
        errors.push(format!("{label}: invalid package name"));
    }
    if !valid_version(&package.version) {
        errors.push(format!("{label}: invalid version {:?}", package.version));
    }
    if package.architecture != "any" && package.architecture != source_arch {
        errors.push(format!(
            "{label}: architecture {:?} is neither \"any\" nor {source_arch:?}",
            package.architecture
        ));
    }
    let stem = format!(
        "{}-{}-{}.pkg.tar.",
        package.name, package.version, package.architecture
    );
    let filename_ok = package
        .filename
        .strip_prefix(&stem)
        .is_some_and(|suffix| suffix == "zst" || suffix == "xz");
    if !filename_ok {
        errors.push(format!(
            "{label}: filename {:?} does not match {stem}{{zst,xz}}",
            package.filename
        ));
    }
    if package.size == 0 {
        errors.push(format!("{label}: size must be positive"));
    }
    for (field, value) in [
        ("sha256", &package.sha256),
        ("signature_sha256", &package.signature_sha256),
    ] {
        if !is_lower_hex(value, 64) {
            errors.push(format!("{label}: {field} must be 64 lowercase hex digits"));
        }
    }
    if !(package.signer_fingerprint.len() == 40
        && package
            .signer_fingerprint
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b)))
    {
        errors.push(format!(
            "{label}: signer_fingerprint must be a 40-digit uppercase hex OpenPGP fingerprint"
        ));
    }
}

/// `[epoch:]pkgver-pkgrel`, using pacman's version alphabet.
fn valid_version(version: &str) -> bool {
    let (epoch, rest) = match version.split_once(':') {
        Some((epoch, rest)) => (Some(epoch), rest),
        None => (None, version),
    };
    if epoch.is_some_and(|e| e.is_empty() || !e.bytes().all(|b| b.is_ascii_digit())) {
        return false;
    }
    let Some((pkgver, pkgrel)) = rest.rsplit_once('-') else {
        return false;
    };
    is_token(pkgver, |c| c.is_ascii_alphanumeric() || "._+".contains(c))
        && is_token(pkgrel, |c| c.is_ascii_digit() || c == '.')
}

fn is_token(value: &str, allowed: impl Fn(char) -> bool) -> bool {
    !value.is_empty() && value.len() <= 255 && value.chars().all(allowed)
}

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn strictly_increasing(values: &[String]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CacheProblem {
    pub package: String,
    pub artifact: &'static str,
    pub sha256: String,
    pub problem: String,
}

#[derive(Debug, Serialize)]
pub struct CacheReport {
    pub manifest_sha256: String,
    pub artifacts_verified: usize,
    pub missing: Vec<CacheProblem>,
    pub mismatched: Vec<CacheProblem>,
    /// `not_verified` without a keyring; otherwise
    /// `verified_against_supplied_keyring` or `failed`.
    pub signature_trust: &'static str,
    /// SHA-256 of the keyring signatures were checked against, if any.
    pub keyring_sha256: Option<String>,
    pub signature_problems: Vec<CacheProblem>,
}

impl CacheReport {
    pub fn complete(&self) -> bool {
        self.missing.is_empty() && self.mismatched.is_empty() && self.signature_problems.is_empty()
    }
}

/// Verifies every archive and detached signature against a cache laid out as
/// `<cache>/sha256/<hex digest>`. Reads only; never substitutes or fetches.
/// With a keyring, each package whose archive and signature bytes verified
/// also has its signature checked by [`signature::verify`].
pub fn verify_cache(checked: &Checked, cache: &Path, keyring: Option<&Path>) -> CacheReport {
    let mut report = CacheReport {
        manifest_sha256: checked.sha256.clone(),
        artifacts_verified: 0,
        missing: Vec::new(),
        mismatched: Vec::new(),
        signature_trust: SIGNATURE_TRUST,
        keyring_sha256: None,
        signature_problems: Vec::new(),
    };
    if let Some(keyring) = keyring {
        match hash_file(keyring) {
            Ok(digest) => report.keyring_sha256 = Some(digest),
            Err(error) => report.signature_problems.push(CacheProblem {
                package: String::new(),
                artifact: "keyring",
                sha256: String::new(),
                problem: format!("cannot read keyring {}: {error}", keyring.display()),
            }),
        }
    }
    for package in &checked.manifest.packages {
        let before = report.missing.len() + report.mismatched.len();
        for (artifact, digest, size) in [
            ("archive", &package.sha256, Some(package.size)),
            ("signature", &package.signature_sha256, None),
        ] {
            let problem = |problem: String| CacheProblem {
                package: package.name.clone(),
                artifact,
                sha256: digest.clone(),
                problem,
            };
            match hash_cached(&cache.join("sha256").join(digest), size) {
                Ok(()) => report.artifacts_verified += 1,
                Err(CacheError::Missing) => report.missing.push(problem("absent".into())),
                Err(CacheError::Bad(reason)) => report.mismatched.push(problem(reason)),
            }
        }
        let bytes_verified = report.missing.len() + report.mismatched.len() == before;
        if let (Some(keyring), Some(_), true) = (keyring, &report.keyring_sha256, bytes_verified) {
            let entry = |digest: &String| cache.join("sha256").join(digest);
            let problems = signature::verify(
                keyring,
                &entry(&package.signature_sha256),
                &entry(&package.sha256),
                &package.signer_fingerprint,
            );
            if !problems.is_empty() {
                report.signature_problems.push(CacheProblem {
                    package: package.name.clone(),
                    artifact: "signature",
                    sha256: package.signature_sha256.clone(),
                    problem: problems.join("; "),
                });
            }
        }
    }
    if keyring.is_some() {
        report.signature_trust = if report.complete() {
            signature::VERIFIED
        } else {
            "failed"
        };
    }
    report
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    sha256_reader(&mut File::open(path)?)
}

enum CacheError {
    Missing,
    Bad(String),
}

fn hash_cached(path: &Path, expected_size: Option<u64>) -> Result<(), CacheError> {
    // O_NOFOLLOW: a cache entry must be the file itself, not a link that
    // could redirect verification to bytes outside the cache.
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CacheError::Missing)
        }
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(CacheError::Bad("cache entry is a symbolic link".into()))
        }
        Err(error) => return Err(CacheError::Bad(format!("cannot open: {error}"))),
    };
    let metadata = file
        .metadata()
        .map_err(|error| CacheError::Bad(format!("cannot stat: {error}")))?;
    if !metadata.is_file() {
        return Err(CacheError::Bad("cache entry is not a regular file".into()));
    }
    if let Some(expected) = expected_size {
        if metadata.len() != expected {
            return Err(CacheError::Bad(format!(
                "size {} does not match manifest size {expected}",
                metadata.len()
            )));
        }
    }
    let actual = sha256_reader(&mut file)
        .map_err(|error| CacheError::Bad(format!("read failed: {error}")))?;
    let expected = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if actual != expected {
        return Err(CacheError::Bad(format!("content sha256 is {actual}")));
    }
    Ok(())
}

fn sha256_reader(reader: &mut impl Read) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(hex(&hasher.finalize()));
        }
        hasher.update(&buffer[..read]);
    }
}

pub fn read_manifest(path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|error| anyhow::anyhow!("reading {}: {error}", path.display()))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn digest_of(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    fn package(name: &str, archive: &[u8], signature: &[u8]) -> Package {
        Package {
            repository: "core".into(),
            name: name.into(),
            version: "1:2.3-4".into(),
            architecture: "x86_64".into(),
            filename: format!("{name}-1:2.3-4-x86_64.pkg.tar.zst"),
            size: archive.len() as u64,
            sha256: digest_of(archive),
            signature_sha256: digest_of(signature),
            signer_fingerprint: "0123456789ABCDEF0123456789ABCDEF01234567".into(),
        }
    }

    fn manifest() -> Manifest {
        Manifest {
            schema: SCHEMA.into(),
            source: Source {
                snapshot_url: "https://archive.archlinux.org/repos/2026/10/30/".into(),
                snapshot_date: "2026-10-30".into(),
                architecture: "x86_64".into(),
            },
            requested: vec!["base".into()],
            packages: vec![
                package("base", b"base archive", b"base sig"),
                package("filesystem", b"fs archive", b"fs sig"),
            ],
        }
    }

    fn errors_of(manifest: &Manifest) -> String {
        validate(manifest).join("\n")
    }

    #[test]
    fn canonical_manifest_checks_and_has_stable_digest() {
        let bytes = canonical_bytes(&manifest());
        let checked = check(&bytes).expect("valid manifest");
        assert_eq!(checked.sha256, digest_of(&bytes));
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.find("\"architecture\"").unwrap() < text.find("\"filename\"").unwrap());
        assert!(text.find("\"packages\"").unwrap() < text.find("\"schema\"").unwrap());
    }

    #[test]
    fn non_canonical_formatting_is_rejected() {
        let compact = serde_json::to_vec(&manifest()).unwrap();
        let errors = check(&compact).unwrap_err().join("\n");
        assert!(errors.contains("canonical form"), "{errors}");
    }

    #[test]
    fn unknown_fields_and_other_schemas_fail_closed() {
        let mut value = serde_json::to_value(manifest()).unwrap();
        value["trust"] = serde_json::json!("assumed");
        let errors = check(&serde_json::to_vec_pretty(&value).unwrap()).unwrap_err();
        assert!(errors[0].contains("unknown field"), "{errors:?}");

        let mut newer = manifest();
        newer.schema = "flowerly.generation-inputs.v2".into();
        assert!(errors_of(&newer).contains("unsupported manifest schema"));
    }

    #[test]
    fn live_or_unsafe_snapshot_sources_are_rejected() {
        for url in [
            "https://geo.mirror.pkgbuild.com/",
            "http://archive.archlinux.org/repos/2026/10/30/",
            "https://user@archive.archlinux.org/repos/2026/10/30/",
            "https://archive.archlinux.org/repos/2026/10/30/?latest",
            "https://archive.archlinux.org/repos/2026/10/30/../../last/",
            "https://archive.archlinux.org/repos/2026/10/30",
        ] {
            let mut m = manifest();
            m.source.snapshot_url = url.into();
            assert!(errors_of(&m).contains("snapshot_url"), "{url} accepted");
        }
        let mut m = manifest();
        m.source.snapshot_date = "2026-10-31".into();
        assert!(errors_of(&m).contains("dated path"));
        m.source.snapshot_date = "yesterday".into();
        assert!(errors_of(&m).contains("YYYY-MM-DD"));
        m = manifest();
        m.source.architecture = "any".into();
        assert!(errors_of(&m).contains("concrete architecture"));
    }

    #[test]
    fn package_identity_fields_are_validated() {
        type Case = (fn(&mut Package), &'static str);
        let cases: Vec<Case> = vec![
            (|p| p.name = "../etc".into(), "invalid package name"),
            (|p| p.name = "-x".into(), "invalid package name"),
            (|p| p.version = "1.0".into(), "invalid version"),
            (|p| p.version = ":1-1".into(), "invalid version"),
            (|p| p.version = "1/2-1".into(), "invalid version"),
            (|p| p.architecture = "aarch64".into(), "architecture"),
            (|p| p.filename = "../../base.pkg.tar.zst".into(), "filename"),
            (|p| p.filename.push_str(".sig"), "filename"),
            (|p| p.size = 0, "size must be positive"),
            (|p| p.sha256 = p.sha256.to_uppercase(), "sha256 must be"),
            (
                |p| p.signature_sha256.truncate(10),
                "signature_sha256 must be",
            ),
            (
                |p| p.signer_fingerprint = "abc".into(),
                "signer_fingerprint",
            ),
            (|p| p.repository = "Core".into(), "invalid repository"),
        ];
        for (mutate, expected) in cases {
            let mut m = manifest();
            mutate(&mut m.packages[0]);
            let errors = errors_of(&m);
            assert!(
                errors.contains(expected),
                "expected {expected:?} in {errors}"
            );
        }
    }

    #[test]
    fn ordering_duplicates_and_requested_coverage_are_enforced() {
        let mut m = manifest();
        m.packages.reverse();
        assert!(errors_of(&m).contains("strictly sorted"));

        let mut m = manifest();
        let mut dup = m.packages[0].clone();
        dup.repository = "extra".into();
        dup.sha256 = digest_of(b"other");
        dup.signature_sha256 = digest_of(b"other sig");
        m.packages.push(dup);
        assert!(errors_of(&m).contains("resolved more than once"));

        let mut m = manifest();
        m.packages[1].sha256 = m.packages[0].sha256.clone();
        assert!(errors_of(&m).contains("shared with another artifact"));

        let mut m = manifest();
        m.requested = vec!["base".into(), "linux".into()];
        assert!(errors_of(&m).contains("\"linux\" is not in the resolved set"));
        m.requested = vec!["linux".into(), "base".into()];
        assert!(errors_of(&m).contains("sorted and unique"));
    }

    fn write_cache(dir: &Path, entries: &[&[u8]]) {
        let root = dir.join("sha256");
        std::fs::create_dir_all(&root).unwrap();
        for bytes in entries {
            std::fs::write(root.join(digest_of(bytes)), bytes).unwrap();
        }
    }

    fn checked() -> Checked {
        check(&canonical_bytes(&manifest())).unwrap()
    }

    #[test]
    fn complete_cache_verifies_every_artifact_but_not_trust() {
        let dir = tempfile::tempdir().unwrap();
        write_cache(
            dir.path(),
            &[b"base archive", b"base sig", b"fs archive", b"fs sig"],
        );
        let report = verify_cache(&checked(), dir.path(), None);
        assert!(report.complete(), "{report:?}");
        assert_eq!(report.artifacts_verified, 4);
        assert_eq!(report.signature_trust, "not_verified");
    }

    #[test]
    fn missing_and_tampered_artifacts_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        write_cache(dir.path(), &[b"base archive", b"base sig", b"fs sig"]);
        // Same size as the expected archive, different bytes.
        std::fs::write(
            dir.path().join("sha256").join(digest_of(b"base archive")),
            b"BASE ARCHIVE",
        )
        .unwrap();
        let report = verify_cache(&checked(), dir.path(), None);
        assert!(!report.complete());
        assert_eq!(report.missing.len(), 1);
        assert_eq!(report.missing[0].package, "filesystem");
        assert_eq!(report.missing[0].artifact, "archive");
        assert_eq!(report.mismatched.len(), 1);
        assert!(report.mismatched[0].problem.contains("content sha256"));
        assert_eq!(report.artifacts_verified, 2);
    }

    #[test]
    fn wrong_size_symlinks_and_non_files_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sha256");
        write_cache(dir.path(), &[b"base sig", b"fs sig"]);
        std::fs::write(root.join(digest_of(b"base archive")), b"short").unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"fs archive").unwrap();
        std::os::unix::fs::symlink(&outside, root.join(digest_of(b"fs archive"))).unwrap();
        let report = verify_cache(&checked(), dir.path(), None);
        let problems: Vec<_> = report
            .mismatched
            .iter()
            .map(|p| p.problem.as_str())
            .collect();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("does not match manifest size")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("symbolic link")),
            "{problems:?}"
        );

        std::fs::remove_file(root.join(digest_of(b"fs archive"))).unwrap();
        std::fs::create_dir(root.join(digest_of(b"fs archive"))).unwrap();
        let report = verify_cache(&checked(), dir.path(), None);
        assert!(report
            .mismatched
            .iter()
            .any(|p| p.package == "filesystem" && p.problem.contains("not a regular file")));
    }
}
