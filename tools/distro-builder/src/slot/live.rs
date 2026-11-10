//! The slot broker's view of a running system (ADR-0023 G4): the real disk,
//! opened read-only, and the mounted ESP. The broker never writes the block
//! device; its only writes are boot entries on the ESP, made through
//! [`DirEsp`], which never follows a symlink and never leaves the ESP root.

use anyhow::{anyhow, bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use super::esp::Esp;
use super::Image;

impl Image {
    /// Open a block device or disk image read-only (never for writing).
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .with_context(|| format!("opening disk {}", path.display()))?;
        let meta = file.metadata()?;
        let size = if meta.is_file() {
            meta.len()
        } else if meta.file_type().is_block_device() {
            file.seek(SeekFrom::End(0))?
        } else {
            bail!(
                "{} is neither a block device nor a disk image",
                path.display()
            );
        };
        Ok(Self { file, size })
    }
}

/// Bind the manifest to the real GPT of a read-only disk.
pub fn open_bound_read_only(disk: &Path, manifest: &Path) -> Result<(Image, super::Disk)> {
    let image = Image::open_read_only(disk)?;
    let manifest = super::Disk::load(manifest)?;
    if manifest.size != image.size {
        bail!("disk size differs from the manifest");
    }
    manifest.bind_gpt(&super::gpt::read(&image.file, image.size)?)?;
    Ok((image, manifest))
}

const MAX_ESP_FILE: u64 = 256 * 1024 * 1024;

/// A mounted ESP directory. Paths are absolute within the ESP (`/loader/...`).
pub struct DirEsp {
    root: PathBuf,
}

impl DirEsp {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
        }
    }

    /// The real path of `path`, refusing anything but plain names and any
    /// existing symlink on the way (the last component may be missing).
    fn resolve(&self, path: &str) -> Result<PathBuf> {
        let relative = path
            .strip_prefix('/')
            .ok_or_else(|| anyhow!("ESP path {path:?} must be absolute"))?;
        if relative.is_empty() || relative.split('/').any(|part| part.is_empty()) {
            bail!("ESP path {path:?} is malformed");
        }
        let mut real = self.root.clone();
        for component in Path::new(relative).components() {
            let Component::Normal(name) = component else {
                bail!("ESP path {path:?} leaves the ESP");
            };
            real.push(name);
            match std::fs::symlink_metadata(&real) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    bail!("ESP path {path:?} goes through a symlink")
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(real)
    }

    fn sync_parent(path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("no parent directory"))?;
        File::open(parent)?.sync_all()?;
        Ok(())
    }
}

impl Esp for DirEsp {
    fn list(&self, dir: &str) -> Result<Vec<String>> {
        let real = self.resolve(dir)?;
        let entries = match std::fs::read_dir(&real) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut names = Vec::new();
        for entry in entries {
            let name = entry?.file_name();
            names.push(
                name.into_string()
                    .map_err(|_| anyhow!("non-UTF-8 name on the ESP"))?,
            );
        }
        Ok(names)
    }

    fn read(&self, path: &str) -> Result<Vec<u8>> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self.resolve(path)?)
            .with_context(|| format!("reading {path} on the ESP"))?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() > MAX_ESP_FILE {
            bail!("{path} is not a regular ESP file of a sane size");
        }
        let mut data = Vec::new();
        file.take(MAX_ESP_FILE).read_to_end(&mut data)?;
        Ok(data)
    }

    fn write_new(&mut self, path: &str, data: &[u8]) -> Result<()> {
        let real = self.resolve(path)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&real)
            .with_context(|| format!("creating {path} on the ESP"))?;
        file.write_all(data)?;
        file.sync_all()?;
        Self::sync_parent(&real)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let source = self.resolve(from)?;
        let target = self.resolve(to)?;
        if self.exists(to)? {
            bail!("{to} already exists on the ESP");
        }
        std::fs::rename(&source, &target)?;
        Self::sync_parent(&target)?;
        Self::sync_parent(&source)
    }

    fn delete(&mut self, path: &str) -> Result<()> {
        let real = self.resolve(path)?;
        if !std::fs::symlink_metadata(&real)?.is_file() {
            bail!("{path} is not a regular ESP file");
        }
        std::fs::remove_file(&real)?;
        Self::sync_parent(&real)
    }

    fn mkdir(&mut self, path: &str) -> Result<()> {
        let real = self.resolve(path)?;
        std::fs::create_dir(&real)?;
        Self::sync_parent(&real)
    }

    fn sync(&mut self) -> Result<()> {
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slot::esp::Esp;

    fn esp() -> (tempfile::TempDir, DirEsp) {
        let dir = tempfile::tempdir().unwrap();
        let esp = DirEsp::new(dir.path());
        (dir, esp)
    }

    #[test]
    fn files_are_created_once_renamed_without_overwrite_and_listed() {
        let (_dir, mut esp) = esp();
        esp.mkdir("/loader").unwrap();
        esp.mkdir("/loader/entries").unwrap();
        esp.write_new("/loader/entries/a.tmp", b"entry").unwrap();
        assert!(esp.write_new("/loader/entries/a.tmp", b"again").is_err());
        assert_eq!(esp.read("/loader/entries/a.tmp").unwrap(), b"entry");
        esp.write_new("/loader/entries/b.conf", b"other").unwrap();
        assert!(esp
            .rename("/loader/entries/a.tmp", "/loader/entries/b.conf")
            .is_err());
        esp.rename("/loader/entries/a.tmp", "/loader/entries/a.conf")
            .unwrap();
        let mut names = esp.list("/loader/entries").unwrap();
        names.sort();
        assert_eq!(names, ["a.conf", "b.conf"]);
        assert!(esp.list("/missing").unwrap().is_empty());
        esp.delete("/loader/entries/b.conf").unwrap();
        assert!(!esp.exists("/loader/entries/b.conf").unwrap());
        esp.sync().unwrap();
    }

    #[test]
    fn paths_cannot_leave_the_esp() {
        let (_dir, mut esp) = esp();
        for path in ["loader/x", "/../x", "/a/../x", "/a//x", "/./x", "/"] {
            assert!(esp.write_new(path, b"x").is_err(), "{path}");
            assert!(esp.read(path).is_err(), "{path}");
        }
    }

    #[test]
    fn symlinks_are_never_followed() {
        let (dir, mut esp) = esp();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("file")).unwrap();
        assert!(esp.read("/link/secret").is_err());
        assert!(esp.read("/file").is_err());
        assert!(esp.write_new("/link/new", b"x").is_err());
        assert!(esp.delete("/file").is_err());
        assert!(!outside.path().join("new").exists());
        assert!(outside.path().join("secret").exists());
    }

    #[test]
    fn a_device_or_image_is_opened_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.img");
        std::fs::write(&path, vec![7u8; 4096]).unwrap();
        let disk = Image::open_read_only(&path).unwrap();
        assert_eq!(disk.size, 4096);
        assert_eq!(
            disk.hash(0, 4096).unwrap(),
            crate::slot::hex(&{
                use sha2::Digest;
                sha2::Sha256::digest(vec![7u8; 4096])
            })
        );
        assert!(
            Image::open_read_only(dir.path()).is_err(),
            "a directory is not a disk"
        );
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(Image::open_read_only(&link).is_err());
    }
}
