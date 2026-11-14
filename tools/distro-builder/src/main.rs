mod manifest;
mod signature;
mod slot;

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
    /// Stage, arm, roll back and inspect A/B root slots on a disk image
    /// (#48, ADR-0019). Edits disk images only, never block devices.
    #[command(subcommand)]
    Slot(SlotAction),
    /// The root slot broker (ADR-0023 G4): answer one intent on the socket
    /// systemd passes as stdin. Test profile only until Gate D is verified.
    #[command(hide = true)]
    SlotBroker {
        #[arg(long, default_value = slot::broker::DEFAULT_CONFIG)]
        config: PathBuf,
    },
}

#[derive(clap::Args)]
struct SlotDisk {
    /// Raw GPT disk image (a regular file).
    #[arg(long)]
    disk: PathBuf,
    /// Disk manifest written by build-flowerly-ab-disk.sh.
    #[arg(long)]
    manifest: PathBuf,
    /// PARTUUID of the running root. A claim, not authority: the tool only
    /// uses it to refuse touching that slot. Default: root= in /proc/cmdline.
    #[arg(long)]
    active_partuuid: Option<String>,
}

#[derive(Subcommand)]
enum SlotAction {
    /// Write a root image into the inactive slot; changes no boot entry.
    Stage {
        #[command(flatten)]
        target: SlotDisk,
        #[arg(long)]
        slot: String,
        #[arg(long)]
        candidate: PathBuf,
        #[arg(long)]
        sha256: String,
        /// Kernel for the staged slot, placed on the ESP (with --initramfs
        /// and --identity; needed before the slot broker can arm it).
        #[arg(long, requires_all = ["initramfs", "identity"])]
        kernel: Option<PathBuf>,
        #[arg(long, requires_all = ["kernel", "identity"])]
        initramfs: Option<PathBuf>,
        /// The candidate's own generation-root.json.
        #[arg(long, requires_all = ["kernel", "initramfs"])]
        identity: Option<PathBuf>,
    },
    /// Publish a boot-counting entry for a staged slot (pending, not success).
    Arm {
        #[command(flatten)]
        target: SlotDisk,
        #[arg(long)]
        slot: String,
        #[arg(long)]
        kernel: PathBuf,
        #[arg(long)]
        initramfs: PathBuf,
        #[arg(long, default_value_t = 3)]
        tries: u32,
        /// Extra kernel option for the new entry (repeatable).
        #[arg(long = "extra-option")]
        extra_options: Vec<String>,
    },
    /// Remove the other slot's boot entries so the loader boots `--to`.
    Rollback {
        #[command(flatten)]
        target: SlotDisk,
        #[arg(long)]
        to: String,
    },
    /// Report entries, slot identity and, with a witness ledger, the verdict.
    Status {
        #[command(flatten)]
        target: SlotDisk,
        /// Copy of the guest's boot-witness.jsonl.
        #[arg(long)]
        witness: Option<PathBuf>,
        /// Also hash both root slots against the manifest (slow).
        #[arg(long)]
        verify_slots: bool,
    },
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
        /// Binary OpenPGP keyring to check each detached signature against
        /// with gpgv. Without it signatures are reported `not_verified`.
        #[arg(long)]
        keyring: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Args::parse().action {
        Action::Validate => validate(),
        Action::Build => {
            bail!("bootable Arch rootfs/ISO output is not implemented; run distro-builder validate")
        }
        Action::Slot(action) => run_slot(action),
        Action::SlotBroker { config } => slot::broker::serve_stdin(&config),
        Action::Manifest(ManifestAction::Check { path }) => {
            let checked = check_manifest(&path)?;
            println!("manifest_sha256={}", checked.sha256);
            println!("packages={}", checked.manifest.packages.len());
            println!("signature_trust={}", manifest::SIGNATURE_TRUST);
            Ok(())
        }
        Action::Manifest(ManifestAction::VerifyCache {
            path,
            cache,
            keyring,
        }) => {
            let checked = check_manifest(&path)?;
            let report = manifest::verify_cache(&checked, &cache, keyring.as_deref());
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.complete() {
                bail!(
                    "cache verification failed: {} missing, {} mismatched, {} signature problems; no artifact was fetched or substituted",
                    report.missing.len(),
                    report.mismatched.len(),
                    report.signature_problems.len()
                );
            }
            Ok(())
        }
    }
}

fn slot_faults() -> slot::Faults {
    slot::Faults::for_build()
}

fn active_slot(disk: &slot::Disk, claim: &SlotDisk) -> Result<slot::Slot> {
    let partuuid = match &claim.active_partuuid {
        Some(value) => value.clone(),
        None => slot::cmdline_root_partuuid()?,
    };
    disk.active_slot(&partuuid)
}

fn run_slot(action: SlotAction) -> Result<()> {
    let faults = slot_faults();
    let report = match action {
        SlotAction::Stage {
            target,
            slot: name,
            candidate,
            sha256,
            kernel,
            initramfs,
            identity,
        } => {
            let (image, mut disk) = slot::open_bound(&target.disk, &target.manifest, true)?;
            let mut esp = slot::esp::MtoolsEsp::new(&target.disk, disk.part("esp").offset);
            let slot = slot::Slot::parse(&name)?;
            let request = slot::StageRequest {
                manifest_path: &target.manifest,
                active: active_slot(&disk, &target)?,
                target: slot,
                candidate: &candidate,
                expected_sha: &sha256,
            };
            let staged = slot::stage(&image, &mut disk, &esp, &request, &faults)?;
            match (kernel, initramfs, identity) {
                (Some(kernel), Some(initramfs), Some(identity)) => {
                    let payload = slot::BootPayload {
                        kernel: &kernel,
                        initramfs: &initramfs,
                        identity: &identity,
                    };
                    slot::stage_boot(
                        &image,
                        &mut disk,
                        &target.manifest,
                        slot,
                        &mut esp,
                        &payload,
                        &faults,
                    )?
                }
                _ => staged,
            }
        }
        SlotAction::Arm {
            target,
            slot: name,
            kernel,
            initramfs,
            tries,
            extra_options,
        } => {
            let (image, disk) = slot::open_bound(&target.disk, &target.manifest, true)?;
            let mut esp = slot::esp::MtoolsEsp::new(&target.disk, disk.part("esp").offset);
            let request = slot::ArmRequest {
                kernel: &kernel,
                initramfs: &initramfs,
                tries,
                extra_options: &extra_options,
            };
            slot::arm(
                &image,
                &disk,
                active_slot(&disk, &target)?,
                slot::Slot::parse(&name)?,
                &mut esp,
                &request,
                &faults,
            )?
        }
        SlotAction::Rollback { target, to } => {
            let (image, disk) = slot::open_bound(&target.disk, &target.manifest, true)?;
            let mut esp = slot::esp::MtoolsEsp::new(&target.disk, disk.part("esp").offset);
            slot::rollback(&image, &disk, slot::Slot::parse(&to)?, &mut esp, &faults)?
        }
        SlotAction::Status {
            target,
            witness,
            verify_slots,
        } => {
            let (image, disk) = slot::open_bound(&target.disk, &target.manifest, false)?;
            let esp = slot::esp::MtoolsEsp::new(&target.disk, disk.part("esp").offset);
            slot::status(&image, &disk, &esp, witness.as_deref(), verify_slots)?
        }
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
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
