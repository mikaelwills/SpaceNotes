use spacetimedb::{ReducerContext, Table};

use crate::{Note, note};

pub fn extension_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_lowercase(),
        _ => String::new(),
    }
}

pub fn kind_of(extension: &str) -> String {
    if extension == "md" {
        "md".to_string()
    } else {
        "file".to_string()
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
// Note Reducers
// =============================================================================

#[spacetimedb::reducer]
pub fn create_note(
    ctx: &ReducerContext,
    id: String,
    path: String,
    name: String,
    content: String,
    folder_path: String,
    depth: u32,
    extension: String,
    kind: String,
    size: u64,
    created_time: u64,
    modified_time: u64,
) {
    // Check if note already exists by ID
    if ctx.db.note().id().find(&id).is_some() {
        log::warn!("Note already exists with ID: {}", id);
        return;
    }

    // Check if path already exists (unique constraint)
    if ctx.db.note().path().find(&path).is_some() {
        log::warn!("Note already exists with path: {}", path);
        return;
    }

    ctx.db.note().insert(Note {
        id,
        path: path.clone(),
        name,
        content,
        folder_path,
        depth,
        extension,
        kind,
        size,
        created_time,
        modified_time,
        db_updated_at: ctx.timestamp,
    });
    log::info!("Created note: {}", path);
}

/// Update only the content of a note (path stays the same)
#[spacetimedb::reducer]
pub fn update_note_content(
    ctx: &ReducerContext,
    id: String,
    content: String,
    size: u64,
    modified_time: u64,
) {
    if let Some(existing) = ctx.db.note().id().find(&id) {
        // Only update content-related fields, path remains unchanged
        ctx.db.note().id().delete(&id);
        ctx.db.note().insert(Note {
            id: id.clone(),
            path: existing.path.clone(),
            name: existing.name.clone(),
            content,
            folder_path: existing.folder_path.clone(),
            depth: existing.depth,
            extension: existing.extension.clone(),
            kind: existing.kind.clone(),
            size,
            created_time: existing.created_time,
            modified_time,
            db_updated_at: ctx.timestamp,
        });
        log::info!("Updated content for note: {} (ID: {})", existing.path, id);
    } else {
        log::warn!("Note not found for content update: {}", id);
    }
}

/// Rename/move a note (path changes, content stays the same)
#[spacetimedb::reducer]
pub fn rename_note(
    ctx: &ReducerContext,
    id: String,
    new_path: String,
) {
    if let Some(existing) = ctx.db.note().id().find(&id) {
        // Check if new path already exists
        if let Some(collision) = ctx.db.note().path().find(&new_path) {
            if collision.id != id {
                log::error!("Cannot rename: Path '{}' already exists", new_path);
                return;
            }
        }

        // Calculate new metadata from new path
        let new_name = name_from_path(&new_path);
        let new_folder_path = folder_path_of(&new_path);
        let new_depth = new_path.matches('/').count() as u32;
        let new_extension = extension_of(&new_path);
        let new_kind = kind_of(&new_extension);

        ctx.db.note().id().delete(&id);
        ctx.db.note().insert(Note {
            id: id.clone(),
            path: new_path.clone(),
            name: new_name,
            content: existing.content,
            folder_path: new_folder_path,
            depth: new_depth,
            extension: new_extension,
            kind: new_kind,
            size: existing.size,
            created_time: existing.created_time,
            modified_time: existing.modified_time,
            db_updated_at: ctx.timestamp,
        });
        log::info!("Renamed note: {} -> {} (ID: {})", existing.path, new_path, id);
    } else {
        log::warn!("Note not found for rename: {}", id);
    }
}

#[spacetimedb::reducer]
pub fn delete_note(ctx: &ReducerContext, id: String) {
    if ctx.db.note().id().find(&id).is_some() {
        ctx.db.note().id().delete(&id);
        log::info!("Deleted note with ID: {}", id);
    } else {
        log::warn!("Note not found for deletion: {}", id);
    }
}

#[spacetimedb::reducer]
pub fn update_note_path(ctx: &ReducerContext, id: String, new_path: String) {
    if let Some(existing) = ctx.db.note().id().find(&id) {
        // Calculate new metadata from new path
        let new_name = name_from_path(&new_path);
        let new_folder_path = folder_path_of(&new_path);
        let new_depth = new_path.matches('/').count() as u32;
        let new_extension = extension_of(&new_path);
        let new_kind = kind_of(&new_extension);

        ctx.db.note().id().delete(&id);
        ctx.db.note().insert(Note {
            id: id.clone(),
            path: new_path.clone(),
            name: new_name,
            content: existing.content,
            folder_path: new_folder_path,
            depth: new_depth,
            extension: new_extension,
            kind: new_kind,
            size: existing.size,
            created_time: existing.created_time,
            modified_time: existing.modified_time,
            db_updated_at: ctx.timestamp,
        });
        log::info!("Updated path for note {}: {}", id, new_path);
    } else {
        log::warn!("Note not found for path update: {}", id);
    }
}

// DEPRECATED: Use update_note_path instead
// Kept for backwards compatibility during migration
#[spacetimedb::reducer]
pub fn move_note(ctx: &ReducerContext, old_path: String, new_path: String) {
    if let Some(existing) = ctx.db.note().path().find(&old_path) {
        // Calculate new metadata
        let new_name = name_from_path(&new_path);
        let new_folder_path = folder_path_of(&new_path);
        let new_depth = new_path.matches('/').count() as u32;
        let new_extension = extension_of(&new_path);
        let new_kind = kind_of(&new_extension);

        let id = existing.id.clone();
        ctx.db.note().id().delete(&id);
        ctx.db.note().insert(Note {
            id,
            path: new_path.clone(),
            name: new_name,
            content: existing.content,
            folder_path: new_folder_path,
            depth: new_depth,
            extension: new_extension,
            kind: new_kind,
            size: existing.size,
            created_time: existing.created_time,
            modified_time: existing.modified_time,
            db_updated_at: ctx.timestamp,
        });
        log::info!("Moved note: {} -> {}", old_path, new_path);
    } else {
        log::warn!("Note not found for move: {}", old_path);
    }
}

#[spacetimedb::reducer]
pub fn upsert_note(
    ctx: &ReducerContext,
    id: String,
    path: String,
    name: String,
    content: String,
    folder_path: String,
    depth: u32,
    extension: String,
    kind: String,
    size: u64,
    created_time: u64,
    modified_time: u64,
) {
    if let Some(existing) = ctx.db.note().id().find(&id) {
        if existing.path == path
            && existing.content == content
            && existing.folder_path == folder_path
            && existing.depth == depth
            && existing.size == size
            && existing.modified_time == modified_time
        {
            return;
        }
        ctx.db.note().id().delete(&id);
    }
    ctx.db.note().insert(Note {
        id,
        path,
        name,
        content,
        folder_path,
        depth,
        extension,
        kind,
        size,
        created_time,
        modified_time,
        db_updated_at: ctx.timestamp,
    });
}

/// Append content to an existing note (by path)
#[spacetimedb::reducer]
pub fn append_to_note(ctx: &ReducerContext, path: String, content: String) {
    if let Some(existing) = ctx.db.note().path().find(&path) {
        let new_content = format!("{}{}", existing.content, content);
        let new_size = new_content.len() as u64;
        let now = ctx.timestamp.to_micros_since_unix_epoch() as u64 / 1_000;

        ctx.db.note().id().delete(&existing.id);
        ctx.db.note().insert(Note {
            id: existing.id.clone(),
            path: existing.path,
            name: existing.name,
            content: new_content,
            folder_path: existing.folder_path,
            depth: existing.depth,
            extension: existing.extension,
            kind: existing.kind,
            size: new_size,
            created_time: existing.created_time,
            modified_time: now,
            db_updated_at: ctx.timestamp,
        });
        log::info!("Appended {} bytes to note: {}", content.len(), path);
    } else {
        log::warn!("Note not found for append: {}", path);
    }
}

/// Prepend content to an existing note (by path)
#[spacetimedb::reducer]
pub fn prepend_to_note(ctx: &ReducerContext, path: String, content: String) {
    if let Some(existing) = ctx.db.note().path().find(&path) {
        let new_content = format!("{}{}", content, existing.content);
        let new_size = new_content.len() as u64;
        let now = ctx.timestamp.to_micros_since_unix_epoch() as u64 / 1_000;

        ctx.db.note().id().delete(&existing.id);
        ctx.db.note().insert(Note {
            id: existing.id.clone(),
            path: existing.path,
            name: existing.name,
            content: new_content,
            folder_path: existing.folder_path,
            depth: existing.depth,
            extension: existing.extension,
            kind: existing.kind,
            size: new_size,
            created_time: existing.created_time,
            modified_time: now,
            db_updated_at: ctx.timestamp,
        });
        log::info!("Prepended {} bytes to note: {}", content.len(), path);
    } else {
        log::warn!("Note not found for prepend: {}", path);
    }
}

/// Find and replace text in a note (by path)
#[spacetimedb::reducer]
pub fn find_replace_in_note(
    ctx: &ReducerContext,
    path: String,
    old_text: String,
    new_text: String,
    replace_all: bool,
) {
    if let Some(existing) = ctx.db.note().path().find(&path) {
        let new_content = if replace_all {
            existing.content.replace(&old_text, &new_text)
        } else {
            existing.content.replacen(&old_text, &new_text, 1)
        };

        // Check if anything changed
        if new_content == existing.content {
            log::warn!("No match found for replacement in note: {}", path);
            return;
        }

        let new_size = new_content.len() as u64;
        let now = ctx.timestamp.to_micros_since_unix_epoch() as u64 / 1_000;

        ctx.db.note().id().delete(&existing.id);
        ctx.db.note().insert(Note {
            id: existing.id.clone(),
            path: existing.path,
            name: existing.name,
            content: new_content,
            folder_path: existing.folder_path,
            depth: existing.depth,
            extension: existing.extension,
            kind: existing.kind,
            size: new_size,
            created_time: existing.created_time,
            modified_time: now,
            db_updated_at: ctx.timestamp,
        });
        log::info!("REDUCER_EXECUTED: find_replace_in_note path={}, new_size={}", path, new_size);
    } else {
        log::warn!("Note not found for find/replace: {}", path);
    }
}
