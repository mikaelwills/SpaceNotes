use spacetimedb::{ReducerContext, Table, Timestamp};

mod note_reducers;
mod folder_reducers;

// =============================================================================
// Tables
// =============================================================================

#[spacetimedb::table(name = note, public)]
pub struct Note {
    #[primary_key]
    pub id: String,          // UUID (e.g., "550e8400-e29b...")
    #[unique]
    pub path: String,        // "Projects/my-note.md"
    pub name: String,        // "my-note"
    pub content: String,
    pub folder_path: String, // "Projects/"
    pub depth: u32,
    pub frontmatter: String, // JSON-serialized Map
    pub size: u64,
    pub created_time: u64,   // ms since epoch (filesystem)
    pub modified_time: u64,  // ms since epoch (filesystem)
    #[index(btree)]
    pub db_updated_at: Timestamp, // SpacetimeDB transaction time
}

#[spacetimedb::table(name = folder, public)]
pub struct Folder {
    #[primary_key]
    pub path: String,
    pub name: String,
    pub depth: u32,
}

// =============================================================================
// Lifecycle Reducers
// =============================================================================

#[spacetimedb::reducer(init)]
pub fn init(_ctx: &ReducerContext) {
    log::info!("SpaceNotes module initialized");
}

#[spacetimedb::reducer(client_connected)]
pub fn identity_connected(_ctx: &ReducerContext) {
    log::info!("Client connected");
}

#[spacetimedb::reducer(client_disconnected)]
pub fn identity_disconnected(_ctx: &ReducerContext) {
    log::info!("Client disconnected");
}


#[spacetimedb::reducer]
#[allow(clippy::too_many_arguments)]
pub fn clear_all(ctx: &ReducerContext) {
    // Clear all notes
    let note_ids: Vec<String> = ctx.db.note().iter().map(|n| n.id.clone()).collect();
    for id in note_ids {
        ctx.db.note().id().delete(&id);
    }

    // Clear all folders
    let folder_paths: Vec<String> = ctx.db.folder().iter().map(|f| f.path.clone()).collect();
    for path in folder_paths {
        ctx.db.folder().path().delete(&path);
    }

    log::info!("Cleared all notes and folders");
}

// =============================================================================
// Queries (Reducers that return data without side effects)
// =============================================================================

/// Get the most recently updated notes in the database
///
/// This is implemented as a reducer (not a view) so it can accept parameters.
/// It has no side effects - it only queries and returns data.
///
/// # Arguments
/// * `limit` - Number of recent notes to return (e.g., 5, 10, 20)
///
/// # Returns
/// JSON array of the most recent notes via log output
#[spacetimedb::reducer]
pub fn get_recent_notes(ctx: &ReducerContext, limit: u32) {
    let mut notes: Vec<Note> = ctx.db.note().iter().collect();

    // Sort by db_updated_at descending (newest first)
    notes.sort_by(|a, b| b.db_updated_at.cmp(&a.db_updated_at));

    // Take only the requested limit
    notes.truncate(limit as usize);

    // Return results via log
    for note in notes {
        log::info!("Recent note: {} (updated: {:?})", note.path, note.db_updated_at);
    }
}
