use spacetimedb::{ReducerContext, Table};

use crate::{Note, note};

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
    frontmatter: String,
    size: u64,
    created_time: u64,
    modified_time: u64,
) {
    // Check if note already exists by ID
    if ctx.db.note().id().find(&id).is_some() {
        log::warn!("Note already exists with ID: {}", id);
        return;
    }

    ctx.db.note().insert(Note {
        id,
        path: path.clone(),
        name,
        content,
        folder_path,
        depth,
        frontmatter,
        size,
        created_time,
        modified_time,
    });
    log::info!("Created note: {}", path);
}

#[spacetimedb::reducer]
pub fn update_note(
    ctx: &ReducerContext,
    id: String,
    path: String,
    content: String,
    frontmatter: String,
    size: u64,
    modified_time: u64,
) {
    if let Some(existing) = ctx.db.note().id().find(&id) {
        // Calculate name, folder_path, and depth from the new path
        let name = path
            .trim_end_matches(".md")
            .rsplit('/')
            .next()
            .unwrap_or(&path)
            .to_string();

        let folder_path = if let Some(idx) = path.rfind('/') {
            format!("{}/", &path[..idx])
        } else {
            String::new()
        };

        let depth = path.matches('/').count() as u32;

        ctx.db.note().id().delete(&id);
        ctx.db.note().insert(Note {
            id: id.clone(),
            path: path.clone(),
            name,
            content,
            folder_path,
            depth,
            frontmatter,
            size,
            created_time: existing.created_time,
            modified_time,
        });
        log::info!("Updated note: {} (ID: {})", path, id);
    } else {
        log::warn!("Note not found for update: {}", id);
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
        let new_name = new_path
            .trim_end_matches(".md")
            .rsplit('/')
            .next()
            .unwrap_or(&new_path)
            .to_string();

        let new_folder_path = if let Some(idx) = new_path.rfind('/') {
            format!("{}/", &new_path[..idx])
        } else {
            String::new()
        };

        let new_depth = new_path.matches('/').count() as u32;

        ctx.db.note().id().delete(&id);
        ctx.db.note().insert(Note {
            id: id.clone(),
            path: new_path.clone(),
            name: new_name,
            content: existing.content,
            folder_path: new_folder_path,
            depth: new_depth,
            frontmatter: existing.frontmatter,
            size: existing.size,
            created_time: existing.created_time,
            modified_time: existing.modified_time,
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
        let new_name = new_path
            .trim_end_matches(".md")
            .rsplit('/')
            .next()
            .unwrap_or(&new_path)
            .to_string();

        let new_folder_path = if let Some(idx) = new_path.rfind('/') {
            format!("{}/", &new_path[..idx])
        } else {
            String::new()
        };

        let new_depth = new_path.matches('/').count() as u32;

        let id = existing.id.clone();
        ctx.db.note().id().delete(&id);
        ctx.db.note().insert(Note {
            id,
            path: new_path.clone(),
            name: new_name,
            content: existing.content,
            folder_path: new_folder_path,
            depth: new_depth,
            frontmatter: existing.frontmatter,
            size: existing.size,
            created_time: existing.created_time,
            modified_time: existing.modified_time,
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
    frontmatter: String,
    size: u64,
    created_time: u64,
    modified_time: u64,
) {
    // Delete if exists (by ID), then insert
    if ctx.db.note().id().find(&id).is_some() {
        ctx.db.note().id().delete(&id);
    }
    ctx.db.note().insert(Note {
        id,
        path,
        name,
        content,
        folder_path,
        depth,
        frontmatter,
        size,
        created_time,
        modified_time,
    });
}
