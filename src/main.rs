mod client;
mod folder;
mod frontmatter;
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
use std::path::PathBuf;
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

    // Initialize content tracker for loop prevention
    let tracker = Arc::new(ContentTracker::new());

    // Connect to SpacetimeDB
    let client = Arc::new(
        client::SpacetimeClient::connect(&args.spacetime_host, &args.database)?
    );

    // Wait for initial subscription data
    tracing::info!("Waiting for subscription sync...");
    client.wait_for_sync()?;

    // Reconcile local vault with server (two-way sync)
    tracing::info!("Reconciling with server...");
    reconcile::reconcile_on_startup(&absolute_vault_path, &client, &tracker)?;

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

    // Register callback for note updates from server
    let vault_clone = absolute_vault_path.clone();
    let tracker_clone = tracker.clone();
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
            tracing::info!("Downloaded update: {}", note.path);
        }
    });

    // Register callback for note inserts from server
    let vault_clone = absolute_vault_path.clone();
    let tracker_clone = tracker.clone();
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
            tracing::info!("Downloaded new: {}", note.path);
        }
    });

    // Register callback for note deletions from server
    let vault_clone = absolute_vault_path.clone();
    let tracker_clone = tracker.clone();
    client.on_note_deleted(move |old_note| {
        let path = vault_clone.join(&old_note.path);
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::error!("Failed to delete {}: {}", old_note.path, e);
            } else {
                tracker_clone.remove(&old_note.id);
                tracing::info!("Deleted local file: {}", old_note.path);
            }
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
    watcher::start_watcher(absolute_vault_path, client, tracker).await?;

    Ok(())
}
