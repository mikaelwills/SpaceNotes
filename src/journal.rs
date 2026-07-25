use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Params};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

use crate::frontmatter::extract_spacetime_id;
use crate::sanitize::sanitize_path;

const SCHEMA_VERSION: i64 = 1;
const TOMBSTONE_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const EVENT_RETENTION_ROWS: i64 = 10_000;

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS files (
  uuid          TEXT PRIMARY KEY,
  path          TEXT NOT NULL,
  kind          TEXT NOT NULL DEFAULT 'md' CHECK (kind IN ('md','file')),
  content_hash  TEXT NOT NULL,
  size          INTEGER NOT NULL,
  device        INTEGER,
  inode         INTEGER,
  created_time  INTEGER NOT NULL,
  modified_time INTEGER NOT NULL,
  last_seen_at  INTEGER NOT NULL,
  deleted_at    INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_files_path ON files(path) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_files_hash ON files(content_hash, size) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_files_inode ON files(device, inode) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_files_tombstone ON files(deleted_at) WHERE deleted_at IS NOT NULL;
CREATE TABLE IF NOT EXISTS events (
  seq       INTEGER PRIMARY KEY AUTOINCREMENT,
  ts        INTEGER NOT NULL,
  op        TEXT NOT NULL CHECK (op IN ('create','modify','rename','delete','relink','seed','migrate')),
  uuid      TEXT,
  from_path TEXT,
  to_path   TEXT
);
";

const SELECT_FILES: &str = "SELECT uuid, path, kind, content_hash, size, device, inode, \
     created_time, modified_time, last_seen_at, deleted_at FROM files";

const UPSERT_FILE: &str = "INSERT INTO files (uuid, path, kind, content_hash, size, device, inode, \
     created_time, modified_time, last_seen_at, deleted_at) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL) \
     ON CONFLICT(uuid) DO UPDATE SET \
       path = excluded.path, \
       kind = excluded.kind, \
       content_hash = excluded.content_hash, \
       size = excluded.size, \
       device = excluded.device, \
       inode = excluded.inode, \
       created_time = excluded.created_time, \
       modified_time = excluded.modified_time, \
       last_seen_at = excluded.last_seen_at, \
       deleted_at = NULL";

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FileRecord {
    pub uuid: String,
    pub path: String,
    pub kind: String,
    pub content_hash: String,
    pub size: i64,
    pub device: Option<i64>,
    pub inode: Option<i64>,
    pub created_time: i64,
    pub modified_time: i64,
    pub last_seen_at: i64,
    pub deleted_at: Option<i64>,
}

pub struct Journal {
    conn: Mutex<Connection>,
}

#[allow(dead_code)]
impl Journal {
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open journal {:?}", db_path))?;
        conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn set_meta_if_absent(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let value = self
            .conn()
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value)
    }

    pub fn by_path(&self, path: &str) -> Result<Option<FileRecord>> {
        let record = self
            .conn()
            .query_row(
                &format!("{} WHERE path = ?1 AND deleted_at IS NULL", SELECT_FILES),
                params![path],
                row_to_record,
            )
            .optional()?;
        Ok(record)
    }

    pub fn by_uuid(&self, uuid: &str) -> Result<Option<FileRecord>> {
        let record = self
            .conn()
            .query_row(
                &format!("{} WHERE uuid = ?1 AND deleted_at IS NULL", SELECT_FILES),
                params![uuid],
                row_to_record,
            )
            .optional()?;
        Ok(record)
    }

    pub fn by_inode(&self, device: i64, inode: i64) -> Result<Vec<FileRecord>> {
        query_records(
            &self.conn(),
            &format!(
                "{} WHERE device = ?1 AND inode = ?2 AND deleted_at IS NULL",
                SELECT_FILES
            ),
            params![device, inode],
        )
    }

    pub fn by_hash(&self, content_hash: &str, size: i64) -> Result<Vec<FileRecord>> {
        query_records(
            &self.conn(),
            &format!(
                "{} WHERE content_hash = ?1 AND size = ?2 AND deleted_at IS NULL",
                SELECT_FILES
            ),
            params![content_hash, size],
        )
    }

    pub fn live_files(&self) -> Result<Vec<FileRecord>> {
        query_records(
            &self.conn(),
            &format!("{} WHERE deleted_at IS NULL", SELECT_FILES),
            [],
        )
    }

    pub fn upsert(&self, record: &FileRecord) -> Result<()> {
        execute_upsert(&self.conn(), record)?;
        Ok(())
    }

    pub fn seed(&self, record: &FileRecord) -> Result<bool> {
        self.observe(record, "seed")
    }

    pub fn observe(&self, record: &FileRecord, op: &str) -> Result<bool> {
        let conn = self.conn();
        let existing: Option<String> = conn
            .query_row(
                "SELECT uuid FROM files WHERE uuid = ?1",
                params![record.uuid],
                |row| row.get(0),
            )
            .optional()?;
        let displaced: Option<String> = conn
            .query_row(
                "SELECT uuid FROM files WHERE path = ?1 AND uuid <> ?2 AND deleted_at IS NULL",
                params![record.path, record.uuid],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(displaced_uuid) = displaced {
            conn.execute(
                "UPDATE files SET deleted_at = ?1 WHERE uuid = ?2",
                params![record.last_seen_at, displaced_uuid],
            )?;
            insert_event(
                &conn,
                record.last_seen_at,
                "delete",
                Some(&displaced_uuid),
                Some(&record.path),
                None,
            )?;
        }
        execute_upsert(&conn, record)?;
        let inserted = existing.is_none();
        if inserted {
            insert_event(
                &conn,
                record.last_seen_at,
                op,
                Some(&record.uuid),
                None,
                Some(&record.path),
            )?;
        }
        Ok(inserted)
    }

    pub fn rekey(&self, uuid: &str, to_path: &str, ts: i64) -> Result<()> {
        let conn = self.conn();
        let from_path: Option<String> = conn
            .query_row(
                "SELECT path FROM files WHERE uuid = ?1",
                params![uuid],
                |row| row.get(0),
            )
            .optional()?;
        let Some(from_path) = from_path else {
            anyhow::bail!("rekey: unknown uuid {}", uuid);
        };
        conn.execute(
            "UPDATE files SET path = ?1, last_seen_at = ?2, deleted_at = NULL WHERE uuid = ?3",
            params![to_path, ts, uuid],
        )?;
        insert_event(&conn, ts, "rename", Some(uuid), Some(&from_path), Some(to_path))?;
        Ok(())
    }

    pub fn rekey_prefix(
        &self,
        from_prefix: &str,
        to_prefix: &str,
        ts: i64,
    ) -> Result<Vec<(String, String)>> {
        let conn = self.conn();
        let prefix = format!("{}/", from_prefix);
        let rows = query_records(
            &conn,
            &format!(
                "{} WHERE deleted_at IS NULL AND substr(path, 1, ?1) = ?2",
                SELECT_FILES
            ),
            params![prefix.len() as i64, prefix],
        )?;
        let mut moved = Vec::new();
        for record in rows {
            let new_path = format!("{}/{}", to_prefix, &record.path[prefix.len()..]);
            conn.execute(
                "UPDATE files SET path = ?1, last_seen_at = ?2 WHERE uuid = ?3",
                params![new_path, ts, record.uuid],
            )?;
            insert_event(
                &conn,
                ts,
                "rename",
                Some(&record.uuid),
                Some(&record.path),
                Some(&new_path),
            )?;
            moved.push((record.uuid, new_path));
        }
        Ok(moved)
    }

    pub fn tombstone(&self, uuid: &str, ts: i64) -> Result<()> {
        let conn = self.conn();
        let path: Option<String> = conn
            .query_row(
                "SELECT path FROM files WHERE uuid = ?1 AND deleted_at IS NULL",
                params![uuid],
                |row| row.get(0),
            )
            .optional()?;
        let Some(path) = path else {
            return Ok(());
        };
        conn.execute(
            "UPDATE files SET deleted_at = ?1 WHERE uuid = ?2",
            params![ts, uuid],
        )?;
        insert_event(&conn, ts, "delete", Some(uuid), Some(&path), None)?;
        Ok(())
    }

    pub fn tombstone_unseen(&self, seen_before: i64, ts: i64) -> Result<usize> {
        let conn = self.conn();
        let stale = query_records(
            &conn,
            &format!(
                "{} WHERE deleted_at IS NULL AND last_seen_at < ?1",
                SELECT_FILES
            ),
            params![seen_before],
        )?;
        for record in &stale {
            conn.execute(
                "UPDATE files SET deleted_at = ?1 WHERE uuid = ?2",
                params![ts, record.uuid],
            )?;
            insert_event(&conn, ts, "delete", Some(&record.uuid), Some(&record.path), None)?;
        }
        Ok(stale.len())
    }

    pub fn log_event(
        &self,
        ts: i64,
        op: &str,
        uuid: Option<&str>,
        from_path: Option<&str>,
        to_path: Option<&str>,
    ) -> Result<()> {
        insert_event(&self.conn(), ts, op, uuid, from_path, to_path)
    }

    pub fn backup_to(&self, target: &Path) -> Result<()> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create backup dir {:?}", parent))?;
        }
        let mut tmp_name = target
            .file_name()
            .context("Backup target has no file name")?
            .to_os_string();
        tmp_name.push(".tmp");
        let tmp = target.with_file_name(tmp_name);
        let _ = std::fs::remove_file(&tmp);
        self.conn()
            .execute("VACUUM INTO ?1", params![tmp.to_string_lossy()])?;
        std::fs::rename(&tmp, target)
            .with_context(|| format!("Failed to finalize backup {:?}", target))?;
        Ok(())
    }

    pub fn prune(&self, now: i64) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM files WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            params![now - TOMBSTONE_RETENTION_MS],
        )?;
        conn.execute(
            "DELETE FROM events WHERE seq NOT IN \
             (SELECT seq FROM events ORDER BY seq DESC LIMIT ?1)",
            params![EVENT_RETENTION_ROWS],
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub fn event_count(&self, op: &str) -> i64 {
        self.conn()
            .query_row(
                "SELECT COUNT(*) FROM events WHERE op = ?1",
                params![op],
                |row| row.get(0),
            )
            .unwrap()
    }
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )?;
    let current: i64 = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    for version in (current + 1)..=SCHEMA_VERSION {
        apply_migration(conn, version)?;
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![version.to_string()],
        )?;
    }
    Ok(())
}

fn apply_migration(conn: &Connection, version: i64) -> Result<()> {
    match version {
        1 => {
            conn.execute_batch(SCHEMA_V1)?;
            Ok(())
        }
        _ => anyhow::bail!("Unknown journal schema version {}", version),
    }
}

fn execute_upsert(conn: &Connection, record: &FileRecord) -> Result<()> {
    conn.execute(
        UPSERT_FILE,
        params![
            record.uuid,
            record.path,
            record.kind,
            record.content_hash,
            record.size,
            record.device,
            record.inode,
            record.created_time,
            record.modified_time,
            record.last_seen_at,
        ],
    )?;
    Ok(())
}

fn insert_event(
    conn: &Connection,
    ts: i64,
    op: &str,
    uuid: Option<&str>,
    from_path: Option<&str>,
    to_path: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO events (ts, op, uuid, from_path, to_path) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![ts, op, uuid, from_path, to_path],
    )?;
    Ok(())
}

fn query_records(conn: &Connection, sql: &str, params: impl Params) -> Result<Vec<FileRecord>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params, row_to_record)?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<FileRecord> {
    Ok(FileRecord {
        uuid: row.get(0)?,
        path: row.get(1)?,
        kind: row.get(2)?,
        content_hash: row.get(3)?,
        size: row.get(4)?,
        device: row.get(5)?,
        inode: row.get(6)?,
        created_time: row.get(7)?,
        modified_time: row.get(8)?,
        last_seen_at: row.get(9)?,
        deleted_at: row.get(10)?,
    })
}

pub fn stored_vault_path(db_path: &Path) -> Option<String> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row(
        "SELECT value FROM meta WHERE key = 'vault_path_last_seen'",
        [],
        |row| row.get(0),
    )
    .ok()
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub struct SeedStats {
    pub seeded: usize,
    pub refreshed: usize,
    pub skipped: usize,
    pub tombstoned: usize,
}

pub fn seed_from_vault(journal: &Journal, vault_path: &Path) -> Result<SeedStats> {
    let scan_started = now_ms();
    let mut stats = SeedStats {
        seeded: 0,
        refreshed: 0,
        skipped: 0,
        tombstoned: 0,
    };

    let walker = WalkDir::new(vault_path).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        !name.starts_with('.') && name != "@eaDir"
    });

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() || path.extension().map_or(true, |e| e != "md") {
            continue;
        }
        match seed_one(journal, vault_path, path) {
            Ok(Some(true)) => stats.seeded += 1,
            Ok(Some(false)) => stats.refreshed += 1,
            Ok(None) => stats.skipped += 1,
            Err(e) => {
                tracing::warn!("Journal seed failed for {:?}: {}", path, e);
                stats.skipped += 1;
            }
        }
    }

    stats.tombstoned = journal.tombstone_unseen(scan_started, now_ms())?;
    journal.set_meta("last_full_scan_at", &now_ms().to_string())?;
    Ok(stats)
}

fn seed_one(journal: &Journal, vault_path: &Path, abs_path: &Path) -> Result<Option<bool>> {
    let bytes = std::fs::read(abs_path)?;
    let text = String::from_utf8_lossy(&bytes);
    let Some(uuid) = extract_spacetime_id(&text) else {
        tracing::debug!("Journal seed skipping note without UUID: {:?}", abs_path);
        return Ok(None);
    };

    let record = build_record(vault_path, abs_path, &bytes, uuid)?;
    Ok(Some(journal.seed(&record)?))
}

pub fn record_from_disk(vault_path: &Path, abs_path: &Path, uuid: String) -> Result<FileRecord> {
    let bytes = std::fs::read(abs_path)?;
    build_record(vault_path, abs_path, &bytes, uuid)
}

fn build_record(
    vault_path: &Path,
    abs_path: &Path,
    bytes: &[u8],
    uuid: String,
) -> Result<FileRecord> {
    let rel_path = sanitize_path(&abs_path.strip_prefix(vault_path)?.to_string_lossy());
    let metadata = std::fs::metadata(abs_path)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let created = metadata
        .created()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(modified);
    let (device, inode) = device_inode(&metadata);

    Ok(FileRecord {
        uuid,
        path: rel_path,
        kind: "md".to_string(),
        content_hash: hash_bytes(bytes),
        size: metadata.len() as i64,
        device,
        inode,
        created_time: created,
        modified_time: modified,
        last_seen_at: now_ms(),
        deleted_at: None,
    })
}

#[cfg(unix)]
fn device_inode(metadata: &std::fs::Metadata) -> (Option<i64>, Option<i64>) {
    use std::os::unix::fs::MetadataExt;
    (Some(metadata.dev() as i64), Some(metadata.ino() as i64))
}

#[cfg(not(unix))]
fn device_inode(_metadata: &std::fs::Metadata) -> (Option<i64>, Option<i64>) {
    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "spacenotes-journal-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_note(vault: &Path, file: &str, id: &str) -> Vec<u8> {
        let content = format!("---\nspacetime_id: {}\n---\nbody of {}\n", id, file);
        std::fs::write(vault.join(file), &content).unwrap();
        content.into_bytes()
    }

    const ID_A: &str = "11111111-1111-1111-1111-111111111111";
    const ID_B: &str = "22222222-2222-2222-2222-222222222222";

    #[test]
    fn seed_matches_frontmatter_truth() {
        let dir = temp_dir("seed");
        let vault = dir.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        let bytes_a = write_note(&vault, "a.md", ID_A);
        write_note(&vault, "b.md", ID_B);
        std::fs::write(vault.join("noid.md"), "no frontmatter here\n").unwrap();

        let journal = Journal::open(&dir.join("journal.db")).unwrap();
        let stats = seed_from_vault(&journal, &vault).unwrap();

        assert_eq!(stats.seeded, 2);
        assert_eq!(stats.skipped, 1);
        assert_eq!(stats.tombstoned, 0);

        let record = journal.by_path("a.md").unwrap().unwrap();
        assert_eq!(record.uuid, ID_A);
        assert_eq!(record.kind, "md");
        assert_eq!(record.content_hash, hash_bytes(&bytes_a));
        assert_eq!(record.size, bytes_a.len() as i64);
        assert!(record.inode.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reseed_is_idempotent() {
        let dir = temp_dir("idempotent");
        let vault = dir.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        write_note(&vault, "a.md", ID_A);

        let journal = Journal::open(&dir.join("journal.db")).unwrap();
        let first = seed_from_vault(&journal, &vault).unwrap();
        let second = seed_from_vault(&journal, &vault).unwrap();

        assert_eq!(first.seeded, 1);
        assert_eq!(second.seeded, 0);
        assert_eq!(second.refreshed, 1);
        assert_eq!(journal.live_files().unwrap().len(), 1);
        assert_eq!(journal.event_count("seed"), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleted_file_tombstoned_on_reseed() {
        let dir = temp_dir("tombstone");
        let vault = dir.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        write_note(&vault, "a.md", ID_A);
        write_note(&vault, "b.md", ID_B);

        let journal = Journal::open(&dir.join("journal.db")).unwrap();
        seed_from_vault(&journal, &vault).unwrap();
        std::fs::remove_file(vault.join("b.md")).unwrap();
        let stats = seed_from_vault(&journal, &vault).unwrap();

        assert_eq!(stats.tombstoned, 1);
        assert!(journal.by_path("b.md").unwrap().is_none());
        assert_eq!(journal.live_files().unwrap().len(), 1);
        assert_eq!(journal.event_count("delete"), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backup_restores_same_rows() {
        let dir = temp_dir("backup");
        let vault = dir.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        write_note(&vault, "a.md", ID_A);
        write_note(&vault, "b.md", ID_B);

        let journal = Journal::open(&dir.join("journal.db")).unwrap();
        seed_from_vault(&journal, &vault).unwrap();
        let target = dir.join("backup").join("journal-backup.db");
        journal.backup_to(&target).unwrap();

        let restored = Journal::open(&target).unwrap();
        assert_eq!(restored.live_files().unwrap().len(), 2);
        assert_eq!(restored.by_path("a.md").unwrap().unwrap().uuid, ID_A);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migration_records_schema_version() {
        let dir = temp_dir("migration");
        let journal = Journal::open(&dir.join("journal.db")).unwrap();
        assert_eq!(
            journal.get_meta("schema_version").unwrap().as_deref(),
            Some("1")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rekey_preserves_uuid_and_logs_rename() {
        let dir = temp_dir("rekey");
        let vault = dir.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        write_note(&vault, "a.md", ID_A);

        let journal = Journal::open(&dir.join("journal.db")).unwrap();
        seed_from_vault(&journal, &vault).unwrap();
        journal.rekey(ID_A, "moved.md", now_ms()).unwrap();

        assert!(journal.by_path("a.md").unwrap().is_none());
        assert_eq!(journal.by_path("moved.md").unwrap().unwrap().uuid, ID_A);
        assert_eq!(journal.event_count("rename"), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
