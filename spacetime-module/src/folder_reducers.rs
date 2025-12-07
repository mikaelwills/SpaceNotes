use spacetimedb::{ReducerContext, Table};

use crate::{Folder, folder, note};

// =============================================================================
// Folder Reducers
// =============================================================================

#[spacetimedb::reducer]
pub fn create_folder(ctx: &ReducerContext, path: String, name: String, depth: u32) {
    // Normalize: strip trailing slash to match storage standard
    let normalized_path = path.trim_end_matches('/').to_string();

    if ctx.db.folder().path().find(&normalized_path).is_some() {
        log::warn!("Folder already exists: {}", normalized_path);
        return;
    }

    ctx.db.folder().insert(Folder {
        path: normalized_path.clone(),
        name,
        depth,
    });
    log::info!("Created folder: {}", normalized_path);
}

#[spacetimedb::reducer]
pub fn delete_folder(ctx: &ReducerContext, path: String) {
    // Normalize: strip trailing slash to match storage standard
    let normalized_path = path.trim_end_matches('/').to_string();

    if ctx.db.folder().path().find(&normalized_path).is_none() {
        log::warn!("Folder not found for deletion: {}", normalized_path);
        return;
    }

    // For cascade operations, use path with slash to match note.folder_path
    let path_with_slash = format!("{}/", normalized_path);

    // CASCADE: Delete all notes inside this folder (and subfolders)
    let notes_to_delete: Vec<String> = ctx
        .db
        .note()
        .iter()
        .filter(|note| note.folder_path.starts_with(&path_with_slash))
        .map(|note| note.id.clone())
        .collect();

    for note_id in &notes_to_delete {
        ctx.db.note().id().delete(note_id);
    }

    if !notes_to_delete.is_empty() {
        log::info!("Cascade deleted {} notes from folder: {}", notes_to_delete.len(), normalized_path);
    }

    // CASCADE: Delete all subfolders (use normalized path for comparison)
    let subfolders_to_delete: Vec<String> = ctx
        .db
        .folder()
        .iter()
        .filter(|f| f.path.starts_with(&normalized_path) && f.path != normalized_path)
        .map(|f| f.path.clone())
        .collect();

    for subfolder_path in &subfolders_to_delete {
        ctx.db.folder().path().delete(subfolder_path);
    }

    if !subfolders_to_delete.is_empty() {
        log::info!("Cascade deleted {} subfolders from: {}", subfolders_to_delete.len(), normalized_path);
    }

    // Delete the folder itself
    ctx.db.folder().path().delete(&normalized_path);
    log::info!("Deleted folder: {}", normalized_path);
}

#[spacetimedb::reducer]
pub fn move_folder(ctx: &ReducerContext, old_path: String, new_path: String) {
    // Normalize: strip trailing slashes
    let old_normalized = old_path.trim_end_matches('/').to_string();
    let new_normalized = new_path.trim_end_matches('/').to_string();

    // Verify source folder exists
    if ctx.db.folder().path().find(&old_normalized).is_none() {
        log::warn!("Folder not found for move: {}", old_normalized);
        return;
    }

    // Check if destination already exists
    if ctx.db.folder().path().find(&new_normalized).is_some() {
        log::error!("Cannot move: Destination folder already exists: {}", new_normalized);
        return;
    }

    // Calculate new metadata for the folder
    let new_name = new_normalized
        .rsplit('/')
        .next()
        .unwrap_or(&new_normalized)
        .to_string();
    let new_depth = new_normalized.matches('/').count() as u32;

    // For cascade operations, use paths with slashes
    let old_path_with_slash = format!("{}/", old_normalized);
    let new_path_with_slash = format!("{}/", new_normalized);

    // CASCADE 1: Update all notes inside this folder
    let notes_to_update: Vec<_> = ctx
        .db
        .note()
        .iter()
        .filter(|note| note.folder_path.starts_with(&old_path_with_slash))
        .collect();

    let notes_count = notes_to_update.len();
    for note in notes_to_update {
        // Calculate new paths for the note
        let new_note_folder_path = note.folder_path.replacen(&old_path_with_slash, &new_path_with_slash, 1);
        let new_note_path = note.path.replacen(&old_path_with_slash, &new_path_with_slash, 1);
        let new_note_depth = new_note_path.matches('/').count() as u32;

        // Delete old entry and insert with updated paths
        ctx.db.note().id().delete(&note.id);
        ctx.db.note().insert(crate::Note {
            id: note.id.clone(),
            path: new_note_path,
            name: note.name,
            content: note.content,
            folder_path: new_note_folder_path,
            depth: new_note_depth,
            frontmatter: note.frontmatter,
            size: note.size,
            created_time: note.created_time,
            modified_time: note.modified_time,
            db_updated_at: ctx.timestamp,
        });
    }

    if notes_count > 0 {
        log::info!("Cascade updated {} notes in folder move", notes_count);
    }

    // CASCADE 2: Update all subfolders
    let subfolders_to_update: Vec<_> = ctx
        .db
        .folder()
        .iter()
        .filter(|f| f.path.starts_with(&old_normalized) && f.path != old_normalized)
        .collect();

    let subfolders_count = subfolders_to_update.len();
    for subfolder in subfolders_to_update {
        // Calculate new path for subfolder
        let new_subfolder_path = subfolder.path.replacen(&old_normalized, &new_normalized, 1);
        let new_subfolder_name = new_subfolder_path
            .rsplit('/')
            .next()
            .unwrap_or(&new_subfolder_path)
            .to_string();
        let new_subfolder_depth = new_subfolder_path.matches('/').count() as u32;

        // Delete old entry and insert with updated path
        ctx.db.folder().path().delete(&subfolder.path);
        ctx.db.folder().insert(Folder {
            path: new_subfolder_path,
            name: new_subfolder_name,
            depth: new_subfolder_depth,
        });
    }

    if subfolders_count > 0 {
        log::info!("Cascade updated {} subfolders in folder move", subfolders_count);
    }

    // Move the folder itself
    ctx.db.folder().path().delete(&old_normalized);
    ctx.db.folder().insert(Folder {
        path: new_normalized.clone(),
        name: new_name,
        depth: new_depth,
    });

    log::info!("Moved folder: {} -> {} (with {} notes, {} subfolders)",
               old_normalized, new_normalized, notes_count, subfolders_count);
}

#[spacetimedb::reducer]
pub fn upsert_folder(ctx: &ReducerContext, path: String, name: String, depth: u32) {
    // Normalize: strip trailing slash to match storage standard
    let normalized_path = path.trim_end_matches('/').to_string();

    // Delete if exists, then insert
    if ctx.db.folder().path().find(&normalized_path).is_some() {
        ctx.db.folder().path().delete(&normalized_path);
    }
    ctx.db.folder().insert(Folder {
        path: normalized_path,
        name,
        depth
    });
}
