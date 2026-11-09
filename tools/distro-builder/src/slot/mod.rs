//! A/B slot tool (#48, ADR-0019): `stage`, `arm`, `rollback` and `status` on a
//! Flowerly two-slot disk **image**.
//!
//! Safety properties the tests pin down:
//! - slots are resolved from the manifest bound to the real GPT, never from a
//!   caller-supplied offset, and only the target slot's byte range is ever
//!   written (the data partition and the active root cannot be reached);
//! - the active slot is a caller claim (`--active-partuuid`, default
//!   `/proc/cmdline`); the tool refuses to touch it and re-verifies its hash;
//! - the durable pending record is the boot entry itself (systemd-boot's
//!   boot-counting file name), published by a single FAT rename, so there is no
//!   second record that could disagree after power loss;
//! - `status` never reports success from boot entries alone: `ACTIVE` needs the
//!   guest's witness ledger to agree.
//!
//! Block devices and mounted ESPs are out of scope for now; a privileged
//! broker owning them is a later step.

pub mod esp;
pub mod gpt;

use anyhow::{anyhow, bail, Context, Result};
use esp::Esp;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

pub const SCHEMA: &str = "flowerly-ab-disk/v1";
pub const LOADER_DEFAULT: &str = "default flowerly-*";
const MIB: u64 = 1 << 20;
const MAX_MANIFEST: u64 = MIB;
const MAX_WITNESS: u64 = 2 * MIB;
const EXT4_MAGIC: [u8; 2] = [0x53, 0xEF];
const ENTRIES: &str = "/loader/entries";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "root-a" => Ok(Self::A),
            "root-b" => Ok(Self::B),
            _ => bail!("slot must be root-a or root-b"),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::A => "root-a",
            Self::B => "root-b",
        }
    }
    pub fn letter(self) -> char {
        match self {
            Self::A => 'a',
            Self::B => 'b',
        }
    }
    pub fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }
}

/// Interruption points. In ordinary builds every method is a no-op; with the
/// `fault-injection` feature the binary kills itself (SIGKILL) at the point
/// named in `FLOWERLY_SLOT_FAULT`, which is what the KVM acceptance uses.
pub struct Faults {
    #[cfg(any(test, feature = "fault-injection"))]
    point: Option<String>,
    #[cfg(feature = "fault-injection")]
    kill: bool,
}

impl Faults {
    #[cfg(any(test, not(feature = "fault-injection")))]
    pub fn none() -> Self {
        Self {
            #[cfg(any(test, feature = "fault-injection"))]
            point: None,
            #[cfg(feature = "fault-injection")]
            kill: false,
        }
    }

    #[cfg(test)]
    pub fn error_at(point: &str) -> Self {
        Self {
            point: Some(point.to_owned()),
            #[cfg(feature = "fault-injection")]
            kill: false,
        }
    }

    #[cfg(feature = "fault-injection")]
    pub fn from_env() -> Self {
        Self {
            point: std::env::var("FLOWERLY_SLOT_FAULT").ok(),
            kill: true,
        }
    }

    pub fn at(&self, point: &str) -> Result<()> {
        #[cfg(any(test, feature = "fault-injection"))]
        if self.point.as_deref() == Some(point) {
            #[cfg(feature = "fault-injection")]
            if self.kill {
                // SAFETY: raising SIGKILL on ourselves has no memory effects.
                unsafe { libc::raise(libc::SIGKILL) };
            }
            bail!("injected fault at {point}");
        }
        let _ = point;
        Ok(())
    }

    fn at_chunk(&self, n: u64) -> Result<()> {
        #[cfg(any(test, feature = "fault-injection"))]
        if self.point.is_some() {
            return self.at(&format!("stage:chunk:{n}"));
        }
        let _ = n;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Part {
    pub name: String,
    pub offset: u64,
    pub size: u64,
    pub partuuid: String,
    pub sha256: String,
    pub filesystem: String,
}

/// The disk manifest. Unknown fields survive a rewrite untouched.
pub struct Disk {
    raw: Value,
    pub parts: Vec<Part>,
    pub size: u64,
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

impl Disk {
    pub fn load(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .with_context(|| format!("opening manifest {}", path.display()))?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() > MAX_MANIFEST {
            bail!("manifest must be a regular file of at most 1 MiB");
        }
        let mut bytes = Vec::new();
        file.take(MAX_MANIFEST).read_to_end(&mut bytes)?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let raw: Value = serde_json::from_slice(bytes).context("manifest is not JSON")?;
        if raw["schema"] != SCHEMA {
            bail!("unsupported disk manifest schema");
        }
        let size = raw["disk"]["size_bytes"]
            .as_u64()
            .ok_or_else(|| anyhow!("manifest disk size missing"))?;
        let list = raw["partitions"]
            .as_array()
            .ok_or_else(|| anyhow!("manifest partitions missing"))?;
        let mut parts = Vec::new();
        for item in list {
            let text = |key: &str| {
                item[key]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow!("manifest partition field {key} missing"))
            };
            let number = |key: &str| {
                item[key]
                    .as_u64()
                    .ok_or_else(|| anyhow!("manifest partition field {key} missing"))
            };
            parts.push(Part {
                name: text("name")?,
                offset: number("offset_bytes")?,
                size: number("size_bytes")?,
                partuuid: text("partuuid")?,
                sha256: text("sha256")?,
                filesystem: text("filesystem")?,
            });
        }
        let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        if names != ["esp", "root-a", "root-b", "data"] {
            bail!("manifest partitions must be esp, root-a, root-b, data in order");
        }
        let mut end = 0;
        for part in &parts {
            if part.offset < end
                || part.size < MIB
                || part.offset % MIB != 0
                || part.size % MIB != 0
                || part.offset + part.size > size
                || !is_uuid(&part.partuuid)
                || !is_hex(&part.sha256, 64)
            {
                bail!("manifest partition {} is malformed or overlaps", part.name);
            }
            end = part.offset + part.size;
        }
        let uuids: std::collections::BTreeSet<&str> =
            parts.iter().map(|p| p.partuuid.as_str()).collect();
        if uuids.len() != parts.len() {
            bail!("manifest partition identities are not unique");
        }
        Ok(Self { raw, parts, size })
    }

    pub fn part(&self, name: &str) -> &Part {
        self.parts
            .iter()
            .find(|p| p.name == name)
            .expect("validated")
    }

    pub fn slot(&self, slot: Slot) -> &Part {
        self.part(slot.name())
    }

    /// Resolve the running root from a claimed PARTUUID.
    pub fn active_slot(&self, partuuid: &str) -> Result<Slot> {
        let claimed = partuuid.to_ascii_lowercase();
        [Slot::A, Slot::B]
            .into_iter()
            .find(|s| self.slot(*s).partuuid == claimed)
            .ok_or_else(|| anyhow!("active root PARTUUID is not a known root slot"))
    }

    pub fn bind_gpt(&self, table: &[gpt::GptPartition]) -> Result<()> {
        if table.len() != self.parts.len() {
            bail!(
                "actual GPT has {} partitions, manifest has {}",
                table.len(),
                self.parts.len()
            );
        }
        for part in &self.parts {
            let real = table
                .iter()
                .find(|g| g.name == part.name)
                .ok_or_else(|| anyhow!("GPT lacks partition {}", part.name))?;
            if real.partuuid != part.partuuid
                || real.offset != part.offset
                || real.size != part.size
            {
                bail!("actual GPT differs from the manifest for {}", part.name);
            }
        }
        Ok(())
    }

    fn set_slot(&mut self, slot: Slot, sha256: &str, filesystem: &str) {
        let index = if slot == Slot::A { 1 } else { 2 };
        self.raw["partitions"][index]["sha256"] = json!(sha256);
        self.raw["partitions"][index]["filesystem"] = json!(filesystem);
        let part = &mut self.parts[index];
        part.sha256 = sha256.to_owned();
        part.filesystem = filesystem.to_owned();
    }

    /// Atomic replace: temp file, fsync, rename, fsync of the directory.
    fn save(&self, path: &Path, faults: &Faults) -> Result<()> {
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temp = tempfile::Builder::new()
            .prefix(".ab-manifest-")
            .tempfile_in(dir)?;
        std::io::Write::write_all(
            &mut temp,
            (serde_json::to_string_pretty(&self.raw)? + "\n").as_bytes(),
        )?;
        temp.as_file().sync_all()?;
        faults.at("stage:manifest-temp")?;
        temp.persist(path).map_err(|e| e.error)?;
        File::open(dir)?.sync_all()?;
        faults.at("stage:manifest-renamed")?;
        Ok(())
    }
}

/// An open, locked disk image. Only [`Image::write_slot`] writes, and only
/// inside the byte range of the partition it is handed.
pub struct Image {
    file: File,
    pub size: u64,
}

impl Image {
    pub fn open(path: &Path, write: bool) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(write)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .with_context(|| format!("opening disk image {}", path.display()))?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            bail!("refusing a block device or special file: the tool only edits disk images");
        }
        let op = if write { libc::LOCK_EX } else { libc::LOCK_SH } | libc::LOCK_NB;
        // SAFETY: flock on a valid descriptor owned by `file`.
        if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), op) } != 0 {
            bail!("disk image is in use by another process");
        }
        Ok(Self {
            file,
            size: meta.size(),
        })
    }

    pub fn hash(&self, offset: u64, len: u64) -> Result<String> {
        let mut digest = Sha256::new();
        let mut buf = vec![0u8; MIB as usize];
        let mut done = 0;
        while done < len {
            let take = (len - done).min(MIB) as usize;
            self.file.read_exact_at(&mut buf[..take], offset + done)?;
            digest.update(&buf[..take]);
            done += take as u64;
        }
        Ok(hex(&digest.finalize()))
    }

    fn write_slot(&self, part: &Part, relative: u64, data: &[u8]) -> Result<()> {
        if relative + data.len() as u64 > part.size {
            bail!("write outside partition {}", part.name);
        }
        self.file.write_all_at(data, part.offset + relative)?;
        Ok(())
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut file = open_regular(path)?;
    let mut digest = Sha256::new();
    let mut buf = vec![0u8; MIB as usize];
    let mut total = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok((hex(&digest.finalize()), total));
        }
        digest.update(&buf[..n]);
        total += n as u64;
    }
}

fn open_regular(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    if !file.metadata()?.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    Ok(file)
}

/// Open the image and bind manifest to the real GPT before anything else.
pub fn open_bound(disk: &Path, manifest: &Path, write: bool) -> Result<(Image, Disk)> {
    let image = Image::open(disk, write)?;
    let manifest = Disk::load(manifest)?;
    if manifest.size != image.size {
        bail!("disk image size differs from the manifest");
    }
    manifest.bind_gpt(&gpt::read(&image.file, image.size)?)?;
    Ok((image, manifest))
}

pub fn cmdline_root_partuuid() -> Result<String> {
    let cmdline = std::fs::read_to_string("/proc/cmdline").context("reading /proc/cmdline")?;
    let roots: Vec<&str> = cmdline
        .split_whitespace()
        .filter_map(|t| t.strip_prefix("root=PARTUUID="))
        .collect();
    match roots.as_slice() {
        [one] => Ok(one.to_ascii_lowercase()),
        _ => bail!("cannot determine the running root from /proc/cmdline; pass --active-partuuid"),
    }
}

fn verify_slot(image: &Image, part: &Part, what: &str) -> Result<()> {
    if image.hash(part.offset, part.size)? != part.sha256 {
        bail!("{what} {} does not match the manifest hash", part.name);
    }
    Ok(())
}

// ---- boot entries ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryState {
    Counting,
    Good,
    Bad,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub slot: Slot,
    pub state: EntryState,
    pub tries_left: Option<u32>,
    pub tries_done: u32,
}

/// `flowerly-<a|b>[+left[-done]].conf`, as systemd-boot renames it.
pub fn parse_entry(name: &str) -> Option<Entry> {
    let rest = name.strip_prefix("flowerly-")?.strip_suffix(".conf")?;
    let slot = match rest.chars().next()? {
        'a' => Slot::A,
        'b' => Slot::B,
        _ => return None,
    };
    let counter = &rest[1..];
    let (state, tries_left, tries_done) = if counter.is_empty() {
        (EntryState::Good, None, 0)
    } else {
        let counter = counter.strip_prefix('+')?;
        let (left, done) = match counter.split_once('-') {
            Some((l, d)) => (l, d),
            None => (counter, "0"),
        };
        let number = |s: &str| {
            (!s.is_empty() && s.len() <= 3 && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u32>().ok())
                .flatten()
        };
        let (left, done) = (number(left)?, number(done)?);
        let state = if left == 0 {
            EntryState::Bad
        } else {
            EntryState::Counting
        };
        (state, Some(left), done)
    };
    Some(Entry {
        name: name.to_owned(),
        slot,
        state,
        tries_left,
        tries_done,
    })
}

fn is_stray_temp(name: &str, slot: Slot) -> bool {
    name.starts_with(&format!("flowerly-{}", slot.letter())) && name.ends_with(".conf.tmp")
}

struct Listing {
    entries: Vec<Entry>,
    strays: Vec<String>,
}

fn list_entries(esp: &dyn Esp) -> Result<Listing> {
    let mut entries = Vec::new();
    let mut strays = Vec::new();
    for name in esp.list(ENTRIES)? {
        if let Some(entry) = parse_entry(&name) {
            entries.push(entry);
        } else if is_stray_temp(&name, Slot::A) || is_stray_temp(&name, Slot::B) {
            strays.push(name);
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    strays.sort();
    Ok(Listing { entries, strays })
}

fn check_loader_default(esp: &dyn Esp) -> Result<()> {
    let text = String::from_utf8_lossy(&esp.read("/loader/loader.conf")?).into_owned();
    if !text.lines().any(|l| l.trim() == LOADER_DEFAULT) {
        bail!(
            "loader.conf does not say '{LOADER_DEFAULT}'; boot counting could not fall back (rebuild the disk)"
        );
    }
    Ok(())
}

/// Build slot B's entry from the active slot's entry: same options, new root.
pub fn derive_entry(
    active_entry: &str,
    from_uuid: &str,
    to: Slot,
    to_uuid: &str,
    extra: &[String],
) -> Result<String> {
    let options = active_entry
        .lines()
        .find_map(|l| l.strip_prefix("options "))
        .ok_or_else(|| anyhow!("active entry has no options line"))?;
    let needle = format!("root=PARTUUID={from_uuid}");
    let mut tokens: Vec<String> = Vec::new();
    let mut replaced = 0;
    for token in options.split_whitespace() {
        if token == needle {
            tokens.push(format!("root=PARTUUID={to_uuid}"));
            replaced += 1;
        } else if token.starts_with("root=") {
            bail!("active entry has an unexpected root= option");
        } else {
            tokens.push(token.to_owned());
        }
    }
    if replaced != 1 {
        bail!("active entry must name exactly its own root PARTUUID once");
    }
    for option in extra {
        if option.is_empty()
            || !option
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._=,:/+-".contains(&b))
        {
            bail!("unsafe extra boot option {option:?}");
        }
        tokens.push(option.clone());
    }
    let l = to.letter();
    Ok(format!(
        "title Flowerly OS slot {}\nlinux /flowerly/{l}/vmlinuz\ninitrd /flowerly/{l}/initramfs.img\noptions {}\n",
        l.to_ascii_uppercase(),
        tokens.join(" ")
    ))
}

// ---- commands ---------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct StageRequest<'a> {
    pub manifest_path: &'a Path,
    pub active: Slot,
    pub target: Slot,
    pub candidate: &'a Path,
    pub expected_sha: &'a str,
}

pub fn stage(
    image: &Image,
    disk: &mut Disk,
    esp: &dyn Esp,
    request: &StageRequest,
    faults: &Faults,
) -> Result<Value> {
    let StageRequest {
        manifest_path,
        active,
        target,
        candidate,
        expected_sha,
    } = *request;
    if target == active {
        bail!("refusing to stage the active slot {}", active.name());
    }
    if !is_hex(expected_sha, 64) {
        bail!("expected SHA-256 must be 64 lowercase hex digits");
    }
    verify_slot(image, disk.slot(active), "active root")?;
    let listing = list_entries(esp)?;
    if listing.entries.iter().any(|e| e.slot == target)
        || listing.strays.iter().any(|s| is_stray_temp(s, target))
    {
        bail!(
            "{} has a boot entry; roll back before restaging it",
            target.name()
        );
    }
    let part = disk.slot(target).clone();
    let (digest, length) = hash_file(candidate)?;
    if length != part.size {
        bail!("candidate does not fit the exact root slot size");
    }
    if digest != expected_sha {
        bail!("candidate digest mismatch");
    }
    let source = open_regular(candidate)?;
    let mut magic = [0u8; 2];
    source.read_exact_at(&mut magic, 1080)?;
    if magic != EXT4_MAGIC {
        bail!("candidate is not an ext4 filesystem");
    }
    let mut buf = vec![0u8; MIB as usize];
    let mut written = 0;
    let mut chunk = 0;
    while written < part.size {
        source.read_exact_at(&mut buf, written)?;
        image.write_slot(&part, written, &buf)?;
        image.file.sync_data()?;
        written += MIB;
        chunk += 1;
        faults.at_chunk(chunk)?;
    }
    faults.at("stage:before-readback")?;
    if image.hash(part.offset, part.size)? != expected_sha {
        bail!("staged slot readback mismatch");
    }
    disk.set_slot(target, expected_sha, "ext4");
    let disk_hash = image.hash(0, image.size)?;
    disk.raw["disk"]["sha256"] = json!(disk_hash);
    faults.at("stage:before-manifest")?;
    disk.save(manifest_path, faults)?;
    Ok(json!({
        "status": "STAGED_NOT_ARMED",
        "target": target.name(),
        "candidate_sha256": expected_sha,
        "disk_sha256": disk_hash,
    }))
}

pub struct ArmRequest<'a> {
    pub kernel: &'a Path,
    pub initramfs: &'a Path,
    pub tries: u32,
    pub extra_options: &'a [String],
}

pub fn arm(
    image: &Image,
    disk: &Disk,
    active: Slot,
    target: Slot,
    esp: &mut dyn Esp,
    request: &ArmRequest,
    faults: &Faults,
) -> Result<Value> {
    if target == active {
        bail!("refusing to arm the active slot {}", active.name());
    }
    if !(1..=9).contains(&request.tries) {
        bail!("boot-count tries must be 1..=9");
    }
    verify_slot(image, disk.slot(active), "active root")?;
    let part = disk.slot(target);
    if part.filesystem != "ext4" {
        bail!("{} has not been staged", target.name());
    }
    verify_slot(image, part, "staged root")?;
    let listing = list_entries(&*esp)?;
    if listing.entries.iter().any(|e| e.slot == target) {
        bail!(
            "{} already has a boot entry; roll back first",
            target.name()
        );
    }
    let active_entry = listing
        .entries
        .iter()
        .find(|e| e.slot == active)
        .ok_or_else(|| anyhow!("no boot entry for the active slot; nothing to fall back to"))?;
    check_loader_default(&*esp)?;
    let entry_text = derive_entry(
        &String::from_utf8_lossy(&esp.read(&format!("{ENTRIES}/{}", active_entry.name))?),
        &disk.slot(active).partuuid,
        target,
        &part.partuuid,
        request.extra_options,
    )?;
    // An earlier interrupted arm may have left a temp entry; it is not a boot
    // entry, so it can be dropped and the arm re-run.
    for stray in listing.strays.iter().filter(|s| is_stray_temp(s, target)) {
        esp.delete(&format!("{ENTRIES}/{stray}"))?;
    }
    let dir = format!("/flowerly/{}", target.letter());
    if !esp.exists(&dir)? {
        esp.mkdir(&dir)?;
    }
    for (path, file) in [
        (format!("{dir}/vmlinuz"), request.kernel),
        (format!("{dir}/initramfs.img"), request.initramfs),
    ] {
        let data = read_regular(file)?;
        if esp.exists(&path)? {
            esp.delete(&path)?;
        }
        esp.write_new(&path, &data)?;
        if esp.read(&path)? != data {
            bail!("{path} read back differently from what was written");
        }
    }
    esp.sync()?;
    faults.at("arm:after-payload")?;
    let entry = format!("flowerly-{}+{}.conf", target.letter(), request.tries);
    let temp = format!("{ENTRIES}/{entry}.tmp");
    esp.write_new(&temp, entry_text.as_bytes())?;
    esp.sync()?;
    faults.at("arm:after-tmp-entry")?;
    esp.rename(&temp, &format!("{ENTRIES}/{entry}"))?;
    esp.sync()?;
    faults.at("arm:after-rename")?;
    Ok(json!({
        "status": "ARMED_PENDING",
        "target": target.name(),
        "boot_entry": entry,
        "note": "pending only: success needs a witnessed boot and the health gate",
    }))
}

fn read_regular(path: &Path) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    open_regular(path)?.read_to_end(&mut data)?;
    Ok(data)
}

pub fn rollback(
    image: &Image,
    disk: &Disk,
    to: Slot,
    esp: &mut dyn Esp,
    faults: &Faults,
) -> Result<Value> {
    verify_slot(image, disk.slot(to), "rollback target root")?;
    let listing = list_entries(&*esp)?;
    if !listing.entries.iter().any(|e| e.slot == to) {
        bail!(
            "no boot entry for {}; refusing to leave no bootable entry",
            to.name()
        );
    }
    let other = to.other();
    let doomed: Vec<String> = listing
        .entries
        .iter()
        .filter(|e| e.slot == other)
        .map(|e| e.name.clone())
        .chain(
            listing
                .strays
                .iter()
                .filter(|s| is_stray_temp(s, other))
                .cloned(),
        )
        .collect();
    if doomed.is_empty() {
        bail!("{} has no boot entry; nothing to roll back", other.name());
    }
    check_loader_default(&*esp)?;
    for (n, name) in doomed.iter().enumerate() {
        esp.delete(&format!("{ENTRIES}/{name}"))?;
        esp.sync()?;
        faults.at(&format!("rollback:after-delete:{}", n + 1))?;
    }
    Ok(json!({
        "status": "ROLLED_BACK",
        "to": to.name(),
        "removed_entries": doomed,
    }))
}

pub fn status(
    image: &Image,
    disk: &Disk,
    esp: &dyn Esp,
    witness: Option<&Path>,
    verify_slots: bool,
) -> Result<Value> {
    let listing = list_entries(esp)?;
    let loader_default_ok = check_loader_default(esp).is_ok();
    let slots: Vec<Value> = [Slot::A, Slot::B]
        .into_iter()
        .map(|slot| {
            let part = disk.slot(slot);
            let image_state = if !verify_slots {
                "NOT_VERIFIED"
            } else if image.hash(part.offset, part.size).ok().as_deref() == Some(&part.sha256) {
                "MATCHES_MANIFEST"
            } else {
                "MODIFIED"
            };
            json!({
                "slot": slot.name(),
                "partuuid": part.partuuid,
                "manifest_filesystem": part.filesystem,
                "image_state": image_state,
            })
        })
        .collect();
    let last_witness = match witness {
        Some(path) => read_witness(path)?,
        None => None,
    };
    let witnessed = last_witness
        .as_ref()
        .and_then(|w| w["slot"].as_str())
        .and_then(|s| Slot::parse(s).ok());
    let counting_or_bad = listing.entries.iter().find(|e| e.state != EntryState::Good);
    let verdict = match counting_or_bad {
        Some(entry) if entry.state == EntryState::Counting => "PENDING",
        Some(entry) => match witnessed {
            Some(slot) if slot != entry.slot => "FELL_BACK",
            Some(_) => "FAILED_UNRESOLVED",
            None => "FELL_BACK_UNCONFIRMED",
        },
        None => {
            let slots_with_entries: std::collections::BTreeSet<char> =
                listing.entries.iter().map(|e| e.slot.letter()).collect();
            match slots_with_entries.len() {
                0 => "NO_BOOT_ENTRIES",
                1 => "NO_CANDIDATE",
                _ if witnessed.is_some() => "ACTIVE",
                _ => "ACTIVE_UNCONFIRMED",
            }
        }
    };
    Ok(json!({
        "verdict": verdict,
        "pending_slots": listing.entries.iter()
            .filter(|e| e.state == EntryState::Counting)
            .map(|e| e.slot.name())
            .collect::<Vec<_>>(),
        "bad_slots": listing.entries.iter()
            .filter(|e| e.state == EntryState::Bad)
            .map(|e| e.slot.name())
            .collect::<Vec<_>>(),
        "witness_slot": witnessed.map(Slot::name),
        "last_witness": last_witness,
        "loader_default_ok": loader_default_ok,
        "entries": listing.entries.iter().map(|e| json!({
            "name": e.name,
            "slot": e.slot.name(),
            "state": match e.state {
                EntryState::Counting => "COUNTING",
                EntryState::Good => "GOOD",
                EntryState::Bad => "BAD",
            },
            "tries_left": e.tries_left,
            "tries_done": e.tries_done,
        })).collect::<Vec<_>>(),
        "stray_temp_entries": listing.strays,
        "slots": slots,
        "rule": "success is never inferred from boot entries alone; ACTIVE needs the witness ledger",
    }))
}

fn read_witness(path: &Path) -> Result<Option<Value>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("opening witness ledger"),
    };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > MAX_WITNESS {
        bail!("witness ledger must be a regular file of at most 2 MiB");
    }
    let mut bytes = Vec::new();
    file.take(MAX_WITNESS).read_to_end(&mut bytes)?;
    // A record without its newline was cut by power loss; it is not evidence.
    let complete = match bytes.iter().rposition(|b| *b == b'\n') {
        Some(end) => &bytes[..end],
        None => return Ok(None),
    };
    match complete.rsplit(|b| *b == b'\n').find(|l| !l.is_empty()) {
        Some(line) => Ok(Some(
            serde_json::from_slice(line).context("last witness record is not JSON")?,
        )),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests;
