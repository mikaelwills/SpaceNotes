use spacetimedb::{CaseConversionPolicy, ReducerContext, Table, TimeDuration, Timestamp};

#[spacetimedb::settings]
const CASE_CONVERSION_POLICY: CaseConversionPolicy = CaseConversionPolicy::None;

mod call_reducers;
mod file_reducers;
mod folder_reducers;
mod space_channel_reducers;
mod space_channel_tables;

// =============================================================================
// Tables
// =============================================================================

#[spacetimedb::table(accessor = space_file, public)]
pub struct SpaceFile {
    #[primary_key]
    pub id: String, // UUID (e.g., "550e8400-e29b...")
    #[unique]
    pub path: String, // "Projects/my-note.md"
    pub name: String, // "my-note"
    pub content: String,
    pub folder_path: String, // "Projects/"
    pub depth: u32,
    pub extension: String,
    pub size: u64,
    pub created_time: u64,  // ms since epoch (filesystem)
    pub modified_time: u64, // ms since epoch (filesystem)
    #[index(btree)]
    pub db_updated_at: Timestamp, // SpacetimeDB transaction time
}

#[spacetimedb::table(accessor = folder, public)]
pub struct Folder {
    #[primary_key]
    pub path: String,
    pub name: String,
    pub depth: u32,
}

#[spacetimedb::table(accessor = connected_user, public)]
pub struct ConnectedUser {
    #[primary_key]
    pub connection_id: String,
    #[index(btree)]
    pub identity: spacetimedb::Identity,
    pub connected_at: u64,
    pub name: String,
}

#[spacetimedb::table(accessor = user_profile, public)]
pub struct UserProfile {
    #[primary_key]
    pub identity: spacetimedb::Identity,
    pub name: String,
}

// =============================================================================
// Lifecycle Reducers
// =============================================================================

#[spacetimedb::reducer(init)]
pub fn init(ctx: &ReducerContext) {
    use space_channel_tables::{sweep_schedule, SweepSchedule};

    if ctx.db.sweep_schedule().iter().next().is_none() {
        ctx.db.sweep_schedule().insert(SweepSchedule {
            scheduled_id: 0,
            scheduled_at: TimeDuration::from_micros(60 * 60 * 1_000_000).into(),
        });
    }

    {
        use space_channel_tables::{channel_config, ChannelConfig};
        if ctx.db.channel_config().id().find(0).is_none() {
            ctx.db.channel_config().insert(ChannelConfig {
                id: 0,
                a2a_enabled: true,
            });
        }
    }

    log::info!("SpaceNotes module initialized");
}

#[spacetimedb::reducer(client_connected)]
pub fn identity_connected(ctx: &ReducerContext) {
    let Some(connection_id) = ctx.connection_id().map(|c| c.to_hex().to_string()) else {
        log::warn!("client_connected: no connection_id");
        return;
    };
    let saved_name = ctx.db.user_profile().identity().find(&ctx.sender())
        .map(|p| p.name)
        .unwrap_or_default();
    ctx.db.connected_user().connection_id().delete(&connection_id);
    ctx.db.connected_user().insert(ConnectedUser {
        connection_id: connection_id.clone(),
        identity: ctx.sender(),
        connected_at: ctx.timestamp.to_duration_since_unix_epoch().unwrap_or_default().as_millis() as u64,
        name: saved_name,
    });
    log::info!("Client connected: {:?} conn {}", ctx.sender(), connection_id);
}

#[spacetimedb::reducer(client_disconnected)]
pub fn identity_disconnected(ctx: &ReducerContext) {
    use call_reducers::{call_session, CallSession, CallState};

    if let Some(connection_id) = ctx.connection_id().map(|c| c.to_hex().to_string()) {
        ctx.db.connected_user().connection_id().delete(&connection_id);
    }

    if ctx.db.connected_user().identity().filter(&ctx.sender()).next().is_some() {
        log::info!(
            "Client disconnected: {:?} still has another live connection, keeping calls",
            ctx.sender()
        );
        return;
    }

    for call in ctx.db.call_session().iter() {
        let dominated = call.caller == ctx.sender()
            || call.callee == ctx.sender();
        let active = call.state != CallState::Ended;

        if dominated && active {
            ctx.db.call_session().call_id().update(CallSession {
                state: CallState::Ended,
                ..call
            });
        }
    }
    log::info!("Client fully disconnected: {:?}", ctx.sender());
}

#[spacetimedb::reducer]
pub fn set_display_name(ctx: &ReducerContext, name: String) {
    let sessions: Vec<ConnectedUser> =
        ctx.db.connected_user().identity().filter(&ctx.sender()).collect();
    if sessions.is_empty() {
        log::warn!("set_display_name: user not found");
        return;
    }
    for session in sessions {
        ctx.db.connected_user().connection_id().update(ConnectedUser {
            name: name.clone(),
            ..session
        });
    }
    if ctx.db.user_profile().identity().find(&ctx.sender()).is_some() {
        ctx.db.user_profile().identity().update(UserProfile {
            identity: ctx.sender(),
            name,
        });
    } else {
        ctx.db.user_profile().insert(UserProfile {
            identity: ctx.sender(),
            name,
        });
    }
}

#[spacetimedb::reducer]
#[allow(clippy::too_many_arguments)]
pub fn clear_all(ctx: &ReducerContext) {
    // Clear all files
    let file_ids: Vec<String> = ctx.db.space_file().iter().map(|f| f.id.clone()).collect();
    for id in file_ids {
        ctx.db.space_file().id().delete(&id);
    }

    // Clear all folders
    let folder_paths: Vec<String> = ctx.db.folder().iter().map(|f| f.path.clone()).collect();
    for path in folder_paths {
        ctx.db.folder().path().delete(&path);
    }

    log::info!("Cleared all files and folders");
}

// =============================================================================
// Queries (Reducers that return data without side effects)
// =============================================================================

/// Get the most recently updated files in the database
///
/// This is implemented as a reducer (not a view) so it can accept parameters.
/// It has no side effects - it only queries and returns data.
///
/// # Arguments
/// * `limit` - Number of recent files to return (e.g., 5, 10, 20)
///
/// # Returns
/// JSON array of the most recent files via log output
#[spacetimedb::reducer]
pub fn get_recent_files(ctx: &ReducerContext, limit: u32) {
    let mut files: Vec<SpaceFile> = ctx.db.space_file().iter().collect();

    // Sort by db_updated_at descending (newest first)
    files.sort_by(|a, b| b.db_updated_at.cmp(&a.db_updated_at));

    // Take only the requested limit
    files.truncate(limit as usize);

    // Return results via log
    for file in files {
        log::info!(
            "Recent file: {} (updated: {:?})",
            file.path,
            file.db_updated_at
        );
    }
}
