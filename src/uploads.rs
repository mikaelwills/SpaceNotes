//! Resumable uploads, tus-style.
//!
//! `POST /uploads` opens a session, `HEAD /uploads/{id}` reports how many
//! bytes survived, `PATCH /uploads/{id}` appends from that offset. A dropped
//! connection costs the current chunk, not the transfer.
//!
//! State lives beside the bytes in `<vault>/.uploads/`: `<id>.part` holds the
//! data, `<id>.json` the target path and size. The directory is a dotfile, so
//! both the watcher and the cold-start scan skip it; `.part` is not an
//! ingestible extension either, so a partial upload cannot reach the database
//! through any path.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub const UPLOADS_DIR: &str = ".uploads";

/// Sidecar describing an in-flight upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadSession {
    pub id: String,
    /// Vault-relative destination, already collision-checked at creation.
    pub path: String,
    pub size: u64,
    pub created_ms: u64,
}

pub fn uploads_dir(vault_root: &Path) -> PathBuf {
    vault_root.join(UPLOADS_DIR)
}

pub fn part_path(vault_root: &Path, id: &str) -> PathBuf {
    uploads_dir(vault_root).join(format!("{id}.part"))
}

pub fn meta_path(vault_root: &Path, id: &str) -> PathBuf {
    uploads_dir(vault_root).join(format!("{id}.json"))
}

/// Ids become filenames, so this is the guard that stops a crafted id from
/// naming a path. Accepts only the hex-and-dash shape `new_id` produces,
/// which excludes separators, dots and anything non-ASCII.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn read_session(vault_root: &Path, id: &str) -> Result<UploadSession> {
    let raw = std::fs::read_to_string(meta_path(vault_root, id))?;
    Ok(serde_json::from_str(&raw)?)
}

pub fn write_session(vault_root: &Path, session: &UploadSession) -> Result<()> {
    std::fs::create_dir_all(uploads_dir(vault_root))?;
    let encoded = serde_json::to_string(session)?;
    std::fs::write(meta_path(vault_root, &session.id), encoded)?;
    Ok(())
}

/// Bytes already durable for this upload. A missing part file is offset zero,
/// which lets a client that never got its first chunk through just start.
pub fn current_offset(vault_root: &Path, id: &str) -> u64 {
    std::fs::metadata(part_path(vault_root, id))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Deletes both the partial bytes and the sidecar.
pub fn discard(vault_root: &Path, id: &str) {
    let _ = std::fs::remove_file(part_path(vault_root, id));
    let _ = std::fs::remove_file(meta_path(vault_root, id));
}

const MAX_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// Drops sessions older than a week, so an abandoned upload cannot hold vault
/// space indefinitely. Returns how many were removed.
pub fn sweep_stale(vault_root: &Path) -> usize {
    let dir = uploads_dir(vault_root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };

    let now = now_ms();
    let mut removed = 0;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };

        let stale = match read_session(vault_root, id) {
            Ok(session) => now.saturating_sub(session.created_ms) > MAX_AGE_MS,
            Err(_) => true,
        };

        if stale {
            discard(vault_root, id);
            removed += 1;
        }
    }

    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_vault(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "spacenotes-uploads-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn ids_are_unique_and_filename_safe() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert!(is_valid_id(&a));
        assert!(!a.contains('/'));
        assert!(!a.contains('.'));
    }

    #[test]
    fn an_id_that_could_escape_the_uploads_dir_is_refused() {
        assert!(!is_valid_id("../../etc/passwd"));
        assert!(!is_valid_id("a/b"));
        assert!(!is_valid_id(".."));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id("zz-not-hex"));
    }

    #[test]
    fn offset_of_an_unknown_upload_is_zero() {
        let vault = temp_vault("unknown-offset");
        assert_eq!(current_offset(&vault, "deadbeef"), 0);
        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_session_round_trips_through_its_sidecar() {
        let vault = temp_vault("roundtrip");
        let session = UploadSession {
            id: new_id(),
            path: "Music/take 1.wav".to_string(),
            size: 41_943_040,
            created_ms: now_ms(),
        };

        write_session(&vault, &session).unwrap();
        let read = read_session(&vault, &session.id).unwrap();

        assert_eq!(read.path, session.path);
        assert_eq!(read.size, session.size);
        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn sweep_removes_a_week_old_session_and_keeps_a_fresh_one() {
        let vault = temp_vault("sweep");

        let fresh = UploadSession {
            id: new_id(),
            path: "fresh.wav".to_string(),
            size: 10,
            created_ms: now_ms(),
        };
        let stale = UploadSession {
            id: new_id(),
            path: "stale.wav".to_string(),
            size: 10,
            created_ms: now_ms() - (MAX_AGE_MS + 1),
        };

        write_session(&vault, &fresh).unwrap();
        write_session(&vault, &stale).unwrap();
        std::fs::write(part_path(&vault, &stale.id), b"partial").unwrap();

        assert_eq!(sweep_stale(&vault), 1);
        assert!(read_session(&vault, &fresh.id).is_ok());
        assert!(read_session(&vault, &stale.id).is_err());
        assert!(!part_path(&vault, &stale.id).exists());

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn sweep_drops_a_session_whose_sidecar_is_unreadable() {
        let vault = temp_vault("sweep-corrupt");
        std::fs::create_dir_all(uploads_dir(&vault)).unwrap();
        let id = new_id();
        std::fs::write(meta_path(&vault, &id), b"{ not json").unwrap();
        std::fs::write(part_path(&vault, &id), b"orphaned").unwrap();

        assert_eq!(sweep_stale(&vault), 1);
        assert!(!part_path(&vault, &id).exists());

        let _ = std::fs::remove_dir_all(&vault);
    }

    /// The watcher and cold-start scan both skip dot-directories unless they
    /// are explicitly allowlisted, so this name is what keeps a partial
    /// upload out of the database.
    #[test]
    fn the_uploads_dir_name_is_hidden() {
        assert!(UPLOADS_DIR.starts_with('.'));
    }
}
