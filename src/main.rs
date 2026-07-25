mod client;
mod folder;
mod isolation;
mod journal;
mod migrate;
mod note;
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
use crate::writer::write_note_to_disk;

#[derive(Parser, Debug)]
#[command(name = "spacenotes")]
#[command(about = "Sync markdown notes to SpacetimeDB")]
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

        let folder_path = absolute_vault_path.join(&server_folder.path);
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

    // Register callback for note updates from server
    let vault_clone = absolute_vault_path.clone();
    let tracker_clone = tracker.clone();
    let journal_clone = opened_journal.journal.clone();
    client.on_note_updated(move |old_note, new_note| {
        let path_changed = old_note.path != new_note.path;
        let content_changed = tracker_clone.is_modified(&new_note.id, &new_note.content);

        // Skip if nothing changed (echo from our own update)
        if !path_changed && !content_changed {
            tracing::debug!("Skipping update echo: {}", new_note.path);
            return;
        }

        // If path changed, delete the old file (this is a rename)
        if old_note.path != new_note.path {
            let old_path = vault_clone.join(&old_note.path);
            if old_path.exists() {
                if let Err(e) = std::fs::remove_file(&old_path) {
                    tracing::error!("Failed to delete old file {}: {}", old_note.path, e);
                } else {
                    tracing::info!("Deleted old file during rename: {}", old_note.path);
                }
            }
        }

        // Convert DbNote to LocalNote for writer
        let note = note::Note {
            id: new_note.id.clone(),
            path: new_note.path.clone(),
            name: new_note.name.clone(),
            content: new_note.content.clone(),
            folder_path: new_note.folder_path.clone(),
            depth: new_note.depth,
            frontmatter: new_note.frontmatter.clone(),
            size: new_note.size,
            created_time: new_note.created_time,
            modified_time: new_note.modified_time,
        };

        tracker_clone.update(&note.id, &note.content);
        if let Err(e) = write_note_to_disk(&vault_clone, &note) {
            tracing::error!("Failed to write {}: {}", note.path, e);
        } else {
            record_note_in_journal(&journal_clone, &vault_clone, &note);
            tracing::info!("Downloaded update: {}", note.path);
        }
    });

    // Register callback for note inserts from server
    let vault_clone = absolute_vault_path.clone();
    let tracker_clone = tracker.clone();
    let journal_clone = opened_journal.journal.clone();
    client.on_note_inserted(move |db_note| {
        // Skip if we already have this content (echo from our own upload)
        if !tracker_clone.is_modified(&db_note.id, &db_note.content) {
            tracing::debug!("Skipping insert echo: {}", db_note.path);
            return;
        }

        let note = note::Note {
            id: db_note.id.clone(),
            path: db_note.path.clone(),
            name: db_note.name.clone(),
            content: db_note.content.clone(),
            folder_path: db_note.folder_path.clone(),
            depth: db_note.depth,
            frontmatter: db_note.frontmatter.clone(),
            size: db_note.size,
            created_time: db_note.created_time,
            modified_time: db_note.modified_time,
        };

        tracker_clone.update(&note.id, &note.content);
        if let Err(e) = write_note_to_disk(&vault_clone, &note) {
            tracing::error!("Failed to write {}: {}", note.path, e);
        } else {
            record_note_in_journal(&journal_clone, &vault_clone, &note);
            tracing::info!("Downloaded new: {}", note.path);
        }
    });

    // Register callback for note deletions from server
    let vault_clone = absolute_vault_path.clone();
    let tracker_clone = tracker.clone();
    let journal_clone = opened_journal.journal.clone();
    client.on_note_deleted(move |old_note| {
        let path = vault_clone.join(&old_note.path);
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::error!("Failed to delete {}: {}", old_note.path, e);
                return;
            }
            tracker_clone.remove(&old_note.id);
            tracing::info!("Deleted local file: {}", old_note.path);
        }
        if let Err(e) = journal_clone.tombstone(&old_note.id, journal::now_ms()) {
            tracing::error!("Journal tombstone failed for {}: {}", old_note.path, e);
        }
    });

    // Register callback for folder inserts from server
    let vault_clone = absolute_vault_path.clone();
    client.on_folder_inserted(move |new_folder| {
        // Skip @eaDir folders (Synology metadata)
        if new_folder.path.contains("@eaDir") {
            return;
        }

        let path = vault_clone.join(&new_folder.path);
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
        let path = vault_clone.join(&old_folder.path);
        if path.exists() && path.is_dir() {
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
        let old_path = vault_clone.join(&old_folder.path);
        let new_path = vault_clone.join(&new_folder.path);

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

fn record_note_in_journal(journal: &journal::Journal, vault_path: &Path, note: &note::Note) {
    let abs = vault_path.join(&note.path);
    match journal::record_from_disk(vault_path, &abs, note.id.clone()) {
        Ok(record) => {
            if let Err(e) = journal.observe(&record, "create") {
                tracing::error!("Journal record failed for {}: {}", record.path, e);
            }
        }
        Err(e) => tracing::error!("Journal record failed for {}: {}", note.path, e),
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
