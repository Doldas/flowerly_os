use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
}

fn main() -> Result<()> {
    match Args::parse().action {
        Action::Validate => validate(),
        Action::Build => {
            bail!("bootable Arch rootfs/ISO output is not implemented; run distro-builder validate")
        }
    }
}

fn validate() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
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
