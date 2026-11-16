//! Binds a built Generation's identity to the frozen package set (#49,
//! ADR-0020 step 7).
//!
//! Two facts, both checked from bytes:
//!
//! 1. the baked identity names this exact manifest
//!    (`input_manifest_sha256` equals the manifest's digest), and
//! 2. the kernel staged for boot is the `vmlinuz` inside the frozen `linux`
//!    archive from the verified cache, not a kernel from somewhere else.
//!
//! The archive is read with the system `tar` and `zstd` (as `signature` uses
//! `gpgv`), after its size and SHA-256 matched the manifest.

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::manifest::{sha256_reader, Checked};
use crate::slot::BakedIdentity;

/// Arch's kernel image is under 20 MiB; this is a bound, not an expectation.
const MAX_KERNEL: u64 = 256 * 1024 * 1024;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Binding {
    pub manifest_sha256: String,
    pub identity_input_manifest_sha256: String,
    pub identity_names_this_manifest: bool,
    pub kernel_package_version: String,
    pub kernel_release: String,
    pub frozen_vmlinuz_sha256: String,
    pub staged_kernel_sha256: String,
    pub staged_kernel_is_the_frozen_one: bool,
}

impl Binding {
    pub fn holds(&self) -> bool {
        self.identity_names_this_manifest && self.staged_kernel_is_the_frozen_one
    }
}

fn tar_command(args: &[&str], archive: &Path) -> Command {
    let mut command = Command::new("tar");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(args)
        .arg(archive)
        .stderr(Stdio::null());
    command
}

fn open_regular(path: &Path, what: &str) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("opening the {what} {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file(),
        "the {what} must be a regular file"
    );
    Ok(file)
}

/// `(kernel release, SHA-256 of its vmlinuz)` from the frozen `linux`
/// archive in `cache`, after the archive itself matched the manifest.
pub fn frozen_kernel(checked: &Checked, cache: &Path) -> Result<(String, String, String)> {
    let package = checked
        .manifest
        .packages
        .iter()
        .find(|package| package.name == "linux")
        .ok_or_else(|| anyhow!("the manifest has no linux package"))?;
    let archive = cache.join("sha256").join(&package.sha256);
    let mut file = open_regular(&archive, "frozen linux archive")?;
    ensure!(
        file.metadata()?.len() == package.size,
        "the frozen linux archive has the wrong size"
    );
    ensure!(
        sha256_reader(&mut file)? == package.sha256,
        "the frozen linux archive does not match the manifest's digest"
    );

    let tar = |args: &[&str]| tar_command(args, &archive);
    let listing = tar(&["--zstd", "-tf"])
        .output()
        .context("listing the linux archive with tar")?;
    ensure!(
        listing.status.success(),
        "tar could not list the linux archive"
    );
    let members: Vec<String> = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter(|line| {
            line.strip_prefix("usr/lib/modules/")
                .and_then(|rest| rest.strip_suffix("/vmlinuz"))
                .is_some_and(|release| !release.is_empty() && !release.contains('/'))
        })
        .map(str::to_owned)
        .collect();
    let [member] = members.as_slice() else {
        bail!(
            "the linux archive must hold exactly one usr/lib/modules/<release>/vmlinuz, found {}",
            members.len()
        );
    };
    let release = member
        .trim_start_matches("usr/lib/modules/")
        .trim_end_matches("/vmlinuz")
        .to_owned();
    let mut child = tar(&["--zstd", "-xOf"])
        .arg(member)
        .stdout(Stdio::piped())
        .spawn()
        .context("extracting the kernel with tar")?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    {
        let mut out = child.stdout.take().expect("piped");
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = out.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            total += read as u64;
            ensure!(
                total <= MAX_KERNEL,
                "the frozen kernel is implausibly large"
            );
            hasher.update(&buffer[..read]);
        }
    }
    ensure!(
        child.wait()?.success() && total > 0,
        "tar could not extract the kernel"
    );
    let digest = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((package.version.clone(), release, digest))
}

/// Checks both facts for a baked identity and a staged kernel image.
pub fn verify_binding(
    checked: &Checked,
    cache: &Path,
    identity: &Path,
    staged_kernel: &Path,
) -> Result<Binding> {
    let mut text = String::new();
    open_regular(identity, "baked identity")?
        .take(64 * 1024)
        .read_to_string(&mut text)?;
    let identity = BakedIdentity::parse(&serde_json::from_str(&text).context("identity JSON")?)?;
    let (version, release, frozen) = frozen_kernel(checked, cache)?;
    let mut staged = open_regular(staged_kernel, "staged kernel")?;
    ensure!(
        staged.metadata()?.len() <= MAX_KERNEL,
        "the staged kernel is implausibly large"
    );
    let staged = sha256_reader(&mut staged)?;
    Ok(Binding {
        manifest_sha256: checked.sha256.clone(),
        identity_names_this_manifest: identity.input_manifest_sha256 == checked.sha256,
        identity_input_manifest_sha256: identity.input_manifest_sha256,
        kernel_package_version: version,
        kernel_release: release,
        staged_kernel_is_the_frozen_one: staged == frozen,
        frozen_vmlinuz_sha256: frozen,
        staged_kernel_sha256: staged,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{canonical_bytes, check, Manifest, Package, Source};

    fn hex(data: &[u8]) -> String {
        Sha256::digest(data)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    struct Rig {
        dir: tempfile::TempDir,
        checked: Checked,
        kernel: Vec<u8>,
    }

    /// A `.pkg.tar.zst`-shaped archive whose members are `usr/...`.
    fn zstd_tar(dir: &Path, files: &[(&str, &[u8])]) -> Vec<u8> {
        let root = dir.join("pkg");
        for (name, data) in files {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }
        let out = dir.join("pkg.tar.zst");
        let status = Command::new("tar")
            .args(["--zstd", "-cf"])
            .arg(&out)
            .arg("-C")
            .arg(&root)
            .arg("usr")
            .status()
            .unwrap();
        assert!(status.success(), "tar --zstd is required for this test");
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::read(out).unwrap()
    }

    fn rig(files: &[(&str, &[u8])], kernel: &[u8]) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let archive = zstd_tar(dir.path(), files);
        let sha = hex(&archive);
        let cache = dir.path().join("cache/sha256");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join(&sha), &archive).unwrap();
        let manifest = Manifest {
            schema: crate::manifest::SCHEMA.into(),
            source: Source {
                snapshot_url: "https://archive.archlinux.org/repos/2026/11/09/".into(),
                snapshot_date: "2026-11-09".into(),
                architecture: "x86_64".into(),
            },
            requested: vec!["linux".into()],
            packages: vec![Package {
                repository: "core".into(),
                name: "linux".into(),
                version: "7.2.14.arch1-1".into(),
                architecture: "x86_64".into(),
                filename: "linux-7.2.14.arch1-1-x86_64.pkg.tar.zst".into(),
                size: archive.len() as u64,
                sha256: sha,
                signature_sha256: "ab".repeat(32),
                signer_fingerprint: "0123456789ABCDEF0123456789ABCDEF01234567".into(),
            }],
        };
        let checked = check(&canonical_bytes(&manifest)).unwrap();
        Rig {
            dir,
            checked,
            kernel: kernel.to_vec(),
        }
    }

    fn identity(rig: &Rig, manifest_sha: &str) -> std::path::PathBuf {
        let path = rig.dir.path().join("identity.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "schema": crate::slot::IDENTITY_SCHEMA,
                "slot": "root-b",
                "root_partuuid": "11111111-2222-3333-4444-555555555555",
                "source_commit": "a".repeat(40),
                "input_manifest_sha256": manifest_sha,
                "build_id": "build-1",
                "schema_version": 17,
                "min_reader": 16,
            })
            .to_string(),
        )
        .unwrap();
        path
    }

    fn staged(rig: &Rig, bytes: &[u8]) -> std::path::PathBuf {
        let path = rig.dir.path().join("vmlinuz");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    const KERNEL: &[u8] = b"kernel image bytes";

    fn linux_files() -> Vec<(&'static str, &'static [u8])> {
        vec![
            ("usr/lib/modules/7.2.14-arch1-1/vmlinuz", KERNEL),
            ("usr/lib/modules/7.2.14-arch1-1/modules.dep", b"x"),
        ]
    }

    #[test]
    fn the_staged_kernel_and_identity_of_this_manifest_bind() {
        let rig = rig(&linux_files(), KERNEL);
        let sha = rig.checked.sha256.clone();
        let binding = verify_binding(
            &rig.checked,
            &rig.dir.path().join("cache"),
            &identity(&rig, &sha),
            &staged(&rig, &rig.kernel),
        )
        .unwrap();
        assert!(binding.holds(), "{binding:?}");
        assert_eq!(binding.kernel_release, "7.2.14-arch1-1");
        assert_eq!(binding.frozen_vmlinuz_sha256, hex(KERNEL));
    }

    #[test]
    fn a_kernel_from_elsewhere_does_not_bind() {
        let rig = rig(&linux_files(), KERNEL);
        let sha = rig.checked.sha256.clone();
        let binding = verify_binding(
            &rig.checked,
            &rig.dir.path().join("cache"),
            &identity(&rig, &sha),
            &staged(&rig, b"a different kernel"),
        )
        .unwrap();
        assert!(binding.identity_names_this_manifest);
        assert!(!binding.staged_kernel_is_the_frozen_one && !binding.holds());
    }

    #[test]
    fn an_identity_for_another_manifest_does_not_bind() {
        let rig = rig(&linux_files(), KERNEL);
        let binding = verify_binding(
            &rig.checked,
            &rig.dir.path().join("cache"),
            &identity(&rig, &"0".repeat(64)),
            &staged(&rig, &rig.kernel),
        )
        .unwrap();
        assert!(binding.staged_kernel_is_the_frozen_one);
        assert!(!binding.identity_names_this_manifest && !binding.holds());
    }

    #[test]
    fn a_tampered_or_ambiguous_linux_archive_is_refused() {
        let rig = rig(&linux_files(), KERNEL);
        let cache = rig.dir.path().join("cache");
        let entry = cache
            .join("sha256")
            .join(&rig.checked.manifest.packages[0].sha256);
        let mut bytes = std::fs::read(&entry).unwrap();
        bytes[0] ^= 1;
        std::fs::write(&entry, bytes).unwrap();
        let error = frozen_kernel(&rig.checked, &cache).unwrap_err().to_string();
        assert!(error.contains("digest"), "{error}");

        let two = rig_two_kernels();
        let error = frozen_kernel(&two.checked, &two.dir.path().join("cache"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("exactly one"), "{error}");
    }

    fn rig_two_kernels() -> Rig {
        rig(
            &[
                ("usr/lib/modules/7.2.14-arch1-1/vmlinuz", KERNEL),
                ("usr/lib/modules/7.2.15-arch1-1/vmlinuz", b"other"),
            ],
            KERNEL,
        )
    }

    #[test]
    fn a_symlinked_cache_entry_or_kernel_is_not_followed() {
        let rig = rig(&linux_files(), KERNEL);
        let cache = rig.dir.path().join("cache");
        let entry = cache
            .join("sha256")
            .join(&rig.checked.manifest.packages[0].sha256);
        let real = rig.dir.path().join("real");
        std::fs::rename(&entry, &real).unwrap();
        std::os::unix::fs::symlink(&real, &entry).unwrap();
        let error = frozen_kernel(&rig.checked, &cache).unwrap_err();
        assert!(
            format!("{error:#}").contains("opening the frozen linux archive"),
            "{error:#}"
        );
    }
}
