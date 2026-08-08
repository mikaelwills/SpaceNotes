mod client;
mod folder;
mod isolation;
mod journal;
mod migrate;
mod space_file;
mod reconcile;
mod sanitize;
mod scanner;
mod spacetime_bindings;
mod tracker;
mod watcher;
mod writer;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::tracker::ContentTracker;
use crate::writer::write_file_to_disk;

#[derive(Parser, Debug)]
#[command(name = "spacenotes")]
#[command(about = "Sync markdown files to SpacetimeDB")]
struct Args {
    #[arg(short, long, env = "VAULT_PATH")]
    vault_path: PathBuf,

    #[arg(short = 's', long, env = "SPACETIME_HOST",
          default_value = "http://localhost:3003")]
    spacetime_host: String,

    #[arg(short, long, env = "SPACETIME_DB",
          default_value = "spacenotes")]
    database: String,

    #[arg(long, env = "DATA_DIR")]
    data_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    // Validate and canonicalize path
    if !args.vault_path.exists() {
        anyhow::bail!("Vault path does not exist: {:?}", args.vault_path);
    }
    let absolute_vault_path = std::fs::canonicalize(&args.vault_path)
        .context("Failed to resolve absolute path for vault")?;

    tracing::info!("Vault path: {:?}", absolute_vault_path);
    tracing::info!("SpacetimeDB: {}/{}", args.spacetime_host, args.database);

    let data_dir = args.data_dir.clone().unwrap_or_else(default_data_dir);
    tracing::info!("Data dir: {:?}", data_dir);

    let opened_journal = open_journal(&absolute_vault_path, &data_dir)?;

    // Initialize content tracker for loop prevention
    let tracker = Arc::new(ContentTracker::new());

    // Connect to SpacetimeDB
    let client = Arc::new(
        client::SpacetimeClient::connect(&args.spacetime_host, &args.database)?
    );

    // Wait for initial subscription data
    tracing::info!("Waiting for subscription sync...");
    client.wait_for_sync()?;

    tracing::info!("Running frontmatter strip migration...");
    let migration = migrate::run(&opened_journal.journal, &client, &tracker, &absolute_vault_path)?;
    tracing::info!(
        "Migration: {} stripped, {} adopted, {} clean, {} failed",
        migration.stripped,
        migration.adopted,
        migration.clean,
        migration.failed
    );

    // Reconcile local vault with server (two-way sync)
    tracing::info!("Reconciling with server...");
    reconcile::reconcile_on_startup(
        &absolute_vault_path,
        &client,
        &tracker,
        &opened_journal.journal,
    )?;

    // Reconcile folders (two-way sync)
    tracing::info!("Reconciling folders...");
    let local_folders = scanner::scan_folders(&absolute_vault_path)?;
    let server_folders = client.get_all_folders();

    // Create folders that exist on server but not locally
    for server_folder in &server_folders {
        // Skip @eaDir folders (Synology metadata)
        if server_folder.path.contains("@eaDir") {
            continue;
        }

        let folder_path = match writer::resolve_vault_path(&absolute_vault_path, &server_folder.path) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Refusing server folder {}: {}", server_folder.path, e);
                continue;
            }
        };
        if !folder_path.exists() {
            if let Err(e) = std::fs::create_dir_all(&folder_path) {
                tracing::error!("Failed to create folder {}: {}", server_folder.path, e);
            } else {
                tracing::info!("Created local folder from server: {}", server_folder.path);
            }
        }
    }

    // Upload folders that exist locally but not on server
    client.sync_folders(&local_folders);

    run_journal_maintenance(&opened_journal, &absolute_vault_path, &data_dir);

    // Register callback for file updates from server
    let vault_clone = absolute_vault_path.clone();
    let tracker_clone = tracker.clone();
    let journal_clone = opened_journal.journal.clone();
    client.on_file_updated(move |old_file, new_file| {
        let path_changed = old_file.path != new_file.path;
        let signal = new_file.content.clone();
        let content_changed = tracker_clone.is_modified(&new_file.id, &signal);

        // Skip if nothing changed (echo from our own update)
        if !path_changed && !content_changed {
            tracing::debug!("Skipping update echo: {}", new_file.path);
            return;
        }

        if path_changed && !apply_server_rename(&vault_clone, &old_file.path, &new_file.path) {
            return;
        }

        // Convert DbSpaceFile to LocalSpaceFile for writer
        let file = space_file::SpaceFile {
            id: new_file.id.clone(),
            path: new_file.path.clone(),
            name: new_file.name.clone(),
            content: new_file.content.clone(),
            folder_path: new_file.folder_path.clone(),
            depth: new_file.depth,
            extension: new_file.extension.clone(),
            size: new_file.size,
            created_time: new_file.created_time,
            modified_time: new_file.modified_time,
        };

        tracker_clone.update(&file.id, &signal);
        download_server_file(&journal_clone, &vault_clone, &file, "Downloaded update");
    });

    // Register callback for file inserts from server
    let vault_clone = absolute_vault_path.clone();
    let tracker_clone = tracker.clone();
    let journal_clone = opened_journal.journal.clone();
    client.on_file_inserted(move |db_file| {
        let signal = db_file.content.clone();
        // Skip if we already have this content (echo from our own upload)
        if !tracker_clone.is_modified(&db_file.id, &signal) {
            tracing::debug!("Skipping insert echo: {}", db_file.path);
            return;
        }

        let file = space_file::SpaceFile {
            id: db_file.id.clone(),
            path: db_file.path.clone(),
            name: db_file.name.clone(),
            content: db_file.content.clone(),
            folder_path: db_file.folder_path.clone(),
            depth: db_file.depth,
            extension: db_file.extension.clone(),
            size: db_file.size,
            created_time: db_file.created_time,
            modified_time: db_file.modified_time,
        };

        tracker_clone.update(&file.id, &signal);
        download_server_file(&journal_clone, &vault_clone, &file, "Downloaded new");
    });

    // Register callback for file deletions from server
    let vault_clone = absolute_vault_path.clone();
    let tracker_clone = tracker.clone();
    let journal_clone = opened_journal.journal.clone();
    client.on_file_deleted(move |old_file| {
        let path = match writer::resolve_vault_path(&vault_clone, &old_file.path) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Refusing server delete of {}: {}", old_file.path, e);
                return;
            }
        };
        if path.exists() {
            if scanner::is_binary(&path) {
                tracing::warn!(
                    "Refusing to delete binary file {} from disk: its bytes are not stored in the database and exist nowhere else",
                    old_file.path
                );
                return;
            }
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::error!("Failed to delete {}: {}", old_file.path, e);
                return;
            }
            tracker_clone.remove(&old_file.id);
            tracing::info!("Deleted local file: {}", old_file.path);
        }
        if let Err(e) = journal_clone.tombstone(&old_file.id, journal::now_ms()) {
            tracing::error!("Journal tombstone failed for {}: {}", old_file.path, e);
        }
    });

    // Register callback for folder inserts from server
    let vault_clone = absolute_vault_path.clone();
    client.on_folder_inserted(move |new_folder| {
        // Skip @eaDir folders (Synology metadata)
        if new_folder.path.contains("@eaDir") {
            return;
        }

        let path = match writer::resolve_vault_path(&vault_clone, &new_folder.path) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Refusing server folder {}: {}", new_folder.path, e);
                return;
            }
        };
        if !path.exists() {
            if let Err(e) = std::fs::create_dir_all(&path) {
                tracing::error!("Failed to create folder {}: {}", new_folder.path, e);
            } else {
                tracing::info!("Created local folder: {}", new_folder.path);
            }
        }
    });

    // Register callback for folder deletions from server
    let vault_clone = absolute_vault_path.clone();
    client.on_folder_deleted(move |old_folder| {
        let path = match writer::resolve_vault_path(&vault_clone, &old_folder.path) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Refusing server folder delete of {}: {}", old_folder.path, e);
                return;
            }
        };
        if path.exists() && path.is_dir() {
            if let Some(found) = scanner::find_binary_under(&path) {
                tracing::warn!(
                    "Refusing to delete folder {} from disk: it contains binary file {} whose bytes are not stored in the database",
                    old_folder.path,
                    found.display()
                );
                return;
            }
            if let Err(e) = std::fs::remove_dir_all(&path) {
                tracing::error!("Failed to delete folder {}: {}", old_folder.path, e);
            } else {
                tracing::info!("Deleted local folder: {}", old_folder.path);
            }
        }
    });

    // Register callback for folder updates from server (renames/moves)
    let vault_clone = absolute_vault_path.clone();
    client.on_folder_updated(move |old_folder, new_folder| {
        let old_path = match writer::resolve_vault_path(&vault_clone, &old_folder.path) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Refusing server folder rename from {}: {}", old_folder.path, e);
                return;
            }
        };
        let new_path = match writer::resolve_vault_path(&vault_clone, &new_folder.path) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Refusing server folder rename to {}: {}", new_folder.path, e);
                return;
            }
        };

        if old_path.exists() && old_path != new_path {
            // Create parent directory for new location if needed
            if let Some(parent) = new_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            // Rename the folder
            if let Err(e) = std::fs::rename(&old_path, &new_path) {
                tracing::error!("Failed to rename folder {} -> {}: {}",
                    old_folder.path, new_folder.path, e);
            } else {
                tracing::info!("Renamed folder: {} -> {}", old_folder.path, new_folder.path);
            }
        }
    });

    tracing::info!("Two-way sync initialized.");

    // Start file watcher
    let watcher_journal = opened_journal.journal.clone();
    watcher::start_watcher(absolute_vault_path, client, tracker, watcher_journal).await?;

    Ok(())
}

struct OpenedJournal {
    journal: Arc<journal::Journal>,
    vault_id: String,
}

fn default_data_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".local/share/spacenotes"),
        None => PathBuf::from("/data"),
    }
}

fn open_journal(vault_path: &Path, data_dir: &Path) -> Result<OpenedJournal> {
    let journals_dir = data_dir.join("journals");
    std::fs::create_dir_all(&journals_dir)
        .with_context(|| format!("Failed to create journals dir {:?}", journals_dir))?;

    let vault_id = resolve_vault_id(vault_path, &journals_dir)?;
    let db_path = journals_dir.join(format!("{}.db", vault_id));

    if !db_path.exists() {
        restore_from_vault_backup(vault_path, &db_path);
    }

    let journal = match journal::Journal::open(&db_path) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!("Journal open failed ({:#}), moving aside and recreating", e);
            move_corrupt_journal_aside(&db_path);
            restore_from_vault_backup(vault_path, &db_path);
            journal::Journal::open(&db_path)?
        }
    };

    journal.set_meta("vault_id", &vault_id)?;
    journal.set_meta("vault_path_last_seen", &vault_path.to_string_lossy())?;
    journal.set_meta_if_absent("created_at", &journal::now_ms().to_string())?;

    tracing::info!("Journal open: {:?}", db_path);
    Ok(OpenedJournal {
        journal: Arc::new(journal),
        vault_id,
    })
}

fn resolve_vault_id(vault_path: &Path, journals_dir: &Path) -> Result<String> {
    let marker_dir = vault_path.join(".spacenotes");
    let marker = marker_dir.join("vault-id");

    if let Ok(contents) = std::fs::read_to_string(&marker) {
        let existing = contents.trim();
        if !existing.is_empty() {
            return Ok(existing.to_string());
        }
    }

    let vault_id = adopt_existing_journal(vault_path, journals_dir)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    std::fs::create_dir_all(&marker_dir)
        .with_context(|| format!("Failed to create marker dir {:?}", marker_dir))?;
    std::fs::write(&marker, &vault_id)
        .with_context(|| format!("Failed to write vault-id marker {:?}", marker))?;
    tracing::info!("Vault id: {}", vault_id);
    Ok(vault_id)
}

fn adopt_existing_journal(vault_path: &Path, journals_dir: &Path) -> Option<String> {
    let vault_str = vault_path.to_string_lossy().to_string();
    let entries = std::fs::read_dir(journals_dir).ok()?;

    let mut matches = Vec::new();
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() || path.extension().map_or(true, |e| e != "db") {
            continue;
        }
        if journal::stored_vault_path(&path).as_deref() != Some(vault_str.as_str()) {
            continue;
        }
        let Some(stem) = path.file_stem() else { continue };
        matches.push(stem.to_string_lossy().to_string());
    }

    if matches.len() != 1 {
        return None;
    }
    tracing::info!("Re-adopted journal {} for vault with missing marker", matches[0]);
    matches.pop()
}

fn restore_from_vault_backup(vault_path: &Path, db_path: &Path) {
    let backup = vault_path.join(".spacenotes").join("journal-backup.db");
    if !backup.exists() {
        return;
    }
    match std::fs::copy(&backup, db_path) {
        Ok(_) => tracing::info!("Restored journal from vault backup {:?}", backup),
        Err(e) => tracing::error!("Failed to restore journal from vault backup: {}", e),
    }
}

fn move_corrupt_journal_aside(db_path: &Path) {
    let mut corrupt = db_path.as_os_str().to_os_string();
    corrupt.push(format!(".corrupt-{}", journal::now_ms()));
    let _ = std::fs::rename(db_path, PathBuf::from(corrupt));
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = db_path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(sidecar));
    }
}

fn apply_server_rename(vault_path: &Path, old_rel: &str, new_rel: &str) -> bool {
    let old_path = match writer::resolve_vault_path(vault_path, old_rel) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Refusing server rename from {}: {}", old_rel, e);
            return false;
        }
    };
    let new_path = match writer::resolve_vault_path(vault_path, new_rel) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Refusing server rename to {}: {}", new_rel, e);
            return false;
        }
    };
    if !old_path.exists() {
        return true;
    }
    if scanner::is_binary(&old_path) != scanner::is_binary(&new_path) {
        tracing::warn!(
            "Refusing server rename {} -> {}: it crosses the text/binary boundary and would corrupt the file on disk",
            old_rel,
            new_rel
        );
        return false;
    }
    if scanner::is_binary(&old_path) {
        tracing::warn!(
            "Refusing server rename {} -> {}: credentials move only at the vault, never because a row's path changed",
            old_rel,
            new_rel
        );
        return false;
    }
    if let Err(e) = std::fs::remove_file(&old_path) {
        tracing::error!("Failed to delete old file {}: {}", old_rel, e);
    } else {
        tracing::info!("Deleted old file during rename: {}", old_rel);
    }
    true
}

fn download_server_file(
    journal: &journal::Journal,
    vault_path: &Path,
    file: &space_file::SpaceFile,
    verb: &str,
) {
    match write_file_to_disk(vault_path, file) {
        Ok(writer::WriteOutcome::Written) => {
            record_file_in_journal(journal, vault_path, file);
            tracing::info!("{}: {}", verb, file.path);
        }
        Ok(writer::WriteOutcome::SkippedBinary) => {
            if vault_path.join(&file.path).exists() {
                record_file_in_journal(journal, vault_path, file);
            }
            tracing::debug!(
                "Skipped server->disk write for {}: binary bytes are not stored in the database",
                file.path
            );
        }
        Err(e) => tracing::error!("Failed to write {}: {}", file.path, e),
    }
}

fn record_file_in_journal(journal: &journal::Journal, vault_path: &Path, file: &space_file::SpaceFile) {
    let abs = vault_path.join(&file.path);
    match journal::record_from_disk(vault_path, &abs, file.id.clone()) {
        Ok(record) => {
            if let Err(e) = journal.observe(&record, "create") {
                tracing::error!("Journal record failed for {}: {}", record.path, e);
            }
        }
        Err(e) => tracing::error!("Journal record failed for {}: {}", file.path, e),
    }
}

fn run_journal_maintenance(opened: &OpenedJournal, vault_path: &Path, data_dir: &Path) {
    if let Err(e) = opened
        .journal
        .set_meta("last_full_scan_at", &journal::now_ms().to_string())
    {
        tracing::error!("Journal meta update failed: {:#}", e);
    }

    if let Err(e) = opened.journal.prune(journal::now_ms()) {
        tracing::error!("Journal prune failed: {:#}", e);
    }

    let backups = [
        data_dir
            .join("journals")
            .join("backup")
            .join(format!("{}.db", opened.vault_id)),
        vault_path.join(".spacenotes").join("journal-backup.db"),
    ];
    for target in backups {
        match opened.journal.backup_to(&target) {
            Ok(()) => tracing::info!("Journal backup written: {:?}", target),
            Err(e) => tracing::error!("Journal backup to {:?} failed: {:#}", target, e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID_A: &str = "11111111-1111-1111-1111-111111111111";

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "spacenotes-main-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn vault_in(dir: &Path) -> PathBuf {
        let vault = dir.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        vault
    }

    #[test]
    fn server_rename_of_a_credential_is_refused_and_leaves_it_in_place() {
        let dir = temp_dir("binary-rename");
        let vault = vault_in(&dir);
        let ciphertext: &[u8] = &[0x85, 0x02, 0x0c, 0x03, 0xff, 0x00, 0xde, 0xad];
        std::fs::write(vault.join("secret.gpg"), ciphertext).unwrap();

        assert!(!apply_server_rename(&vault, "secret.gpg", "moved/secret.gpg"));

        assert_eq!(
            std::fs::read(vault.join("secret.gpg")).unwrap(),
            ciphertext,
            "a credential moves only at the vault, so it stays put"
        );
        assert!(!vault.join("moved/secret.gpg").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_rename_onto_an_existing_credential_is_refused() {
        let dir = temp_dir("rename-clobber");
        let vault = vault_in(&dir);
        let moving: &[u8] = &[0x85, 0x02, 0xde, 0xad];
        let victim: &[u8] = &[0x99, 0x03, 0xbe, 0xef];
        std::fs::write(vault.join("a.gpg"), moving).unwrap();
        std::fs::write(vault.join("b.gpg"), victim).unwrap();

        let applied = apply_server_rename(&vault, "a.gpg", "b.gpg");

        assert!(!applied, "the caller must be told the rename was refused");
        assert_eq!(std::fs::read(vault.join("b.gpg")).unwrap(), victim);
        assert_eq!(std::fs::read(vault.join("a.gpg")).unwrap(), moving);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_rename_across_the_text_binary_boundary_is_refused() {
        let dir = temp_dir("rename-boundary");
        let vault = vault_in(&dir);
        let ciphertext: &[u8] = &[0x85, 0x02, 0xde, 0xad];
        std::fs::write(vault.join("secret.gpg"), ciphertext).unwrap();
        std::fs::write(vault.join("note.md"), "body\n").unwrap();

        assert!(!apply_server_rename(&vault, "secret.gpg", "secret.md"));
        assert_eq!(std::fs::read(vault.join("secret.gpg")).unwrap(), ciphertext);
        assert!(!vault.join("secret.md").exists());

        assert!(!apply_server_rename(&vault, "note.md", "note.gpg"));
        assert_eq!(std::fs::read_to_string(vault.join("note.md")).unwrap(), "body\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_rename_of_text_row_deletes_the_old_path() {
        let dir = temp_dir("text-rename");
        let vault = vault_in(&dir);
        std::fs::write(vault.join("a.md"), "body\n").unwrap();

        apply_server_rename(&vault, "a.md", "moved.md");

        assert!(!vault.join("a.md").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_rename_with_traversal_in_the_old_path_is_refused() {
        let dir = temp_dir("traversal-old");
        let vault = vault_in(&dir);
        let ciphertext: &[u8] = &[0x85, 0x02];
        std::fs::write(dir.join("outside.gpg"), ciphertext).unwrap();

        apply_server_rename(&vault, "../outside.gpg", "stolen.gpg");

        assert_eq!(std::fs::read(dir.join("outside.gpg")).unwrap(), ciphertext);
        assert!(!vault.join("stolen.gpg").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_rename_with_traversal_in_the_new_path_is_refused() {
        let dir = temp_dir("traversal-new");
        let vault = vault_in(&dir);
        let ciphertext: &[u8] = &[0x85, 0x02];
        std::fs::write(vault.join("secret.gpg"), ciphertext).unwrap();

        apply_server_rename(&vault, "secret.gpg", "../stolen.gpg");

        assert_eq!(std::fs::read(vault.join("secret.gpg")).unwrap(), ciphertext);
        assert!(!dir.join("stolen.gpg").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_device_binary_row_is_not_journaled_as_a_phantom_file() {
        let dir = temp_dir("phantom-binary");
        let vault = vault_in(&dir);
        let journal = journal::Journal::open(&dir.join("journal.db")).unwrap();
        let file = space_file::SpaceFile::new(
            ID_A.to_string(),
            "site.com/user.gpg".to_string(),
            String::new(),
            8,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        download_server_file(&journal, &vault, &file, "Downloaded new");

        assert!(!vault.join("site.com/user.gpg").exists());
        assert!(journal.by_path("site.com/user.gpg").unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn binary_row_update_with_the_file_on_disk_refreshes_the_journal() {
        let dir = temp_dir("binary-journal-refresh");
        let vault = vault_in(&dir);
        let ciphertext: &[u8] = &[0x85, 0x02, 0x0c];
        std::fs::write(vault.join("secret.gpg"), ciphertext).unwrap();
        let journal = journal::Journal::open(&dir.join("journal.db")).unwrap();
        let file = space_file::SpaceFile::new(
            ID_A.to_string(),
            "secret.gpg".to_string(),
            String::new(),
            3,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        download_server_file(&journal, &vault, &file, "Downloaded update");

        assert_eq!(std::fs::read(vault.join("secret.gpg")).unwrap(), ciphertext);
        assert_eq!(journal.by_path("secret.gpg").unwrap().unwrap().uuid, ID_A);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn text_row_download_writes_and_journals() {
        let dir = temp_dir("text-download");
        let vault = vault_in(&dir);
        let journal = journal::Journal::open(&dir.join("journal.db")).unwrap();
        let file = space_file::SpaceFile::new(
            ID_A.to_string(),
            "a.md".to_string(),
            "body\n".to_string(),
            5,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        download_server_file(&journal, &vault, &file, "Downloaded new");

        assert_eq!(std::fs::read_to_string(vault.join("a.md")).unwrap(), "body\n");
        assert_eq!(journal.by_path("a.md").unwrap().unwrap().uuid, ID_A);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
