//! The slot broker (ADR-0023 section 4, R3; G4a): the only root process that
//! changes which root boots next.
//!
//! systemd hands it one accepted connection on stdin (`Accept=yes`). It
//! answers one typed intent, `arm` or `disarm`, and decides everything from
//! its own state, never from the caller:
//! - activation must be enabled: the release switch below is off until Gate D
//!   is verified, so only an image carrying the explicit test-profile marker
//!   answers at all;
//! - the kernel-attested peer must be the service account, and the admin uid
//!   it speaks for must still be in the admin group per the system databases;
//! - the disk is opened read-only and bound to the manifest through the real
//!   GPT; the running root comes from the broker's own kernel command line;
//! - both slots are hashed against the manifest; the staged slot, its boot
//!   payload on the ESP and its baked identity must equal the Generation the
//!   caller claims;
//! - a target whose migrations leave a `min_reader` newer than the running
//!   root's `flowerlyd` would make the fallback unusable: that activation is
//!   irreversible and refused here (its safe path is G4b).
//!
//! It writes nothing but one boot entry on the ESP (or removes one), never
//! the block device, and accepts no path, command line or Generation id.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::esp::Esp;
use super::live::{open_bound_read_only, DirEsp};
use super::{hex, publish_entry, rollback, verify_slot, BakedIdentity, Faults, Slot};

/// Off until the change that records the passing G5 evidence (ADR-0023).
pub const RELEASE_ACTIVATION_ENABLED: bool = false;

pub const DEFAULT_CONFIG: &str = "/usr/lib/flowerly/slot-broker.json";
const MAX_INTENT: usize = 8 * 1024;
const MAX_CONFIG: u64 = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

/// Where the broker looks. Root's file in the read-only root image.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerConfig {
    pub disk: PathBuf,
    pub manifest: PathBuf,
    pub esp: PathBuf,
    pub running_identity: PathBuf,
    pub test_profile_marker: PathBuf,
    pub passwd: PathBuf,
    pub group: PathBuf,
    pub cmdline: PathBuf,
    pub service_user: String,
    pub admin_group: String,
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// A file only the broker's own user may change.
fn read_private(path: &Path, limit: u64, what: &str) -> Result<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path)
        .with_context(|| format!("reading the {what} {}", path.display()))?;
    if !meta.is_file() {
        bail!("the {what} {} is not a regular file", path.display());
    }
    if meta.uid() != effective_uid() || meta.mode() & 0o022 != 0 {
        bail!(
            "the {what} {} must be owned by the broker and not group or world writable",
            path.display()
        );
    }
    if meta.len() > limit {
        bail!("the {what} is too large");
    }
    Ok(std::fs::read(path)?)
}

impl BrokerConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = read_private(path, MAX_CONFIG, "broker config")?;
        serde_json::from_slice(&bytes).context("broker config")
    }

    fn enabled(&self) -> bool {
        RELEASE_ACTIVATION_ENABLED
            || read_private(&self.test_profile_marker, 4096, "test-profile marker").is_ok()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u32,
    intent: String,
    admin_uid: u32,
    #[serde(default)]
    slot: Option<String>,
    #[serde(default)]
    tries: Option<u32>,
    #[serde(default)]
    generation: Option<GenerationClaim>,
}

/// The Generation the caller wants armed: a claim the broker checks against
/// what is staged, never an instruction.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationClaim {
    rootfs_sha256: String,
    kernel_sha256: String,
    initramfs_sha256: String,
    source_commit: String,
    input_manifest_sha256: String,
    build_id: String,
}

/// uid -> (name, primary gid) from passwd.
fn account(passwd: &str, uid: u32) -> Option<(String, u32)> {
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() >= 4 && fields[2].parse() == Ok(uid) {
            Some((fields[0].to_owned(), fields[3].parse().ok()?))
        } else {
            None
        }
    })
}

fn uid_of(passwd: &str, name: &str) -> Option<u32> {
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() >= 4 && fields[0] == name {
            fields[2].parse().ok()
        } else {
            None
        }
    })
}

/// Whether `uid` is in `group`, as a supplementary member or by primary gid.
fn is_member(passwd: &str, group: &str, name: &str, uid: u32) -> Result<bool> {
    let (user, primary) =
        account(passwd, uid).ok_or_else(|| anyhow!("admin uid {uid} is an unknown uid"))?;
    let line = group
        .lines()
        .find(|line| line.split(':').next() == Some(name))
        .ok_or_else(|| anyhow!("group {name} does not exist"))?;
    let fields: Vec<&str> = line.split(':').collect();
    if fields.len() < 4 {
        bail!("group {name} is malformed");
    }
    let gid: u32 = fields[2].parse().context("group gid")?;
    Ok(primary == gid || fields[3].split(',').any(|member| member == user))
}

fn running_slot(cmdline: &str) -> Result<String> {
    let roots: Vec<&str> = cmdline
        .split_whitespace()
        .filter_map(|t| t.strip_prefix("root=PARTUUID="))
        .collect();
    match roots.as_slice() {
        [one] => Ok(one.to_ascii_lowercase()),
        _ => bail!("cannot determine the running root from the kernel command line"),
    }
}

/// Answer one intent. `peer_uid` is the kernel-attested caller.
pub fn handle(
    config: &BrokerConfig,
    peer_uid: u32,
    request: &[u8],
    esp: &mut dyn Esp,
) -> Result<Value> {
    if !config.enabled() {
        bail!("activation is disabled until Gate D is verified");
    }
    let passwd = std::fs::read_to_string(&config.passwd).context("reading passwd")?;
    let group = std::fs::read_to_string(&config.group).context("reading group")?;
    let service = uid_of(&passwd, &config.service_user)
        .ok_or_else(|| anyhow!("service account {} does not exist", config.service_user))?;
    if peer_uid != service {
        bail!("peer uid {peer_uid} is not the service account");
    }
    if request.len() > MAX_INTENT {
        bail!("intent is too large");
    }
    let intent: Intent = serde_json::from_slice(request).context("malformed intent")?;
    if intent.version != 1 {
        bail!("unsupported intent version {}", intent.version);
    }
    if !is_member(&passwd, &group, &config.admin_group, intent.admin_uid)? {
        bail!(
            "admin uid {} is not a member of {}",
            intent.admin_uid,
            config.admin_group
        );
    }

    read_private(&config.manifest, 1 << 20, "disk manifest")?;
    let (image, disk) = open_bound_read_only(&config.disk, &config.manifest)?;
    let cmdline = std::fs::read_to_string(&config.cmdline).context("reading the command line")?;
    let active = disk.active_slot(&running_slot(&cmdline)?)?;
    let target = active.other();
    verify_slot(&image, disk.slot(active), "active root")?;

    match intent.intent.as_str() {
        "disarm" => rollback(&image, &disk, active, esp, &Faults::none()),
        "arm" => {
            let claimed = intent.slot.as_deref().unwrap_or_default();
            if claimed != target.name() {
                bail!(
                    "slot {claimed:?} is not the inactive slot {}",
                    target.name()
                );
            }
            let tries = intent.tries.unwrap_or(3);
            if !(1..=9).contains(&tries) {
                bail!("tries must be 1..=9");
            }
            let claim = intent
                .generation
                .ok_or_else(|| anyhow!("arm needs the generation it arms"))?;
            verify_target(config, &image, &disk, active, target, esp, &claim)?;
            publish_entry(&disk, active, target, esp, tries, &[], &Faults::none())
        }
        other => bail!("unknown intent {other:?}"),
    }
}

fn verify_target(
    config: &BrokerConfig,
    image: &super::Image,
    disk: &super::Disk,
    active: Slot,
    target: Slot,
    esp: &dyn Esp,
    claim: &GenerationClaim,
) -> Result<()> {
    let part = disk.slot(target);
    if part.filesystem != "ext4" {
        bail!("{} has not been staged", target.name());
    }
    verify_slot(image, part, "staged root")?;
    if part.sha256 != claim.rootfs_sha256 {
        bail!("the staged slot is not the claimed rootfs_sha256");
    }
    let boot = part
        .boot
        .as_ref()
        .ok_or_else(|| anyhow!("{} has no staged boot payload", target.name()))?;
    let identity = part
        .identity
        .as_ref()
        .ok_or_else(|| anyhow!("{} has no recorded identity", target.name()))?;
    for (field, recorded, claimed, file) in [
        (
            "kernel_sha256",
            &boot.kernel_sha256,
            &claim.kernel_sha256,
            "vmlinuz",
        ),
        (
            "initramfs_sha256",
            &boot.initramfs_sha256,
            &claim.initramfs_sha256,
            "initramfs.img",
        ),
    ] {
        if recorded != claimed {
            bail!("the staged {field} is not the claimed one");
        }
        let path = format!("/flowerly/{}/{file}", target.letter());
        if &hex(&Sha256::digest(esp.read(&path)?)) != recorded {
            bail!("{path} on the ESP is not the staged payload");
        }
    }
    if identity.slot != target || identity.root_partuuid != part.partuuid {
        bail!("the recorded identity names another slot");
    }
    for (field, baked, claimed) in [
        (
            "source_commit",
            &identity.source_commit,
            &claim.source_commit,
        ),
        (
            "input_manifest_sha256",
            &identity.input_manifest_sha256,
            &claim.input_manifest_sha256,
        ),
        ("build_id", &identity.build_id, &claim.build_id),
    ] {
        if baked != claimed {
            bail!("the staged image's {field} is not the claimed one");
        }
    }
    // The fallback is the running root: its flowerlyd must still read the
    // database after the target's migrations (ADR-0023 R4).
    let raw: Value = serde_json::from_slice(&read_private(
        &config.running_identity,
        4096,
        "running identity",
    )?)
    .context("running identity")?;
    let running = BakedIdentity::parse(&raw).context("running identity")?;
    if running.slot != active || running.root_partuuid != disk.slot(active).partuuid {
        bail!("the running identity is not the active slot");
    }
    if identity.min_reader > running.schema_version {
        bail!(
            "irreversible activation: the target needs min_reader {}, the fallback knows schema {}; refused until the irreversible path exists",
            identity.min_reader,
            running.schema_version
        );
    }
    Ok(())
}

/// Serve one connection: read one intent line, answer one JSON line.
pub fn serve_stream(stream: UnixStream, config_path: &Path) -> Result<()> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let peer = flowerly_peercred::peer_credentials(&stream).context("peer credentials")?;
    let request = read_line(&stream)?;
    let outcome = BrokerConfig::load(config_path).and_then(|config| {
        let mut esp = DirEsp::new(&config.esp);
        handle(&config, peer.uid, &request, &mut esp)
    });
    let reply = match outcome {
        Ok(mut report) => {
            report["ok"] = json!(true);
            eprintln!(
                "slot broker: peer uid {} served: {}",
                peer.uid, report["status"]
            );
            report
        }
        Err(error) => {
            eprintln!("slot broker: peer uid {} refused: {error:#}", peer.uid);
            json!({ "ok": false, "reason": format!("{error:#}") })
        }
    };
    let mut line = serde_json::to_vec(&reply)?;
    line.push(b'\n');
    (&stream).write_all(&line)?;
    Ok(())
}

/// One intent line, at most [`MAX_INTENT`] bytes (more is answered as too large).
fn read_line(stream: &UnixStream) -> Result<Vec<u8>> {
    let mut request = Vec::new();
    let mut byte = [0u8; 1];
    while request.len() <= MAX_INTENT {
        match (&*stream).read(&mut byte)? {
            0 => break,
            _ if byte[0] == b'\n' => break,
            _ => request.push(byte[0]),
        }
    }
    Ok(request)
}

/// systemd passes the accepted connection as stdin (`Accept=yes`).
pub fn serve_stdin(config_path: &Path) -> Result<()> {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::fs::FileTypeExt;
    // SAFETY: descriptor 0 belongs to this process for its whole life and is
    // taken exactly once, here; nothing else in the broker uses stdin.
    let fd = unsafe { OwnedFd::from_raw_fd(0) };
    let file = std::fs::File::from(fd);
    if !file.metadata()?.file_type().is_socket() {
        bail!("stdin is not the accepted broker socket");
    }
    serve_stream(UnixStream::from(OwnedFd::from(file)), config_path)
}
