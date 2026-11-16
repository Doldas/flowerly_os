//! Does a built rootfs contain exactly the frozen package set? (#49,
//! ADR-0020 step 7.)
//!
//! pacman records every installed package under
//! `<rootfs>/var/lib/pacman/local/<name>-<version>/desc`. This reads those
//! records (no pacman, no network, nothing is followed through a symlink) and
//! compares name, version and architecture with the manifest. A rootfs that
//! was resolved from a rolling mirror differs from the manifest by version;
//! one built from the manifest's snapshot must match it exactly.
//!
//! It proves the installed *package identities*, not the bytes on disk; the
//! archive bytes were proven by `verify-cache` and the build is a separate
//! step. Reads only.

use serde::Serialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::manifest::Checked;

const LOCAL_DB: &str = "var/lib/pacman/local";
const MAX_ENTRIES: usize = 20_000;
const MAX_DESC: u64 = 1 << 20;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct VersionMismatch {
    pub name: String,
    pub expected: String,
    pub installed: String,
}

#[derive(Debug, Serialize)]
pub struct RootfsReport {
    pub manifest_sha256: String,
    pub installed: usize,
    /// In the manifest, not installed.
    pub missing: Vec<String>,
    /// Installed, not in the manifest.
    pub unexpected: Vec<String>,
    pub version_mismatch: Vec<VersionMismatch>,
    pub architecture_mismatch: Vec<String>,
    /// Records that could not be read or parsed; any entry fails the report.
    pub unreadable: Vec<String>,
}

impl RootfsReport {
    pub fn matches(&self) -> bool {
        self.missing.is_empty()
            && self.unexpected.is_empty()
            && self.version_mismatch.is_empty()
            && self.architecture_mismatch.is_empty()
            && self.unreadable.is_empty()
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Installed {
    name: String,
    version: String,
    architecture: String,
}

/// The `%NAME%`, `%VERSION%` and `%ARCH%` of one `desc`, each exactly once.
fn parse_desc(text: &str) -> Result<Installed, String> {
    let (mut name, mut version, mut architecture) = (None, None, None);
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let slot = match line {
            "%NAME%" => &mut name,
            "%VERSION%" => &mut version,
            "%ARCH%" => &mut architecture,
            _ => continue,
        };
        let value = lines
            .next()
            .filter(|value| !value.is_empty() && !value.starts_with('%'))
            .ok_or_else(|| format!("{line} has no value"))?;
        if slot.replace(value.to_owned()).is_some() {
            return Err(format!("{line} appears twice"));
        }
    }
    match (name, version, architecture) {
        (Some(name), Some(version), Some(architecture)) => Ok(Installed {
            name,
            version,
            architecture,
        }),
        _ => Err("missing %NAME%, %VERSION% or %ARCH%".into()),
    }
}

fn read_desc(path: &Path) -> Result<String, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_DESC {
        return Err("not a regular file of reasonable size".into());
    }
    let mut text = String::new();
    file.take(MAX_DESC)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    Ok(text)
}

/// Compares the packages installed in `rootfs` with the manifest.
pub fn verify_installed(checked: &Checked, rootfs: &Path) -> RootfsReport {
    let mut report = RootfsReport {
        manifest_sha256: checked.sha256.clone(),
        installed: 0,
        missing: Vec::new(),
        unexpected: Vec::new(),
        version_mismatch: Vec::new(),
        architecture_mismatch: Vec::new(),
        unreadable: Vec::new(),
    };
    let database = rootfs.join(LOCAL_DB);
    let mut installed = BTreeMap::new();
    match std::fs::read_dir(&database) {
        Err(error) => report
            .unreadable
            .push(format!("{}: {error}", database.display())),
        Ok(entries) => {
            for (count, entry) in entries.enumerate() {
                if count >= MAX_ENTRIES {
                    report.unreadable.push("too many package records".into());
                    break;
                }
                let Ok(entry) = entry else {
                    report.unreadable.push("unreadable directory entry".into());
                    continue;
                };
                let dir = entry.file_name().to_string_lossy().into_owned();
                // `ALPM_DB_VERSION` and similar files are not package records.
                if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    if dir != "ALPM_DB_VERSION" {
                        report
                            .unreadable
                            .push(format!("{dir}: not a package record"));
                    }
                    continue;
                }
                let record = read_desc(&entry.path().join("desc"))
                    .and_then(|text| parse_desc(&text))
                    .and_then(|package| {
                        // pacman names the record `<name>-<version>`.
                        if dir == format!("{}-{}", package.name, package.version) {
                            Ok(package)
                        } else {
                            Err(format!(
                                "record name does not match its desc ({})",
                                package.name
                            ))
                        }
                    });
                match record {
                    Ok(package) => {
                        if installed.insert(package.name.clone(), package).is_some() {
                            report
                                .unreadable
                                .push(format!("{dir}: package installed twice"));
                        }
                    }
                    Err(problem) => report.unreadable.push(format!("{dir}: {problem}")),
                }
            }
        }
    }
    report.installed = installed.len();
    for package in &checked.manifest.packages {
        match installed.remove(&package.name) {
            None => report.missing.push(package.name.clone()),
            Some(found) => {
                if found.version != package.version {
                    report.version_mismatch.push(VersionMismatch {
                        name: package.name.clone(),
                        expected: package.version.clone(),
                        installed: found.version,
                    });
                }
                if found.architecture != package.architecture {
                    report.architecture_mismatch.push(package.name.clone());
                }
            }
        }
    }
    report.unexpected = installed.into_keys().collect();
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{canonical_bytes, check, Manifest, Package, Source};

    fn package(name: &str, version: &str, architecture: &str, serial: u8) -> Package {
        Package {
            repository: "core".into(),
            name: name.into(),
            version: version.into(),
            architecture: architecture.into(),
            filename: format!("{name}-{version}-{architecture}.pkg.tar.zst"),
            size: 1,
            sha256: format!("{serial:02x}").repeat(32),
            signature_sha256: format!("{:02x}", serial + 100).repeat(32),
            signer_fingerprint: "0123456789ABCDEF0123456789ABCDEF01234567".into(),
        }
    }

    fn checked(packages: Vec<Package>) -> Checked {
        let manifest = Manifest {
            schema: crate::manifest::SCHEMA.into(),
            source: Source {
                snapshot_url: "https://archive.archlinux.org/repos/2026/11/09/".into(),
                snapshot_date: "2026-11-09".into(),
                architecture: "x86_64".into(),
            },
            requested: vec!["bash".into()],
            packages,
        };
        // `check` validates shape only; equality with packages.base is the
        // CLI's.
        let bytes = canonical_bytes(&manifest);
        match check(&bytes) {
            Ok(checked) => checked,
            Err(errors) => panic!("fixture manifest invalid: {errors:?}"),
        }
    }

    fn rootfs(records: &[(&str, &str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join(LOCAL_DB);
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("ALPM_DB_VERSION"), "9\n").unwrap();
        for (name, version, architecture) in records {
            let record = local.join(format!("{name}-{version}"));
            std::fs::create_dir(&record).unwrap();
            std::fs::write(
                record.join("desc"),
                format!(
                    "%NAME%\n{name}\n\n%VERSION%\n{version}\n\n%ARCH%\n{architecture}\n\n%DESC%\nx\n"
                ),
            )
            .unwrap();
        }
        dir
    }

    fn fixture() -> Checked {
        checked(vec![
            package("tzdata", "2026a-1", "any", 3),
            package("bash", "5.3-1", "x86_64", 1),
            package("glibc", "1:2.40-3", "x86_64", 2),
        ])
    }

    #[test]
    fn an_exact_match_is_accepted() {
        let dir = rootfs(&[
            ("bash", "5.3-1", "x86_64"),
            ("glibc", "1:2.40-3", "x86_64"),
            ("tzdata", "2026a-1", "any"),
        ]);
        let report = verify_installed(&fixture(), dir.path());
        assert!(report.matches(), "{report:?}");
        assert_eq!(report.installed, 3);
    }

    #[test]
    fn a_rolling_mirror_build_differs_by_version() {
        let dir = rootfs(&[
            ("bash", "5.3-2", "x86_64"),
            ("glibc", "1:2.40-3", "x86_64"),
            ("tzdata", "2026a-1", "any"),
        ]);
        let report = verify_installed(&fixture(), dir.path());
        assert!(!report.matches());
        assert_eq!(
            report.version_mismatch,
            vec![VersionMismatch {
                name: "bash".into(),
                expected: "5.3-1".into(),
                installed: "5.3-2".into()
            }]
        );
    }

    #[test]
    fn missing_extra_and_wrong_architecture_packages_fail() {
        let dir = rootfs(&[
            ("bash", "5.3-1", "x86_64"),
            ("tzdata", "2026a-1", "x86_64"),
            ("mkinitcpio", "40-1", "any"),
        ]);
        let report = verify_installed(&fixture(), dir.path());
        assert_eq!(report.missing, ["glibc"]);
        assert_eq!(report.unexpected, ["mkinitcpio"]);
        assert_eq!(report.architecture_mismatch, ["tzdata"]);
        assert!(!report.matches());
    }

    #[test]
    fn an_absent_database_or_a_broken_record_never_looks_like_a_match() {
        let empty = tempfile::tempdir().unwrap();
        let report = verify_installed(&fixture(), empty.path());
        assert!(!report.matches() && !report.unreadable.is_empty());

        let dir = rootfs(&[
            ("bash", "5.3-1", "x86_64"),
            ("glibc", "1:2.40-3", "x86_64"),
            ("tzdata", "2026a-1", "any"),
        ]);
        let local = dir.path().join(LOCAL_DB);
        std::fs::write(local.join("bash-5.3-1/desc"), "%NAME%\nbash\n").unwrap();
        let report = verify_installed(&fixture(), dir.path());
        assert!(!report.matches());
        assert!(report
            .unreadable
            .iter()
            .any(|item| item.starts_with("bash-5.3-1")));
        assert!(report.missing.contains(&"bash".to_owned()));
    }

    #[test]
    fn a_record_whose_name_disagrees_with_its_desc_is_unreadable() {
        let dir = rootfs(&[
            ("bash", "5.3-1", "x86_64"),
            ("glibc", "1:2.40-3", "x86_64"),
            ("tzdata", "2026a-1", "any"),
        ]);
        let local = dir.path().join(LOCAL_DB);
        std::fs::rename(local.join("tzdata-2026a-1"), local.join("other-1-1")).unwrap();
        let report = verify_installed(&fixture(), dir.path());
        assert!(!report.matches());
        assert!(report
            .unreadable
            .iter()
            .any(|item| item.starts_with("other-1-1")));
    }

    #[test]
    fn a_symlinked_desc_is_not_followed() {
        let dir = rootfs(&[
            ("bash", "5.3-1", "x86_64"),
            ("glibc", "1:2.40-3", "x86_64"),
            ("tzdata", "2026a-1", "any"),
        ]);
        let record = dir.path().join(LOCAL_DB).join("bash-5.3-1");
        std::fs::rename(record.join("desc"), dir.path().join("real-desc")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("real-desc"), record.join("desc")).unwrap();
        let report = verify_installed(&fixture(), dir.path());
        assert!(!report.matches());
        assert!(report
            .unreadable
            .iter()
            .any(|item| item.starts_with("bash-5.3-1")));
    }

    #[test]
    fn desc_parsing_is_strict() {
        assert!(parse_desc("%NAME%\na\n%VERSION%\n1-1\n%ARCH%\nany\n").is_ok());
        for bad in [
            "%NAME%\na\n%NAME%\nb\n%VERSION%\n1-1\n%ARCH%\nany\n",
            "%NAME%\n\n%VERSION%\n1-1\n%ARCH%\nany\n",
            "%NAME%\n%VERSION%\n1-1\n%ARCH%\nany\n",
            "%VERSION%\n1-1\n%ARCH%\nany\n",
        ] {
            assert!(parse_desc(bad).is_err(), "{bad:?}");
        }
    }
}
