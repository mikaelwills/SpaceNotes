use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::client::SpacetimeClient;
use crate::note::Note;
use crate::scanner::scan_notes;
use crate::tracker::ContentTracker;
use crate::writer::write_note_to_disk;

/// Reconcile local vault with SpacetimeDB on startup
/// Uses last-write-wins based on timestamps
pub fn reconcile_on_startup(
    vault_path: &Path,
    client: &SpacetimeClient,
    tracker: &ContentTracker,
) -> Result<()> {
    // 1. Get all notes from SpacetimeDB
    let server_notes = client.get_all_notes();

    // 2. Get all local notes
    let local_notes = scan_notes(vault_path)?;

    // 3. Build lookup maps by ID
    let server_map: HashMap<String, Note> = server_notes
        .into_iter()
        .map(|n| (n.id.clone(), n))
        .collect();

    let local_map: HashMap<String, Note> = local_notes
        .into_iter()
        .map(|n| (n.id.clone(), n))
        .collect();

    // 4. Reconcile each note by ID
    let all_ids: HashSet<&String> = server_map.keys().chain(local_map.keys()).collect();

    let mut downloaded = 0;
    let mut uploaded = 0;
    let mut unchanged = 0;

    for id in all_ids {
        match (local_map.get(id), server_map.get(id)) {
            // Both exist - compare timestamps
            (Some(local), Some(server)) => {
                if server.modified_time > local.modified_time {
                    // Server is newer - download to disk
                    tracker.update(&server.id, &server.content);
                    write_note_to_disk(vault_path, server)?;
                    tracing::debug!("Downloaded newer: {} (ID: {})", server.path, id);
                    downloaded += 1;
                } else if local.modified_time > server.modified_time {
                    // Local is newer - push to server
                    tracker.update(&local.id, &local.content);
                    client.upsert_note(local);
                    tracing::debug!("Uploaded newer: {} (ID: {})", local.path, id);
                    uploaded += 1;
                } else {
                    // Equal timestamps - just update tracker
                    tracker.update(&local.id, &local.content);
                    unchanged += 1;
                }
            }

            // Only on server - download
            (None, Some(server)) => {
                tracker.update(&server.id, &server.content);
                write_note_to_disk(vault_path, server)?;
                tracing::debug!("Downloaded new: {} (ID: {})", server.path, id);
                downloaded += 1;
            }

            // Only local - upload (WARNING: resurrects deleted files)
            (Some(local), None) => {
                tracker.update(&local.id, &local.content);
                client.upsert_note(local);
                tracing::debug!("Uploaded new: {} (ID: {})", local.path, id);
                uploaded += 1;
            }

            (None, None) => unreachable!(),
        }
    }

    tracing::info!(
        "Reconciliation complete: {} downloaded, {} uploaded, {} unchanged",
        downloaded,
        uploaded,
        unchanged
    );

    Ok(())
}
