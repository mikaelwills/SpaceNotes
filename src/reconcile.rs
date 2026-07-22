use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::client::SpacetimeClient;
use crate::isolation::run_isolated;
use crate::note::Note;
use crate::scanner::scan_notes;
use crate::tracker::ContentTracker;
use crate::writer::write_note_to_disk;

enum Outcome {
    Downloaded,
    Uploaded,
    Unchanged,
    Skipped,
}

fn reconcile_one(
    vault_path: &Path,
    client: &SpacetimeClient,
    tracker: &ContentTracker,
    local_map: &HashMap<String, Note>,
    server_map: &HashMap<String, Note>,
    id: &str,
) -> Result<Outcome> {
    match (local_map.get(id), server_map.get(id)) {
        (Some(local), Some(server)) => {
            if server.modified_time > local.modified_time {
                write_note_to_disk(vault_path, server)?;
                tracker.update(&server.id, &server.content);
                tracing::debug!("Downloaded newer: {} (ID: {})", server.path, id);
                Ok(Outcome::Downloaded)
            } else if local.modified_time > server.modified_time {
                client.upsert_note(local);
                tracker.update(&local.id, &local.content);
                tracing::debug!("Uploaded newer: {} (ID: {})", local.path, id);
                Ok(Outcome::Uploaded)
            } else {
                tracker.update(&local.id, &local.content);
                Ok(Outcome::Unchanged)
            }
        }

        (None, Some(server)) => {
            write_note_to_disk(vault_path, server)?;
            tracker.update(&server.id, &server.content);
            tracing::debug!("Downloaded new: {} (ID: {})", server.path, id);
            Ok(Outcome::Downloaded)
        }

        (Some(local), None) => {
            client.upsert_note(local);
            tracker.update(&local.id, &local.content);
            tracing::debug!("Uploaded new: {} (ID: {})", local.path, id);
            Ok(Outcome::Uploaded)
        }

        (None, None) => unreachable!(),
    }
}

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
        let mut outcome: Result<Outcome> = Ok(Outcome::Skipped);

        let context = format!("reconcile note (ID: {})", id);
        run_isolated(context, || {
            outcome = reconcile_one(vault_path, client, tracker, &local_map, &server_map, id);
        });

        match outcome? {
            Outcome::Downloaded => downloaded += 1,
            Outcome::Uploaded => uploaded += 1,
            Outcome::Unchanged => unchanged += 1,
            Outcome::Skipped => {}
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
