use spacetimedb::{ReducerContext, Table};

use crate::{SpaceFile, space_file};

pub fn extension_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_lowercase(),
        _ => String::new(),
    }
}

fn name_from_path(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    match base.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => base.to_string(),
    }
}

fn folder_path_of(path: &str) -> String {
    match path.rfind('/') {
        Some(idx) => format!("{}/", &path[..idx]),
        None => String::new(),
    }
}

// =============================================================================
// File Reducers
// =============================================================================

#[spacetimedb::reducer]
pub fn create_file(
    ctx: &ReducerContext,
    id: String,
    path: String,
    name: String,
    content: String,
    folder_path: String,
    depth: u32,
    extension: String,
    size: u64,
    created_time: u64,
    modified_time: u64,
) -> Result<(), String> {
    // Check if file already exists by ID
    if ctx.db.space_file().id().find(&id).is_some() {
        return Err(format!("File already exists with ID: {}", id));
    }

    // Check if path already exists (unique constraint)
    if ctx.db.space_file().path().find(&path).is_some() {
        return Err(format!("File already exists with path: {}", path));
    }

    ctx.db.space_file().insert(SpaceFile {
        id,
        path: path.clone(),
        name,
        content,
        folder_path,
        depth,
        extension,
        size,
        created_time,
        modified_time,
        db_updated_at: ctx.timestamp,
    });
    log::info!("Created file: {}", path);
    Ok(())
}

/// Update only the content of a file (path stays the same)
#[spacetimedb::reducer]
pub fn update_file_content(
    ctx: &ReducerContext,
    id: String,
    content: String,
    size: u64,
    modified_time: u64,
) -> Result<(), String> {
    if let Some(existing) = ctx.db.space_file().id().find(&id) {
        // Only update content-related fields, path remains unchanged
        ctx.db.space_file().id().delete(&id);
        ctx.db.space_file().insert(SpaceFile {
            id: id.clone(),
            path: existing.path.clone(),
            name: existing.name.clone(),
            content,
            folder_path: existing.folder_path.clone(),
            depth: existing.depth,
            extension: existing.extension.clone(),
            size,
            created_time: existing.created_time,
            modified_time,
            db_updated_at: ctx.timestamp,
        });
        log::info!("Updated content for file: {} (ID: {})", existing.path, id);
    } else {
        return Err(format!("File not found for content update: {}", id));
    }
    Ok(())
}

/// Rename/move a file (path changes, content stays the same)
#[spacetimedb::reducer]
pub fn rename_file(
    ctx: &ReducerContext,
    id: String,
    new_path: String,
) -> Result<(), String> {
    if let Some(existing) = ctx.db.space_file().id().find(&id) {
        // Check if new path already exists
        if let Some(collision) = ctx.db.space_file().path().find(&new_path) {
            if collision.id != id {
                return Err(format!("Cannot rename: path '{}' already exists", new_path));
            }
        }

        // Calculate new metadata from new path
        let new_name = name_from_path(&new_path);
        let new_folder_path = folder_path_of(&new_path);
        let new_depth = new_path.matches('/').count() as u32;
        let new_extension = extension_of(&new_path);

        ctx.db.space_file().id().delete(&id);
        ctx.db.space_file().insert(SpaceFile {
            id: id.clone(),
            path: new_path.clone(),
            name: new_name,
            content: existing.content,
            folder_path: new_folder_path,
            depth: new_depth,
            extension: new_extension,
            size: existing.size,
            created_time: existing.created_time,
            modified_time: existing.modified_time,
            db_updated_at: ctx.timestamp,
        });
        log::info!("Renamed file: {} -> {} (ID: {})", existing.path, new_path, id);
    } else {
        return Err(format!("File not found for rename: {}", id));
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn delete_file(ctx: &ReducerContext, id: String) -> Result<(), String> {
    if ctx.db.space_file().id().find(&id).is_some() {
        ctx.db.space_file().id().delete(&id);
        log::info!("Deleted file with ID: {}", id);
    } else {
        return Err(format!("File not found for deletion: {}", id));
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn update_file_path(ctx: &ReducerContext, id: String, new_path: String) -> Result<(), String> {
    if let Some(existing) = ctx.db.space_file().id().find(&id) {
        if let Some(collision) = ctx.db.space_file().path().find(&new_path) {
            if collision.id != id {
                return Err(format!("Cannot move: path '{}' already exists", new_path));
            }
        }

        let new_name = name_from_path(&new_path);
        let new_folder_path = folder_path_of(&new_path);
        let new_depth = new_path.matches('/').count() as u32;
        let new_extension = extension_of(&new_path);

        ctx.db.space_file().id().delete(&id);
        ctx.db.space_file().insert(SpaceFile {
            id: id.clone(),
            path: new_path.clone(),
            name: new_name,
            content: existing.content,
            folder_path: new_folder_path,
            depth: new_depth,
            extension: new_extension,
            size: existing.size,
            created_time: existing.created_time,
            modified_time: existing.modified_time,
            db_updated_at: ctx.timestamp,
        });
        log::info!("Updated path for file {}: {}", id, new_path);
    } else {
        return Err(format!("File not found for path update: {}", id));
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn move_file(ctx: &ReducerContext, old_path: String, new_path: String) -> Result<(), String> {
    if let Some(existing) = ctx.db.space_file().path().find(&old_path) {
        // Without this the delete+insert below violates the unique path constraint and panics.
        if let Some(collision) = ctx.db.space_file().path().find(&new_path) {
            if collision.id != existing.id {
                return Err(format!("Cannot move: path '{}' already exists", new_path));
            }
        }

        let new_name = name_from_path(&new_path);
        let new_folder_path = folder_path_of(&new_path);
        let new_depth = new_path.matches('/').count() as u32;
        let new_extension = extension_of(&new_path);

        let id = existing.id.clone();
        ctx.db.space_file().id().delete(&id);
        ctx.db.space_file().insert(SpaceFile {
            id,
            path: new_path.clone(),
            name: new_name,
            content: existing.content,
            folder_path: new_folder_path,
            depth: new_depth,
            extension: new_extension,
            size: existing.size,
            created_time: existing.created_time,
            modified_time: existing.modified_time,
            db_updated_at: ctx.timestamp,
        });
        log::info!("Moved file: {} -> {}", old_path, new_path);
    } else {
        return Err(format!("File not found for move: {}", old_path));
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn upsert_file(
    ctx: &ReducerContext,
    id: String,
    path: String,
    name: String,
    content: String,
    folder_path: String,
    depth: u32,
    extension: String,
    size: u64,
    created_time: u64,
    modified_time: u64,
) -> Result<(), String> {
    if let Some(existing) = ctx.db.space_file().id().find(&id) {
        if existing.path == path
            && existing.content == content
            && existing.folder_path == folder_path
            && existing.depth == depth
            && existing.size == size
            && existing.modified_time == modified_time
        {
            return Ok(());
        }
        ctx.db.space_file().id().delete(&id);
    }
    ctx.db.space_file().insert(SpaceFile {
        id,
        path,
        name,
        content,
        folder_path,
        depth,
        extension,
        size,
        created_time,
        modified_time,
        db_updated_at: ctx.timestamp,
    });
    Ok(())
}

/// Append content to an existing file (by path)
#[spacetimedb::reducer]
pub fn append_to_file(ctx: &ReducerContext, path: String, content: String) -> Result<(), String> {
    if let Some(existing) = ctx.db.space_file().path().find(&path) {
        let new_content = format!("{}{}", existing.content, content);
        let new_size = new_content.len() as u64;
        let now = ctx.timestamp.to_micros_since_unix_epoch() as u64 / 1_000;

        ctx.db.space_file().id().delete(&existing.id);
        ctx.db.space_file().insert(SpaceFile {
            id: existing.id.clone(),
            path: existing.path,
            name: existing.name,
            content: new_content,
            folder_path: existing.folder_path,
            depth: existing.depth,
            extension: existing.extension,
            size: new_size,
            created_time: existing.created_time,
            modified_time: now,
            db_updated_at: ctx.timestamp,
        });
        log::info!("Appended {} bytes to file: {}", content.len(), path);
    } else {
        return Err(format!("File not found for append: {}", path));
    }
    Ok(())
}

/// Prepend content to an existing file (by path)
#[spacetimedb::reducer]
pub fn prepend_to_file(ctx: &ReducerContext, path: String, content: String) -> Result<(), String> {
    if let Some(existing) = ctx.db.space_file().path().find(&path) {
        let new_content = format!("{}{}", content, existing.content);
        let new_size = new_content.len() as u64;
        let now = ctx.timestamp.to_micros_since_unix_epoch() as u64 / 1_000;

        ctx.db.space_file().id().delete(&existing.id);
        ctx.db.space_file().insert(SpaceFile {
            id: existing.id.clone(),
            path: existing.path,
            name: existing.name,
            content: new_content,
            folder_path: existing.folder_path,
            depth: existing.depth,
            extension: existing.extension,
            size: new_size,
            created_time: existing.created_time,
            modified_time: now,
            db_updated_at: ctx.timestamp,
        });
        log::info!("Prepended {} bytes to file: {}", content.len(), path);
    } else {
        return Err(format!("File not found for prepend: {}", path));
    }
    Ok(())
}

/// Find and replace text in a file (by path)
#[spacetimedb::reducer]
pub fn find_replace_in_file(
    ctx: &ReducerContext,
    path: String,
    old_text: String,
    new_text: String,
    replace_all: bool,
) -> Result<(), String> {
    if let Some(existing) = ctx.db.space_file().path().find(&path) {
        let new_content = if replace_all {
            existing.content.replace(&old_text, &new_text)
        } else {
            existing.content.replacen(&old_text, &new_text, 1)
        };

        // Check if anything changed
        if new_content == existing.content {
            return Err(format!("No match found for replacement in file: {}", path));
        }

        let new_size = new_content.len() as u64;
        let now = ctx.timestamp.to_micros_since_unix_epoch() as u64 / 1_000;

        ctx.db.space_file().id().delete(&existing.id);
        ctx.db.space_file().insert(SpaceFile {
            id: existing.id.clone(),
            path: existing.path,
            name: existing.name,
            content: new_content,
            folder_path: existing.folder_path,
            depth: existing.depth,
            extension: existing.extension,
            size: new_size,
            created_time: existing.created_time,
            modified_time: now,
            db_updated_at: ctx.timestamp,
        });
        log::info!("REDUCER_EXECUTED: find_replace_in_file path={}, new_size={}", path, new_size);
    } else {
        return Err(format!("File not found for find/replace: {}", path));
    }
    Ok(())
}
