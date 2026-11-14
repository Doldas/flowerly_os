//! Pre-activation backup (ADR-0023 R4, G4b): a coherent copy of the database
//! taken with `VACUUM INTO`, integrity-checked, and named by its SHA-256 so
//! the slot broker can find and verify it from the hash on the activation row.

use std::fs::{self, DirBuilder, File};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

use super::{Result, StorageError, Store};

/// A verified backup file and the hash it is named by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    pub path: PathBuf,
    pub sha256: String,
}

fn refused(message: impl Into<String>) -> StorageError {
    StorageError::InvalidOperation(message.into())
}

/// The backup directory: created 0700 if missing; never a symlink and never
/// reachable by group or others.
fn private_dir(dir: &Path) -> Result<()> {
    if fs::symlink_metadata(dir).is_err() {
        DirBuilder::new().mode(0o700).create(dir)?;
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
        return Err(refused(format!(
            "the backup directory {} must be a private directory",
            dir.display()
        )));
    }
    Ok(())
}

fn integrity_ok(path: &Path) -> Result<()> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut stmt = conn.prepare("PRAGMA integrity_check")?;
    let rows: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if rows != ["ok"] {
        return Err(refused("the backup failed its integrity check"));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    file.sync_all()?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

impl Store {
    /// Write a coherent, integrity-checked copy of the database to
    /// `<dir>/<sha256>.sqlite` (mode 0600) and return it. A temporary file
    /// is renamed into place only after the check, and removed on failure.
    pub fn backup_into(&self, dir: &Path) -> Result<Backup> {
        private_dir(dir)?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let temp = dir.join(format!(".backup-{}-{nanos}.tmp", std::process::id()));
        let outcome = self.write_backup(dir, &temp);
        if outcome.is_err() {
            let _ = fs::remove_file(&temp);
        }
        outcome
    }

    fn write_backup(&self, dir: &Path, temp: &Path) -> Result<Backup> {
        let target = temp
            .to_str()
            .ok_or_else(|| refused("the backup path is not UTF-8"))?;
        self.conn.execute("VACUUM INTO ?1", [target])?;
        fs::set_permissions(temp, fs::Permissions::from_mode(0o600))?;
        integrity_ok(temp)?;
        let sha256 = sha256_file(temp)?;
        let path = dir.join(format!("{sha256}.sqlite"));
        fs::rename(temp, &path)?;
        File::open(dir)?.sync_all()?;
        Ok(Backup { path, sha256 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Store;
    use crate::types::{Generation, GenerationId, GenerationStatus, Owner, Timestamp};
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;

    fn store_with_a_generation(dir: &Path) -> (Store, GenerationId) {
        let mut store = Store::open(dir.join("state.db")).unwrap();
        store.migrate().unwrap();
        let generation = Generation {
            id: GenerationId::new(),
            parent: None,
            source_commit: None,
            system_manifest: None,
            reason_goal: None,
            experiment: None,
            status: GenerationStatus::Candidate,
            created_at: Timestamp::now(),
            owner: Owner::System,
        };
        store.insert_generation(&generation).unwrap();
        (store, generation.id)
    }

    #[test]
    fn a_backup_is_a_coherent_private_copy_named_by_its_hash() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id) = store_with_a_generation(dir.path());
        let backups = dir.path().join("activation-backups");
        let backup = store.backup_into(&backups).unwrap();
        let bytes = std::fs::read(&backup.path).unwrap();
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(backup.sha256, digest);
        assert_eq!(backup.path, backups.join(format!("{digest}.sqlite")));
        assert!(bytes.starts_with(b"SQLite format 3\0"));
        let mode = std::fs::metadata(&backup.path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let dir_mode = std::fs::metadata(&backups).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o077, 0, "the backup directory is private");
        // The copy holds the data, and reading it changes nothing.
        let copy =
            Connection::open_with_flags(&backup.path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let found: i64 = copy
            .query_row(
                "SELECT count(*) FROM generations WHERE id = ?1",
                [id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, 1);
        drop(copy);
        assert_eq!(std::fs::read(&backup.path).unwrap(), bytes);
        // No temporary file is left behind, and a second backup works.
        store.backup_into(&backups).unwrap();
        let names: Vec<String> = std::fs::read_dir(&backups)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            names
                .iter()
                .all(|n| n.ends_with(".sqlite") && n.len() == 71),
            "{names:?}"
        );
    }

    #[test]
    fn a_backup_refuses_a_directory_others_can_reach() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = store_with_a_generation(dir.path());
        let backups = dir.path().join("shared");
        std::fs::create_dir(&backups).unwrap();
        std::fs::set_permissions(&backups, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = store.backup_into(&backups).unwrap_err().to_string();
        assert!(error.contains("private"), "{error}");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        assert!(store.backup_into(&link).is_err());
        assert_eq!(std::fs::read_dir(&backups).unwrap().count(), 0);
    }

    #[test]
    fn a_damaged_copy_fails_the_integrity_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copy.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE t (x TEXT); CREATE INDEX t_x ON t(x);
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000)
                 INSERT INTO t SELECT printf('row-%05d', i) FROM n;",
            )
            .unwrap();
        }
        integrity_ok(&path).unwrap();
        // Change one stored value where it first occurs (the table or the
        // index, not both): every query still runs, but integrity_check
        // reports the disagreement as rows instead of failing.
        let mut bytes = std::fs::read(&path).unwrap();
        let at = bytes
            .windows(9)
            .position(|w| w == b"row-01000")
            .expect("value on disk");
        bytes[at + 2] = b'x';
        std::fs::write(&path, &bytes).unwrap();
        assert!(integrity_ok(&path).is_err());
    }
}
