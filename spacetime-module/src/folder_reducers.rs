use spacetimedb::{ReducerContext, Table};

use crate::{Folder, folder, space_file};

// =============================================================================
// Folder Reducers
// =============================================================================

#[spacetimedb::reducer]
pub fn create_folder(ctx: &ReducerContext, path: String, name: String, depth: u32) -> Result<(), String> {
    // Normalize: strip trailing slash to match storage standard
    let normalized_path = path.trim_end_matches('/').to_string();

    if ctx.db.folder().path().find(&normalized_path).is_some() {
        return Err(format!("Folder already exists: {}", normalized_path));
    }

    ctx.db.folder().insert(Folder {
        path: normalized_path.clone(),
        name,
        depth,
    });
    log::info!("Created folder: {}", normalized_path);
    Ok(())
}

#[spacetimedb::reducer]
pub fn delete_folder(ctx: &ReducerContext, path: String) -> Result<(), String> {
    // Normalize: strip trailing slash to match storage standard
    let normalized_path = path.trim_end_matches('/').to_string();

    if ctx.db.folder().path().find(&normalized_path).is_none() {
        return Err(format!("Folder not found for deletion: {}", normalized_path));
    }

    // For cascade operations, use path with slash to match space_file.folder_path
    let path_with_slash = format!("{}/", normalized_path);

    // CASCADE: Delete all files inside this folder (and subfolders)
    let files_to_delete: Vec<String> = ctx
        .db
        .space_file()
        .iter()
        .filter(|file| file.folder_path.starts_with(&path_with_slash))
        .map(|file| file.id.clone())
        .collect();

    for file_id in &files_to_delete {
        ctx.db.space_file().id().delete(file_id);
    }

    if !files_to_delete.is_empty() {
        log::info!("Cascade deleted {} files from folder: {}", files_to_delete.len(), normalized_path);
    }

    // CASCADE: Delete all subfolders (use normalized path for comparison)
    let subfolders_to_delete: Vec<String> = ctx
        .db
        .folder()
        .iter()
        .filter(|f| f.path.starts_with(&path_with_slash))
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
    Ok(())
}

#[spacetimedb::reducer]
pub fn move_folder(ctx: &ReducerContext, old_path: String, new_path: String) -> Result<(), String> {
    // Normalize: strip trailing slashes
    let old_normalized = old_path.trim_end_matches('/').to_string();
    let new_normalized = new_path.trim_end_matches('/').to_string();

    // Verify source folder exists
    if ctx.db.folder().path().find(&old_normalized).is_none() {
        return Err(format!("Folder not found for move: {}", old_normalized));
    }

    // Check if destination already exists
    if ctx.db.folder().path().find(&new_normalized).is_some() {
        return Err(format!("Cannot move: destination folder already exists: {}", new_normalized));
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

    // CASCADE 1: Update all files inside this folder
    let files_to_update: Vec<_> = ctx
        .db
        .space_file()
        .iter()
        .filter(|file| file.folder_path.starts_with(&old_path_with_slash))
        .collect();

    let files_count = files_to_update.len();
    for file in files_to_update {
        // Calculate new paths for the file
        let new_file_folder_path = file.folder_path.replacen(&old_path_with_slash, &new_path_with_slash, 1);
        let new_file_path = file.path.replacen(&old_path_with_slash, &new_path_with_slash, 1);
        let new_file_depth = new_file_path.matches('/').count() as u32;

        // Delete old entry and insert with updated paths
        ctx.db.space_file().id().delete(&file.id);
        ctx.db.space_file().insert(crate::SpaceFile {
            id: file.id.clone(),
            path: new_file_path,
            name: file.name,
            content: file.content,
            folder_path: new_file_folder_path,
            depth: new_file_depth,
            extension: file.extension,
            size: file.size,
            created_time: file.created_time,
            modified_time: file.modified_time,
            db_updated_at: ctx.timestamp,
        });
    }

    if files_count > 0 {
        log::info!("Cascade updated {} files in folder move", files_count);
    }

    // CASCADE 2: Update all subfolders
    let subfolders_to_update: Vec<_> = ctx
        .db
        .folder()
        .iter()
        .filter(|f| f.path.starts_with(&old_path_with_slash))
        .collect();

    let subfolders_count = subfolders_to_update.len();
    for subfolder in subfolders_to_update {
        // Calculate new path for subfolder
        let new_subfolder_path = format!(
            "{}{}",
            new_normalized,
            &subfolder.path[old_normalized.len()..]
        );
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

    log::info!("Moved folder: {} -> {} (with {} files, {} subfolders)",
               old_normalized, new_normalized, files_count, subfolders_count);
    Ok(())
}

#[spacetimedb::reducer]
pub fn upsert_folder(ctx: &ReducerContext, path: String, name: String, depth: u32) -> Result<(), String> {
    let normalized_path = path.trim_end_matches('/').to_string();

    if let Some(existing) = ctx.db.folder().path().find(&normalized_path) {
        if existing.name == name && existing.depth == depth {
            return Ok(());
        }
        ctx.db.folder().path().delete(&normalized_path);
    }
    ctx.db.folder().insert(Folder {
        path: normalized_path,
        name,
        depth
    });
    Ok(())
}
