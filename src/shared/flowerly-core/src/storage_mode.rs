//! Typed, side-effect-free storage-mode contract for future installers.
//!
//! No daemon switches its database topology based on this type yet. Invalid
//! configuration must never silently enable cluster replication or delete data.
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::str::FromStr;

pub const SYSTEM_MODE_PATH: &str = "/etc/flowerly/mode";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageMode {
    #[default]
    Standalone,
    Cluster,
    CoreOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeError {
    InvalidMode,
    InvalidEncoding,
    ReadFailed,
    UnsupportedTransition,
}

impl fmt::Display for ModeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMode => formatter.write_str("invalid storage mode"),
            Self::InvalidEncoding => formatter.write_str("invalid mode file encoding"),
            Self::ReadFailed => formatter.write_str("failed to read mode file"),
            Self::UnsupportedTransition => {
                formatter.write_str("storage mode transition requires explicit migration")
            }
        }
    }
}

impl std::error::Error for ModeError {}

impl FromStr for StorageMode {
    type Err = ModeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "standalone" => Ok(Self::Standalone),
            "cluster" => Ok(Self::Cluster),
            "core_only" => Ok(Self::CoreOnly),
            _ => Err(ModeError::InvalidMode),
        }
    }
}

impl StorageMode {
    /// No implicit transitions that could change database ownership.
    /// Re-selecting the already configured mode is always safe.
    pub fn validate_transition(self, next: Self) -> Result<(), ModeError> {
        if self == next {
            Ok(())
        } else {
            Err(ModeError::UnsupportedTransition)
        }
    }

    /// Cluster replication remains unavailable until it is implemented and verified.
    #[must_use]
    pub const fn allows_replication(self) -> bool {
        false
    }

    #[must_use]
    pub const fn opens_remote_listener(self) -> bool {
        false
    }
}

/// A missing mode file means standalone for backwards compatibility. Any other
/// read/parse failure blocks activation of new storage features.
pub fn read_mode_file(path: &Path) -> Result<StorageMode, ModeError> {
    // Linux O_NOFOLLOW: do not follow an attacker-replaced symlink at open time.
    const O_NOFOLLOW: i32 = 0o400000;
    const MAX_MODE_BYTES: u64 = 64;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).custom_flags(O_NOFOLLOW);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StorageMode::Standalone);
        }
        Err(_) => return Err(ModeError::ReadFailed),
    };
    let metadata = file.metadata().map_err(|_| ModeError::ReadFailed)?;
    if !metadata.is_file() || metadata.len() > MAX_MODE_BYTES {
        return Err(ModeError::ReadFailed);
    }
    // Bound reads even if the file changes after the metadata check.
    let mut bytes = Vec::new();
    file.take(MAX_MODE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ModeError::ReadFailed)?;
    if bytes.len() as u64 > MAX_MODE_BYTES {
        return Err(ModeError::ReadFailed);
    }
    let raw = std::str::from_utf8(&bytes).map_err(|_| ModeError::InvalidEncoding)?;
    let token = raw.strip_suffix("\n").unwrap_or(raw);
    StorageMode::from_str(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_mode_defaults_to_standalone() {
        let directory = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            read_mode_file(&directory.path().join("missing")).expect("standalone"),
            StorageMode::Standalone
        );
    }

    #[test]
    fn parses_all_explicit_modes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("mode");
        for (text, expected) in [
            ("standalone\n", StorageMode::Standalone),
            ("cluster", StorageMode::Cluster),
            ("core_only\n", StorageMode::CoreOnly),
        ] {
            std::fs::write(&path, text).expect("write");
            assert_eq!(read_mode_file(&path).expect("valid mode"), expected);
        }
    }

    #[test]
    fn invalid_mode_is_not_silently_coerced() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("mode");
        for text in ["Cluster", "cluster extra", "cluster\n\n", "", "core-only"] {
            std::fs::write(&path, text).expect("write");
            assert_eq!(read_mode_file(&path), Err(ModeError::InvalidMode));
        }
        std::fs::write(&path, [0xff, 0xfe]).expect("write");
        assert_eq!(read_mode_file(&path), Err(ModeError::InvalidEncoding));
    }

    #[test]
    fn mode_file_rejects_symlinks_and_non_regular_files() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().expect("tempdir");
        let original = directory.path().join("actual");
        std::fs::write(&original, b"cluster").expect("write");
        let link = directory.path().join("mode");
        symlink(&original, &link).expect("symlink");
        assert_eq!(read_mode_file(&link), Err(ModeError::ReadFailed));
        assert_eq!(read_mode_file(directory.path()), Err(ModeError::ReadFailed));
    }

    #[test]
    fn mode_file_rejects_oversized_inputs() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("mode");
        std::fs::write(&path, "cluster".repeat(100)).expect("write");
        assert_eq!(read_mode_file(&path), Err(ModeError::ReadFailed));
    }

    #[test]
    fn mode_changes_require_explicit_migration() {
        for before in [
            StorageMode::Standalone,
            StorageMode::CoreOnly,
            StorageMode::Cluster,
        ] {
            for after in [
                StorageMode::Standalone,
                StorageMode::CoreOnly,
                StorageMode::Cluster,
            ] {
                assert_eq!(before.validate_transition(after).is_ok(), before == after);
            }
        }
    }

    #[test]
    fn modes_cannot_enable_untested_replication_or_remote_ports() {
        for mode in [
            StorageMode::Standalone,
            StorageMode::CoreOnly,
            StorageMode::Cluster,
        ] {
            assert!(!mode.allows_replication());
            assert!(!mode.opens_remote_listener());
        }
    }
}
