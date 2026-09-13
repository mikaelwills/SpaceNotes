use spacetimedb::{ReducerContext, Table};

use crate::{FileContent, SpaceFile, file_content, space_file};

/// A file's body, or empty if it has none.
///
/// Empty covers two different things and callers do not need to tell them
/// apart: a large binary that never had a row, and a file whose body is
/// genuinely empty.
pub fn content_of(ctx: &ReducerContext, id: &str) -> String {
    ctx.db
        .file_content()
        .file_id()
        .find(&id.to_string())
        .map(|row| row.content)
        .unwrap_or_default()
}

/// Writes a file's body, replacing any existing one.
fn put_content(ctx: &ReducerContext, id: &str, content: String) {
    ctx.db.file_content().file_id().delete(&id.to_string());
    ctx.db.file_content().insert(FileContent {
        file_id: id.to_string(),
        content,
    });
}

fn drop_content(ctx: &ReducerContext, id: &str) {
    ctx.db.file_content().file_id().delete(&id.to_string());
}

/// Replaces a file's metadata row, stamping the transaction time.
///
/// Every reducer here previously rebuilt this struct by hand, which is why a
/// field added to `SpaceFile` meant editing eleven places. Content is absent
/// on purpose: a rename or move does not touch the body at all now.
fn replace_metadata(ctx: &ReducerContext, file: SpaceFile) {
    ctx.db.space_file().id().delete(&file.id);
    ctx.db.space_file().insert(SpaceFile {
        db_updated_at: ctx.timestamp,
        ..file
    });
}

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

fn require_non_root_path(path: &str) -> Result<(), String> {
    if !path.contains('/') {
        return Err(format!(
            "Cannot place a file at the vault root: '{}' must live inside a folder",
            path
        ));
    }
    Ok(())
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
    require_non_root_path(&path)?;

    // Check if file already exists by ID
    if ctx.db.space_file().id().find(&id).is_some() {
        return Err(format!("File already exists with ID: {}", id));
    }

    // Check if path already exists (unique constraint)
    if ctx.db.space_file().path().find(&path).is_some() {
        return Err(format!("File already exists with path: {}", path));
    }

    ctx.db.space_file().insert(SpaceFile {
        id: id.clone(),
        path: path.clone(),
        name,
        folder_path,
        depth,
        extension,
        size,
        created_time,
        modified_time,
        db_updated_at: ctx.timestamp,
        has_thumbnail: false,
    });
    put_content(ctx, &id, content);
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
    let Some(existing) = ctx.db.space_file().id().find(&id) else {
        return Err(format!("File not found for content update: {}", id));
    };

    let path = existing.path.clone();
    replace_metadata(
        ctx,
        SpaceFile {
            size,
            modified_time,
            ..existing
        },
    );
    put_content(ctx, &id, content);

    log::info!("Updated content for file: {} (ID: {})", path, id);
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

        let old_path = existing.path.clone();
        replace_metadata(
            ctx,
            SpaceFile {
                path: new_path.clone(),
                name: new_name,
                folder_path: new_folder_path,
                depth: new_depth,
                extension: new_extension,
                ..existing
            },
        );
        log::info!("Renamed file: {} -> {} (ID: {})", old_path, new_path, id);
    } else {
        return Err(format!("File not found for rename: {}", id));
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn delete_file(ctx: &ReducerContext, id: String) -> Result<(), String> {
    if ctx.db.space_file().id().find(&id).is_some() {
        ctx.db.space_file().id().delete(&id);
        // Cascade: an orphaned content row is invisible and would accumulate.
        drop_content(ctx, &id);
        log::info!("Deleted file with ID: {}", id);
    } else {
        return Err(format!("File not found for deletion: {}", id));
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn update_file_path(ctx: &ReducerContext, id: String, new_path: String) -> Result<(), String> {
    require_non_root_path(&new_path)?;

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

        replace_metadata(
            ctx,
            SpaceFile {
                path: new_path.clone(),
                name: new_name,
                folder_path: new_folder_path,
                depth: new_depth,
                extension: new_extension,
                ..existing
            },
        );
        log::info!("Updated path for file {}: {}", id, new_path);
    } else {
        return Err(format!("File not found for path update: {}", id));
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn move_file(ctx: &ReducerContext, old_path: String, new_path: String) -> Result<(), String> {
    require_non_root_path(&new_path)?;

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

        replace_metadata(
            ctx,
            SpaceFile {
                path: new_path.clone(),
                name: new_name,
                folder_path: new_folder_path,
                depth: new_depth,
                extension: new_extension,
                ..existing
            },
        );
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
    require_non_root_path(&path)?;

    let has_thumbnail = if let Some(existing) = ctx.db.space_file().id().find(&id) {
        // The unchanged check still compares content, so an ingest that finds
        // nothing new writes nothing — the property that keeps the commitlog
        // from growing on every scan.
        if existing.path == path
            && existing.folder_path == folder_path
            && existing.depth == depth
            && existing.size == size
            && existing.modified_time == modified_time
            && content_of(ctx, &id) == content
        {
            return Ok(());
        }
        ctx.db.space_file().id().delete(&id);
        existing.has_thumbnail
    } else {
        false
    };

    // Both tables in one reducer call, so there is never a moment where a
    // client can see a file row whose content has not arrived.
    ctx.db.space_file().insert(SpaceFile {
        id: id.clone(),
        path,
        name,
        folder_path,
        depth,
        extension,
        size,
        created_time,
        modified_time,
        db_updated_at: ctx.timestamp,
        has_thumbnail,
    });
    put_content(ctx, &id, content);
    Ok(())
}

/// Append content to an existing file (by path)
#[spacetimedb::reducer]
pub fn append_to_file(ctx: &ReducerContext, path: String, content: String) -> Result<(), String> {
    if let Some(existing) = ctx.db.space_file().path().find(&path) {
        let id = existing.id.clone();
        let new_content = format!("{}{}", content_of(ctx, &id), content);
        let new_size = new_content.len() as u64;
        let now = ctx.timestamp.to_micros_since_unix_epoch() as u64 / 1_000;

        replace_metadata(
            ctx,
            SpaceFile {
                size: new_size,
                modified_time: now,
                ..existing
            },
        );
        put_content(ctx, &id, new_content);
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
        let id = existing.id.clone();
        let new_content = format!("{}{}", content, content_of(ctx, &id));
        let new_size = new_content.len() as u64;
        let now = ctx.timestamp.to_micros_since_unix_epoch() as u64 / 1_000;

        replace_metadata(
            ctx,
            SpaceFile {
                size: new_size,
                modified_time: now,
                ..existing
            },
        );
        put_content(ctx, &id, new_content);
        log::info!("Prepended {} bytes to file: {}", content.len(), path);
    } else {
        return Err(format!("File not found for prepend: {}", path));
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn set_thumbnail_available(ctx: &ReducerContext, id: String) -> Result<(), String> {
    if let Some(existing) = ctx.db.space_file().id().find(&id) {
        let path = existing.path.clone();
        // Keeps the existing db_updated_at rather than stamping a new one, so
        // a thumbnail arriving does not read as a content change. That is why
        // this cannot go through replace_metadata.
        ctx.db.space_file().id().delete(&id);
        ctx.db.space_file().insert(SpaceFile {
            has_thumbnail: true,
            ..existing
        });
        log::info!("Marked thumbnail available for file: {} (ID: {})", path, id);
    } else {
        return Err(format!("File not found for thumbnail update: {}", id));
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
        let id = existing.id.clone();
        let current = content_of(ctx, &id);
        let new_content = if replace_all {
            current.replace(&old_text, &new_text)
        } else {
            current.replacen(&old_text, &new_text, 1)
        };

        // Check if anything changed
        if new_content == current {
            return Err(format!("No match found for replacement in file: {}", path));
        }

        let new_size = new_content.len() as u64;
        let now = ctx.timestamp.to_micros_since_unix_epoch() as u64 / 1_000;

        replace_metadata(
            ctx,
            SpaceFile {
                size: new_size,
                modified_time: now,
                ..existing
            },
        );
        put_content(ctx, &id, new_content);
        log::info!("REDUCER_EXECUTED: find_replace_in_file path={}, new_size={}", path, new_size);
    } else {
        return Err(format!("File not found for find/replace: {}", path));
    }
    Ok(())
}
