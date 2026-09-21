mod client;
mod folder;
mod isolation;
mod journal;
mod migrate;
mod pgp_probe;
mod space_file;
mod reconcile;
mod sanitize;
mod scanner;
mod spacetime_bindings;
mod thumbnail;
mod tracker;
mod vault_path;
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

    /// Port for the vault file server (`/files/`, `/thumbnails/`). nginx
    /// proxies to this; it is not exposed outside the container.
    #[arg(long, env = "FILES_PORT", default_value = "5057")]
    files_port: u16,
}

/// Serves vault bytes for nginx to proxy at `/files/`.
///
/// Called before any database work and spawned rather than awaited: it needs
/// only the vault path, and anything that waited for SpacetimeDB would turn a
/// slow startup into 502s on every file request.
fn spawn_file_server(vault_path: std::path::PathBuf, port: u16) {
    match spacenotes::uploads::sweep_stale(&vault_path) {
        0 => {}
        swept => tracing::info!("Dropped {swept} abandoned upload(s)"),
    }

    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
            Ok(listener) => {
                if let Err(e) = spacenotes::files_http::serve(vault_path, listener).await {
                    tracing::error!("File server stopped: {e}");
                }
            }
            Err(e) => tracing::error!("File server could not bind port {port}: {e}"),
        }
    });
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

    spawn_file_server(absolute_vault_path.clone(), args.files_port);

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

    let thumbnails = Arc::new(thumbnail::ThumbnailQueue::start(
        absolute_vault_path.clone(),
        client.clone(),
    )?);

    // Reconcile local vault with server (two-way sync)
    tracing::info!("Reconciling with server...");
    reconcile::reconcile_on_startup(
        &absolute_vault_path,
        &client,
        &tracker,
        &opened_journal.journal,
        &thumbnails,
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
    client.on_file_updated(move |old_file, file| {
        apply_server_update(
            &vault_clone,
            &tracker_clone,
            &journal_clone,
            &old_file.path,
            &file,
        );
    });

    // Register callback for file inserts from server
    let vault_clone = absolute_vault_path.clone();
    let tracker_clone = tracker.clone();
    let journal_clone = opened_journal.journal.clone();
    client.on_file_inserted(move |file| {
        let signal = file.content.clone();
        // Skip if we already have this content (echo from our own upload)
        if !tracker_clone.has_changed(&file.id, &signal) {
            tracing::debug!("Skipping insert echo: {}", file.path);
            return;
        }

        if download_server_file(&journal_clone, &vault_clone, &file, "Downloaded new")
            == Some(writer::WriteOutcome::Written)
        {
            tracker_clone.update(&file.id, &signal);
        }
    });

    // Register callback for file deletions from server
    let vault_clone = absolute_vault_path.clone();
    let tracker_clone = tracker.clone();
    let journal_clone = opened_journal.journal.clone();
    client.on_file_deleted(move |old_file| {
        thumbnail::remove_thumbnail_file(&vault_clone, &old_file.id);
        let path = match writer::resolve_vault_path(&vault_clone, &old_file.path) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Refusing server delete of {}: {}", old_file.path, e);
                return;
            }
        };
        if path.exists() {
            // The tombstone below must still be written when this refuses, or the
            // row stays live while the server row is gone and every reconcile
            // re-uploads the file.
            if is_protected_store_dotfile(&old_file.path) {
                tracing::warn!(
                    "Refusing to delete {} from disk: the store's recipient list is vault truth and a swapped one redirects every encryption",
                    old_file.path
                );
            } else if let Err(e) = std::fs::remove_file(&path) {
                tracing::error!("Failed to delete {}: {}", old_file.path, e);
                return;
            } else {
                tracker_clone.remove(&old_file.id);
                tracing::info!("Deleted local file: {}", old_file.path);
            }
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
                use std::os::unix::fs::PermissionsExt;
                if let Err(e) =
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777))
                {
                    tracing::error!(
                        "Failed to set permissions on folder {}: {}",
                        new_folder.path,
                        e
                    );
                }
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
        if scanner::is_credential_store_path(&old_folder.path)
            || scanner::is_credential_store_path(&new_folder.path)
        {
            tracing::warn!(
                "Refusing server folder rename {} -> {}: the credential store moves only at the vault",
                old_folder.path,
                new_folder.path
            );
            return;
        }

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
    watcher::start_watcher(absolute_vault_path, client, tracker, watcher_journal, thumbnails)
        .await?;

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
    if is_protected_store_dotfile(old_rel) || is_protected_store_dotfile(new_rel) {
        tracing::warn!(
            "Refusing server rename {} -> {}: the store's recipient list is vault truth and a swapped one redirects every encryption",
            old_rel,
            new_rel
        );
        return false;
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
        return apply_credential_rename(old_rel, new_rel, &old_path, &new_path);
    }
    if let Err(e) = std::fs::remove_file(&old_path) {
        tracing::error!("Failed to delete old file {}: {}", old_rel, e);
    } else {
        tracing::info!("Deleted old file during rename: {}", old_rel);
    }
    true
}

fn is_protected_store_dotfile(rel_path: &str) -> bool {
    if !scanner::is_credential_store_path(rel_path) {
        return false;
    }
    Path::new(rel_path)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(scanner::is_allowed_dotfile)
}

fn apply_credential_rename(
    old_rel: &str,
    new_rel: &str,
    old_path: &Path,
    new_path: &Path,
) -> bool {
    if !scanner::is_credential_store_path(old_rel) || !scanner::is_credential_store_path(new_rel) {
        tracing::warn!(
            "Refusing server rename {} -> {}: credentials move only at the vault, never because a row's path changed",
            old_rel,
            new_rel
        );
        return false;
    }
    if old_path.parent() != new_path.parent() {
        tracing::warn!(
            "Refusing server rename {} -> {}: a credential never moves between directories",
            old_rel,
            new_rel
        );
        return false;
    }
    if new_path.exists() {
        tracing::warn!(
            "Refusing server rename {} -> {}: the destination is occupied and its bytes would be destroyed",
            old_rel,
            new_rel
        );
        return false;
    }
    if let Err(e) = std::fs::rename(old_path, new_path) {
        tracing::error!("Failed to rename credential {} -> {}: {}", old_rel, new_rel, e);
        return false;
    }
    tracing::info!("Renamed credential {} -> {}", old_rel, new_rel);
    true
}

fn apply_server_update(
    vault: &Path,
    tracker: &tracker::ContentTracker,
    journal: &journal::Journal,
    old_path: &str,
    new_file: &space_file::SpaceFile,
) {
    let path_changed = old_path != new_file.path;
    let signal = new_file.change_signal();
    let content_changed = tracker.has_changed(&new_file.id, &signal);

    if !path_changed && !content_changed {
        tracing::debug!("Skipping update echo: {}", new_file.path);
        return;
    }

    if path_changed
        && scanner::is_binary(Path::new(&new_file.path))
        && scanner::is_credential_store_path(&new_file.path)
        && !writer::credential_write_is_valid(vault, new_file)
    {
        tracing::warn!(
            "Refusing credential rename {} -> {}: the content arriving with it is not a valid credential, so the bytes stay at their old name",
            old_path,
            new_file.path
        );
        return;
    }

    if path_changed && !apply_server_rename(vault, old_path, &new_file.path) {
        return;
    }

    if path_changed && scanner::is_credential_store_path(&new_file.path) {
        if let Err(e) = journal.rekey(&new_file.id, &new_file.path, journal::now_ms() as i64) {
            tracing::error!("Journal rekey failed for {}: {}", new_file.path, e);
        }
    }

    if download_server_file(journal, vault, new_file, "Downloaded update")
        == Some(writer::WriteOutcome::Written)
    {
        tracker.update(&new_file.id, &signal);
    }
}

fn download_server_file(
    journal: &journal::Journal,
    vault_path: &Path,
    file: &space_file::SpaceFile,
    verb: &str,
) -> Option<writer::WriteOutcome> {
    match write_file_to_disk(vault_path, file) {
        Ok(writer::WriteOutcome::Written) => {
            record_file_in_journal(journal, vault_path, file);
            tracing::info!("{}: {}", verb, file.path);
            Some(writer::WriteOutcome::Written)
        }
        Ok(writer::WriteOutcome::SkippedBinary) => {
            if vault_path.join(&file.path).exists() {
                record_file_in_journal(journal, vault_path, file);
            }
            tracing::debug!(
                "Skipped server->disk write for {}: binary bytes are not stored in the database",
                file.path
            );
            Some(writer::WriteOutcome::SkippedBinary)
        }
        Err(e) => {
            tracing::error!("Failed to write {}: {}", file.path, e);
            None
        }
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
    fn server_rename_of_a_gpg_outside_the_store_is_refused_before_any_destination_check() {
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

    const CRED_ID: &str = "cccccccc-cccc-cccc-cccc-cccccccccccc";

    fn seed_credential_store(vault: &Path) {
        crate::writer::tests::seed_store(vault);
    }

    fn credential_row(path: &str, content: String) -> space_file::SpaceFile {
        space_file::SpaceFile::new(
            CRED_ID.to_string(),
            path.to_string(),
            content,
            0,
            1_600_000_000_000,
            1_600_000_000_000,
        )
    }

    fn put_credential(vault: &Path, rel: &str, bytes: &[u8]) {
        let abs = vault.join(rel);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, bytes).unwrap();
    }

    #[test]
    fn a_same_directory_credential_rename_succeeds_and_preserves_the_bytes() {
        let dir = temp_dir("cred-rename-ok");
        let vault = vault_in(&dir);
        seed_credential_store(&vault);
        let ciphertext = crate::writer::tests::valid_ciphertext();
        put_credential(&vault, ".password-store/a.com/old.gpg", &ciphertext);

        assert!(apply_server_rename(
            &vault,
            ".password-store/a.com/old.gpg",
            ".password-store/a.com/new.gpg"
        ));

        assert!(!vault.join(".password-store/a.com/old.gpg").exists());
        assert_eq!(
            std::fs::read(vault.join(".password-store/a.com/new.gpg")).unwrap(),
            ciphertext
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cross_directory_credential_rename_is_refused() {
        let dir = temp_dir("cred-rename-cross");
        let vault = vault_in(&dir);
        seed_credential_store(&vault);
        let ciphertext = crate::writer::tests::valid_ciphertext();
        put_credential(&vault, ".password-store/a.com/u.gpg", &ciphertext);

        assert!(!apply_server_rename(
            &vault,
            ".password-store/a.com/u.gpg",
            ".password-store/b.com/u.gpg"
        ));

        assert_eq!(
            std::fs::read(vault.join(".password-store/a.com/u.gpg")).unwrap(),
            ciphertext
        );
        assert!(!vault.join(".password-store/b.com/u.gpg").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_credential_rename_onto_an_occupied_destination_leaves_both_files_intact() {
        let dir = temp_dir("cred-rename-clobber");
        let vault = vault_in(&dir);
        seed_credential_store(&vault);
        let moving = crate::writer::tests::valid_ciphertext();
        let victim: &[u8] = &[0x99, 0x03, 0xbe, 0xef, 0x11];
        put_credential(&vault, ".password-store/a.com/from.gpg", &moving);
        put_credential(&vault, ".password-store/a.com/onto.gpg", victim);

        assert!(!apply_server_rename(
            &vault,
            ".password-store/a.com/from.gpg",
            ".password-store/a.com/onto.gpg"
        ));

        assert_eq!(
            std::fs::read(vault.join(".password-store/a.com/from.gpg")).unwrap(),
            moving,
            "the moving credential keeps its bytes"
        );
        assert_eq!(
            std::fs::read(vault.join(".password-store/a.com/onto.gpg")).unwrap(),
            victim,
            "the occupant is never destroyed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_credential_rename_outside_the_store_is_refused() {
        let dir = temp_dir("cred-rename-outside");
        let vault = vault_in(&dir);
        seed_credential_store(&vault);
        let ciphertext = crate::writer::tests::valid_ciphertext();
        put_credential(&vault, "site.com/u.gpg", &ciphertext);

        assert!(!apply_server_rename(
            &vault,
            "site.com/u.gpg",
            "site.com/v.gpg"
        ));

        assert_eq!(
            std::fs::read(vault.join("site.com/u.gpg")).unwrap(),
            ciphertext
        );
        assert!(!vault.join("site.com/v.gpg").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rename_arriving_with_invalid_content_leaves_the_file_at_its_old_name_and_bytes() {
        let dir = temp_dir("cred-order");
        let vault = vault_in(&dir);
        seed_credential_store(&vault);
        let journal = journal::Journal::open(&dir.join("journal.db")).unwrap();
        let tracker = tracker::ContentTracker::new();

        let original = crate::writer::tests::valid_ciphertext();
        put_credential(&vault, ".password-store/a.com/old.gpg", &original);

        let bad_content = crate::scanner::encode_binary_content(b"not an openpgp message at all");
        let row = credential_row(".password-store/a.com/new.gpg", bad_content.clone());

        apply_server_update(
            &vault,
            &tracker,
            &journal,
            ".password-store/a.com/old.gpg",
            &row,
        );

        assert_eq!(
            std::fs::read(vault.join(".password-store/a.com/old.gpg")).unwrap(),
            original,
            "behaviour 38: the bytes stay at the old name when validation refuses"
        );
        assert!(
            !vault.join(".password-store/a.com/new.gpg").exists(),
            "behaviour 38: nothing is created at the new name"
        );
        assert!(
            tracker.has_changed(CRED_ID, &bad_content),
            "the tracker must not commit a refused write, or the content never retries"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_valid_rename_with_new_content_applies_both_and_rekeys_the_journal() {
        let dir = temp_dir("cred-order-ok");
        let vault = vault_in(&dir);
        seed_credential_store(&vault);
        let journal = journal::Journal::open(&dir.join("journal.db")).unwrap();
        let tracker = tracker::ContentTracker::new();

        let original = crate::writer::tests::valid_ciphertext();
        put_credential(&vault, ".password-store/a.com/old.gpg", &original);
        let record = journal::record_from_disk(
            &vault,
            &vault.join(".password-store/a.com/old.gpg"),
            CRED_ID.to_string(),
        )
        .unwrap();
        journal.observe(&record, "create").unwrap();

        let mut fresh = crate::writer::tests::valid_ciphertext();
        fresh.extend_from_slice(&[0xAB, 0xCD]);
        let content = crate::scanner::encode_binary_content(&fresh);
        let row = credential_row(".password-store/a.com/new.gpg", content.clone());

        apply_server_update(
            &vault,
            &tracker,
            &journal,
            ".password-store/a.com/old.gpg",
            &row,
        );

        assert!(!vault.join(".password-store/a.com/old.gpg").exists());
        assert_eq!(
            std::fs::read(vault.join(".password-store/a.com/new.gpg")).unwrap(),
            fresh,
            "the rename and the new content land together"
        );
        assert_eq!(
            journal
                .by_path(".password-store/a.com/new.gpg")
                .unwrap()
                .unwrap()
                .uuid,
            CRED_ID,
            "the journal is rekeyed, or reconcile reverts the rename at the next restart"
        );
        assert!(!tracker.has_changed(CRED_ID, &content));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_in_store_recipient_dotfile_is_refused_for_server_rename() {
        let dir = temp_dir("dotfile-rename");
        let vault = vault_in(&dir);
        seed_credential_store(&vault);

        for name in [".gpg-id", ".gpg-pubkeys.asc"] {
            let rel = format!(".password-store/{name}");
            let abs = vault.join(&rel);
            std::fs::write(&abs, "C582F8C66A659D51!\n").unwrap();

            assert!(
                !apply_server_rename(&vault, &rel, ".password-store/moved.txt"),
                "{name} must not be renameable from the server"
            );
            assert!(abs.exists(), "{name} is still on disk");
            assert!(!vault.join(".password-store/moved.txt").exists());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_identically_named_dotfile_outside_the_store_is_unaffected() {
        let dir = temp_dir("dotfile-outside");
        let vault = vault_in(&dir);
        seed_credential_store(&vault);

        std::fs::create_dir_all(vault.join("elsewhere")).unwrap();
        std::fs::write(vault.join("elsewhere/.gpg-id"), "whatever\n").unwrap();

        assert!(
            apply_server_rename(&vault, "elsewhere/.gpg-id", "elsewhere/renamed.txt"),
            "the guard is scoped to the store, not to the filename"
        );
        assert!(!is_protected_store_dotfile("elsewhere/.gpg-id"));
        assert!(is_protected_store_dotfile(".password-store/.gpg-id"));
        assert!(is_protected_store_dotfile(
            ".password-store/.gpg-pubkeys.asc"
        ));
        assert!(!is_protected_store_dotfile(".password-store/a.com/u.gpg"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_refused_credential_write_leaves_the_content_writable_on_the_next_update() {
        let dir = temp_dir("cred-swallow");
        let vault = vault_in(&dir);
        seed_credential_store(&vault);
        let journal = journal::Journal::open(&dir.join("journal.db")).unwrap();
        let tracker = tracker::ContentTracker::new();

        let bad = crate::scanner::encode_binary_content(b"garbage");
        let refused = credential_row(".password-store/a.com/u.gpg", bad.clone());
        apply_server_update(&vault, &tracker, &journal, ".password-store/a.com/u.gpg", &refused);

        assert!(!vault.join(".password-store/a.com/u.gpg").exists());
        assert!(
            tracker.has_changed(CRED_ID, &bad),
            "a refused write must not commit the tracker"
        );

        let good = crate::writer::tests::valid_row_content();
        let retry = credential_row(".password-store/a.com/u.gpg", good);
        apply_server_update(&vault, &tracker, &journal, ".password-store/a.com/u.gpg", &retry);

        assert_eq!(
            std::fs::read(vault.join(".password-store/a.com/u.gpg")).unwrap(),
            crate::writer::tests::valid_ciphertext(),
            "the retry writes, because the tracker never swallowed the refusal"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
