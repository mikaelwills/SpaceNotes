use spacetimedb::{ReducerContext, Table};

use crate::{Folder, folder, note};

// =============================================================================
// Folder Reducers
// =============================================================================

#[spacetimedb::reducer]
pub fn create_folder(ctx: &ReducerContext, path: String, name: String, depth: u32) {
    if ctx.db.folder().path().find(&path).is_some() {
        log::warn!("Folder already exists: {}", path);
        return;
    }

    ctx.db.folder().insert(Folder {
        path: path.clone(),
        name,
        depth,
    });
    log::info!("Created folder: {}", path);
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
    if let Some(_existing) = ctx.db.folder().path().find(&old_path) {
        let new_name = new_path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&new_path)
            .to_string();

        let new_depth = new_path.matches('/').count() as u32;

        ctx.db.folder().path().delete(&old_path);
        ctx.db.folder().insert(Folder {
            path: new_path.clone(),
            name: new_name,
            depth: new_depth,
        });
        log::info!("Moved folder: {} -> {}", old_path, new_path);
    } else {
        log::warn!("Folder not found for move: {}", old_path);
    }
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
