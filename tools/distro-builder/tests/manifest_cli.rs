//! Black-box tests of `distro-builder manifest` (#49): real executable, real
//! files, exit status and machine-readable output.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::{Command, Output};

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The image recipe's requested packages, each resolved to a synthetic
/// archive and signature whose bytes are written to `cache` when given.
fn fixture(dir: &Path) -> (Value, Vec<(String, Vec<u8>)>) {
    let base = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../distro/base-image/arch/packages.base"),
    )
    .unwrap();
    let requested: Vec<String> = base
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    let mut artifacts = Vec::new();
    let packages: Vec<Value> = requested
        .iter()
        .map(|name| {
            let archive = format!("archive of {name}").into_bytes();
            let signature = format!("signature of {name}").into_bytes();
            let entry = json!({
                "repository": "core",
                "name": name,
                "version": "1.0-1",
                "architecture": "x86_64",
                "filename": format!("{name}-1.0-1-x86_64.pkg.tar.zst"),
                "size": archive.len(),
                "sha256": sha256(&archive),
                "signature_sha256": sha256(&signature),
                "signer_fingerprint": "0123456789ABCDEF0123456789ABCDEF01234567",
            });
            artifacts.push((name.clone(), archive));
            artifacts.push((format!("{name}.sig"), signature));
            entry
        })
        .collect();
    let manifest = json!({
        "schema": "flowerly.generation-inputs.v1",
        "source": {
            "snapshot_url": "https://archive.archlinux.org/repos/2026/10/30/",
            "snapshot_date": "2026-10-30",
            "architecture": "x86_64",
        },
        "requested": requested,
        "packages": packages,
    });
    std::fs::create_dir_all(dir.join("cache/sha256")).unwrap();
    (manifest, artifacts)
}

fn write_manifest(dir: &Path, manifest: &Value) -> std::path::PathBuf {
    let path = dir.join("generation-inputs.json");
    let mut bytes = serde_json::to_vec_pretty(manifest).unwrap();
    bytes.push(b'\n');
    std::fs::write(&path, bytes).unwrap();
    path
}

fn populate(dir: &Path, artifacts: &[(String, Vec<u8>)]) {
    for (_, bytes) in artifacts {
        std::fs::write(dir.join("cache/sha256").join(sha256(bytes)), bytes).unwrap();
    }
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_distro-builder"))
        .args(args)
        .output()
        .unwrap()
}

fn verify_cache(dir: &Path, manifest: &Path) -> Output {
    run(&[
        "manifest",
        "verify-cache",
        manifest.to_str().unwrap(),
        "--cache",
        dir.join("cache").to_str().unwrap(),
    ])
}

#[test]
fn check_prints_the_digest_of_the_canonical_file() {
    let dir = tempfile::tempdir().unwrap();
    let (manifest, _) = fixture(dir.path());
    let path = write_manifest(dir.path(), &manifest);
    let output = run(&["manifest", "check", path.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let expected = format!("manifest_sha256={}", sha256(&std::fs::read(&path).unwrap()));
    assert!(stdout.contains(&expected), "{stdout}");
    assert!(stdout.contains("signature_trust=not_verified"), "{stdout}");
}

#[test]
fn check_rejects_requested_set_that_differs_from_the_recipe() {
    let dir = tempfile::tempdir().unwrap();
    let (mut manifest, _) = fixture(dir.path());
    manifest["requested"].as_array_mut().unwrap().pop();
    let path = write_manifest(dir.path(), &manifest);
    let output = run(&["manifest", "check", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("requested packages differ"));
}

#[test]
fn check_rejects_an_invalid_manifest_with_every_problem() {
    let dir = tempfile::tempdir().unwrap();
    let (mut manifest, _) = fixture(dir.path());
    manifest["source"]["snapshot_url"] = json!("https://geo.mirror.pkgbuild.com/");
    manifest["packages"][0]["sha256"] = json!("0");
    let path = write_manifest(dir.path(), &manifest);
    let output = run(&["manifest", "check", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("dated path"), "{stderr}");
    assert!(stderr.contains("sha256 must be 64"), "{stderr}");
}

#[test]
fn verify_cache_succeeds_only_with_every_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let (manifest, artifacts) = fixture(dir.path());
    let path = write_manifest(dir.path(), &manifest);
    populate(dir.path(), &artifacts);
    let output = verify_cache(dir.path(), &path);
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["artifacts_verified"], artifacts.len());
    assert_eq!(report["signature_trust"], "not_verified");
    assert_eq!(report["missing"], json!([]));
}

#[test]
fn verify_cache_reports_missing_and_tampered_digests_and_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (manifest, artifacts) = fixture(dir.path());
    let path = write_manifest(dir.path(), &manifest);
    populate(dir.path(), &artifacts);
    let cache = dir.path().join("cache/sha256");
    let (missing_name, missing) = &artifacts[0];
    std::fs::remove_file(cache.join(sha256(missing))).unwrap();
    let (_, tampered) = &artifacts[2];
    let mut flipped = tampered.clone();
    flipped[0] ^= 0x20;
    std::fs::write(cache.join(sha256(tampered)), flipped).unwrap();

    let output = verify_cache(dir.path(), &path);
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["missing"][0]["package"], missing_name.as_str());
    assert_eq!(report["missing"][0]["sha256"], sha256(missing));
    assert_eq!(report["mismatched"][0]["sha256"], sha256(tampered));
    assert_eq!(report["artifacts_verified"], artifacts.len() - 2);
    assert!(String::from_utf8_lossy(&output.stderr).contains("no artifact was fetched"));
}

#[test]
fn verify_cache_refuses_an_invalid_manifest_before_reading_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let (manifest, artifacts) = fixture(dir.path());
    populate(dir.path(), &artifacts);
    let path = dir.path().join("generation-inputs.json");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let output = verify_cache(dir.path(), &path);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("canonical form"));
}
