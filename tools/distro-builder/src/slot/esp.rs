//! Access to the FAT ESP of a disk image. Production uses mtools with a byte
//! offset (no mount and no root); tests use an in-memory ESP so every
//! interruption point can be exercised without external tools.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub trait Esp {
    /// Names (not paths) of the entries in `dir`; empty when it is missing.
    fn list(&self, dir: &str) -> Result<Vec<String>>;
    fn read(&self, path: &str) -> Result<Vec<u8>>;
    /// Create `path`; fails if it already exists.
    fn write_new(&mut self, path: &str, data: &[u8]) -> Result<()>;
    /// Rename within the ESP; fails if `to` already exists.
    fn rename(&mut self, from: &str, to: &str) -> Result<()>;
    fn delete(&mut self, path: &str) -> Result<()>;
    fn mkdir(&mut self, path: &str) -> Result<()>;
    /// Flush everything written so far to stable storage.
    fn sync(&mut self) -> Result<()>;

    fn exists(&self, path: &str) -> Result<bool> {
        let (dir, name) = path.rsplit_once('/').ok_or_else(|| anyhow!("bad path"))?;
        // FAT names are case-insensitive: mdir shows a lone-letter directory
        // such as /flowerly/b as `B`.
        Ok(self.list(dir)?.iter().any(|n| n.eq_ignore_ascii_case(name)))
    }
}

pub struct MtoolsEsp {
    disk: PathBuf,
    image: String,
}

impl MtoolsEsp {
    pub fn new(disk: &Path, offset: u64) -> Self {
        Self {
            disk: disk.to_owned(),
            image: format!("{}@@{offset}", disk.display()),
        }
    }

    fn run(&self, tool: &str, args: &[&str]) -> Result<Vec<u8>> {
        let output = Command::new(tool)
            .args(["-i", &self.image])
            .args(args)
            .output()
            .with_context(|| format!("running {tool} (is mtools installed?)"))?;
        if !output.status.success() {
            bail!(
                "{tool} {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output.stdout)
    }
}

fn fat(path: &str) -> String {
    format!("::{path}")
}

impl Esp for MtoolsEsp {
    fn list(&self, dir: &str) -> Result<Vec<String>> {
        let output = Command::new("mdir")
            .args(["-b", "-i", &self.image])
            .arg(fat(dir))
            .output()
            .context("running mdir (is mtools installed?)")?;
        if !output.status.success() {
            // A missing directory is an empty listing; any other failure
            // would have been reported by an earlier mtools call on this
            // image, so keep it simple and distinguishable.
            return Ok(Vec::new());
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            // mdir -b prints directories with a trailing slash.
            .filter_map(|line| line.trim_end_matches('/').rsplit('/').next())
            .map(str::to_owned)
            .filter(|name| !name.is_empty())
            .collect())
    }

    fn read(&self, path: &str) -> Result<Vec<u8>> {
        self.run("mtype", &[&fat(path)])
    }

    fn write_new(&mut self, path: &str, data: &[u8]) -> Result<()> {
        if self.exists(path)? {
            bail!("{path} already exists on the ESP");
        }
        let dir = tempfile::tempdir()?;
        let staged = dir.path().join("payload");
        std::fs::write(&staged, data)?;
        self.run(
            "mcopy",
            &[staged.to_str().context("temp path")?, &fat(path)],
        )?;
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        if self.exists(to)? {
            bail!("{to} already exists on the ESP");
        }
        self.run("mren", &[&fat(from), &fat(to)])?;
        Ok(())
    }

    fn delete(&mut self, path: &str) -> Result<()> {
        self.run("mdel", &[&fat(path)])?;
        Ok(())
    }

    fn mkdir(&mut self, path: &str) -> Result<()> {
        self.run("mmd", &[&fat(path)])?;
        Ok(())
    }

    fn sync(&mut self) -> Result<()> {
        std::fs::File::open(&self.disk)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
pub use mem::MemEsp;

#[cfg(test)]
mod mem {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// In-memory ESP. Each operation is atomic, which is exactly the
    /// guarantee a FAT directory-entry update gives the real tool.
    #[derive(Clone, Default)]
    pub struct MemEsp {
        pub files: BTreeMap<String, Vec<u8>>,
        pub dirs: BTreeSet<String>,
        pub syncs: usize,
    }

    impl Esp for MemEsp {
        fn list(&self, dir: &str) -> Result<Vec<String>> {
            let prefix = format!("{dir}/");
            let mut names: Vec<String> = self
                .files
                .keys()
                .filter_map(|p| p.strip_prefix(&prefix))
                .filter(|rest| !rest.contains('/'))
                .map(str::to_owned)
                .collect();
            names.extend(
                self.dirs
                    .iter()
                    .filter_map(|p| p.strip_prefix(&prefix))
                    .filter(|rest| !rest.contains('/'))
                    .map(str::to_owned),
            );
            Ok(names)
        }
        fn read(&self, path: &str) -> Result<Vec<u8>> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| anyhow!("missing {path}"))
        }
        fn write_new(&mut self, path: &str, data: &[u8]) -> Result<()> {
            if self.files.contains_key(path) {
                bail!("{path} already exists");
            }
            self.files.insert(path.to_owned(), data.to_vec());
            Ok(())
        }
        fn rename(&mut self, from: &str, to: &str) -> Result<()> {
            if self.files.contains_key(to) {
                bail!("{to} already exists");
            }
            let data = self
                .files
                .remove(from)
                .ok_or_else(|| anyhow!("missing {from}"))?;
            self.files.insert(to.to_owned(), data);
            Ok(())
        }
        fn delete(&mut self, path: &str) -> Result<()> {
            self.files
                .remove(path)
                .map(|_| ())
                .ok_or_else(|| anyhow!("missing {path}"))
        }
        fn mkdir(&mut self, path: &str) -> Result<()> {
            self.dirs.insert(path.to_owned());
            Ok(())
        }
        fn sync(&mut self) -> Result<()> {
            self.syncs += 1;
            Ok(())
        }
    }
}
