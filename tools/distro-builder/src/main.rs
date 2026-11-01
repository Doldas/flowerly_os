mod manifest;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "distro-builder",
    about = "Flowerly OS distribution image tools"
)]
struct Args {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Verify Arch input manifests and target service overlay.
    Validate,
    /// Image production is not yet implemented. Fails without side effects.
    Build,
    /// Check or verify a Generation input manifest (#49). Offline only.
    #[command(subcommand)]
    Manifest(ManifestAction),
}

#[derive(Subcommand)]
enum ManifestAction {
    /// Validate shape and canonical form; print the manifest sha256.
    Check { path: PathBuf },
    /// Verify every archive and signature in a content-addressed cache.
    /// Prints a JSON report; exits non-zero on any missing or bad artifact.
    VerifyCache {
        path: PathBuf,
        #[arg(long)]
        cache: PathBuf,
    },
}

fn main() -> Result<()> {
    match Args::parse().action {
        Action::Validate => validate(),
        Action::Build => {
            bail!("bootable Arch rootfs/ISO output is not implemented; run distro-builder validate")
        }
        Action::Manifest(ManifestAction::Check { path }) => {
            let checked = check_manifest(&path)?;
            println!("manifest_sha256={}", checked.sha256);
            println!("packages={}", checked.manifest.packages.len());
            println!("signature_trust={}", manifest::SIGNATURE_TRUST);
            Ok(())
        }
        Action::Manifest(ManifestAction::VerifyCache { path, cache }) => {
            let checked = check_manifest(&path)?;
            let report = manifest::verify_cache(&checked, &cache);
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.complete() {
                bail!(
                    "cache is incomplete: {} missing, {} mismatched; no artifact was fetched or substituted",
                    report.missing.len(),
                    report.mismatched.len()
                );
            }
            Ok(())
        }
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn check_manifest(path: &Path) -> Result<manifest::Checked> {
    let bytes = manifest::read_manifest(path)?;
    let checked = manifest::check(&bytes).map_err(|errors| {
        anyhow::anyhow!("{} is invalid:\n  {}", path.display(), errors.join("\n  "))
    })?;
    let base = repo_root().join("distro/base-image/arch/packages.base");
    let wanted: Vec<String> = std::fs::read_to_string(&base)
        .with_context(|| format!("reading {}", base.display()))?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect();
    if checked.manifest.requested != wanted {
        bail!(
            "{}: requested packages differ from {}",
            path.display(),
            base.display()
        );
    }
    Ok(checked)
}

fn validate() -> Result<()> {
    let root = repo_root();
    let arch = root.join("distro/base-image/arch");
    for name in ["mkosi.conf", "packages.base", "flowerly-binaries.txt"] {
        let path = arch.join(name);
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        if content.trim().is_empty() {
            bail!("empty image input: {}", path.display());
        }
    }
    let binaries = std::fs::read_to_string(arch.join("flowerly-binaries.txt"))?;
    if binaries.lines().collect::<Vec<_>>() != ["agenticctl", "evolutiond", "flowerlyd"] {
        bail!("distribution binary manifest must contain the exact sorted binaries");
    }
    for name in ["flowerlyd", "evolutiond"] {
        let unit = root.join(format!(
            "distro/overlay/usr/lib/systemd/system/{name}.service"
        ));
        if !unit.is_file() {
            bail!("missing target unit {}", unit.display());
        }
    }
    println!("Flowerly Arch inputs validated; image build remains unimplemented");
    Ok(())
}
