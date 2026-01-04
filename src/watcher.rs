use anyhow::Result;
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use crate::client::SpacetimeClient;
use crate::folder::Folder;
use crate::frontmatter::inject_spacetime_id;
use crate::sanitize::sanitize_path;
use crate::scanner::{read_note_at, scan_for_note_by_id};
use crate::tracker::ContentTracker;

pub async fn start_watcher(
    vault_path: PathBuf,
    client: Arc<SpacetimeClient>,
    tracker: Arc<ContentTracker>,
) -> Result<()> {
    let vault_path_clone = vault_path.clone();

    let mut debouncer = new_debouncer(
        Duration::from_secs(2),
        move |res: DebounceEventResult| {
            match res {
                Ok(events) => {
                    for event in events {
                        let path = &event.path;

                        // Skip hidden files/directories and Synology system folders
                        if path.iter().any(|name| {
                            name.to_str().map_or(false, |s| s.starts_with('.') || s == "@eaDir")
                        }) {
                            continue;
                        }

                        // Handle markdown files
                        if path.extension().map_or(false, |e| e == "md") {
                            match read_note_at(&vault_path_clone, path) {
                                Ok(Some(mut note)) => {
                                    // CHECK TRACKER (Echo Prevention)
                                    // If we extracted an ID, and the content hasn't changed, STOP.
                                    if !note.id.is_empty() {
                                        let content_hash = ContentTracker::hash(&note.content);
                                        let has_changed = tracker.has_changed(&note.id, &note.content);
                                        tracing::info!(
                                            "Watcher echo check: path={}, id={}, content_len={}, hash={}, has_changed={}",
                                            note.path, note.id, note.content.len(), &content_hash[..16], has_changed
                                        );
                                        if !has_changed {
                                            tracing::info!("Watcher ignoring echo: {}", note.path);
                                            continue;
                                        }
                                    }

                                    // Check if note has a UUID
                                    if note.id.is_empty() {
                                        // SAFETY CHECK: Does the DB already know about this file?
                                        // If yes, our read failed to parse the UUID (race condition or bad format).
                                        // Do NOT inject a new UUID, or we'll split-brain the file.
                                        if let Some(existing) = client.get_note_by_path(&note.path) {
                                            tracing::warn!(
                                                "Safety Stop: Note {} has no UUID on disk, but DB knows it as {}. Skipping injection to prevent split-brain.",
                                                note.path, existing.id
                                            );
                                            continue;
                                        }

                                        // SAFETY BRAKE: double check raw text before injecting
                                        if let Ok(raw_content) = std::fs::read_to_string(path) {
                                            if raw_content.contains("spacetime_id:") {
                                                tracing::error!(
                                                    "CRITICAL: spacetime_id found in text but parsing failed. Skipping injection for safety: {}",
                                                    note.path
                                                );
                                                continue;
                                            }

                                            // New file without UUID - inject one
                                            let new_id = Uuid::new_v4().to_string();
                                            tracing::info!("Injecting UUID {} into {}", new_id, note.path);

                                            let new_content = inject_spacetime_id(&raw_content, &new_id);
                                            if let Err(e) = std::fs::write(path, &new_content) {
                                                tracing::error!("Failed to inject UUID into {}: {}", note.path, e);
                                                continue;
                                            }
                                            // Update note object with new ID
                                            note.id = new_id;
                                        } else {
                                            tracing::error!("Failed to read {} for UUID injection", note.path);
                                            continue;
                                        }
                                    }

                                    // UPSERT (Only if tracker says content changed)
                                    if tracker.is_modified(&note.id, &note.content) {
                                        client.upsert_note(&note);
                                        tracker.update(&note.id, &note.content);
                                        tracing::info!("Synced: {} (ID: {})", note.name, note.id);
                                    } else {
                                        tracing::debug!("Skipping unchanged: {} (ID: {})", note.path, note.id);
                                    }
                                }
                                Ok(None) => {
                                    // File was deleted - look up ID from client cache
                                    if let Ok(rel) = path.strip_prefix(&vault_path_clone) {
                                        let rel_path = sanitize_path(&rel.to_string_lossy().to_string());

                                        // Find the note in the client cache by path
                                        let notes = client.get_all_notes();
                                        if let Some(note) = notes.iter().find(|n| n.path == rel_path) {
                                            client.delete_note(&note.id);
                                            tracker.remove(&note.id);
                                            tracing::info!("Deleted note: {} (ID: {})", rel_path, note.id);
                                        } else {
                                            tracing::warn!("Note deleted but not found in DB: {}", rel_path);
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("Error processing {:?}: {}", path, e);
                                }
                            }
                        }
                        // Handle directories (check is_dir first, then handle deleted dirs)
                        else if path.is_dir() {
                            // Directory exists - created or modified
                            if let Ok(rel) = path.strip_prefix(&vault_path_clone) {
                                let rel_path = sanitize_path(&rel.to_string_lossy().to_string());
                                let folder = Folder::new(rel_path.clone());
                                client.upsert_folder(&folder);
                                tracing::info!("Synced folder: {}", rel_path);
                            }
                        }
                        // Handle deleted directories (no extension and doesn't exist)
                        else if path.extension().is_none() && !path.exists() {
                            if let Ok(rel) = path.strip_prefix(&vault_path_clone) {
                                let old_folder_path = sanitize_path(&rel.to_string_lossy().to_string());

                                // Get all notes that were in this folder from DB
                                let notes_in_folder: Vec<_> = client.get_all_notes()
                                    .into_iter()
                                    .filter(|n| n.path.starts_with(&format!("{}/", old_folder_path)))
                                    .collect();

                                // Check if notes still exist on disk (indicates folder rename)
                                for note in &notes_in_folder {
                                    let old_path = vault_path_clone.join(&note.path);
                                    if !old_path.exists() {
                                        // Note missing at old path - try to find by UUID
                                        match scan_for_note_by_id(&vault_path_clone, &note.id) {
                                            Ok(Some(mut new_note)) => {
                                                // Found it at new location! Update path in DB
                                                if new_note.id.is_empty() {
                                                    new_note.id = note.id.clone();
                                                }
                                                client.upsert_note(&new_note);
                                                tracker.update(&new_note.id, &new_note.content);
                                                tracing::info!("Updated note path: {} -> {}", note.path, new_note.path);
                                            }
                                            Ok(None) => {
                                                // Note truly deleted
                                                client.delete_note(&note.id);
                                                tracker.remove(&note.id);
                                                tracing::info!("Deleted note: {} (ID: {})", note.path, note.id);
                                            }
                                            Err(e) => {
                                                tracing::error!("Error scanning for note {}: {}", note.id, e);
                                            }
                                        }
                                    }
                                }

                                client.delete_folder(&old_folder_path);
                                tracing::info!("Deleted folder: {}", old_folder_path);
                            }
                        }
                    }
                }
                Err(e) => tracing::error!("Watch error: {:?}", e),
            }
        },
    )?;

    debouncer
        .watcher()
        .watch(&vault_path, RecursiveMode::Recursive)?;

    tracing::info!("Watcher started on {:?}", vault_path);

    // Keep alive indefinitely
    std::future::pending::<()>().await;

    Ok(())
}
