use spacetimedb::{ReducerContext, Table};

use crate::space_channel_tables::{
    message, message_image, permission_request, session, session_activity, tool_event, Message,
    MessageImage, PermissionRequest, Session, SessionActivity, ToolEvent,
};

const MESSAGE_TTL_MICROS: i64 = 48 * 60 * 60 * 1_000_000;
const IMAGE_MAX_BYTES: usize = 4 * 1024 * 1024;

fn ensure_session_exists(ctx: &ReducerContext, session_id: &str) {
    if ctx.db.session().id().find(&session_id.to_string()).is_some() {
        return;
    }
    let now = ctx.timestamp;
    let (base_name, host) = match session_id.find('@') {
        Some(idx) => (
            session_id[..idx].to_string(),
            session_id[idx + 1..].to_string(),
        ),
        None => (session_id.to_string(), String::new()),
    };
    ctx.db.session().insert(Session {
        id: session_id.to_string(),
        base_name,
        host,
        client_id: String::new(),
        created_at: now,
        last_seen: now,
        context_used: 0,
        context_window: 0,
    });
    log::info!("Auto-recreated session row from activity: {}", session_id);
}

#[spacetimedb::reducer]
pub fn register_session(
    ctx: &ReducerContext,
    id: String,
    base_name: String,
    host: String,
    client_id: String,
) {
    let now = ctx.timestamp;

    if let Some(existing) = ctx.db.session().id().find(&id) {
        ctx.db.session().id().update(Session {
            id: id.clone(),
            base_name,
            host,
            client_id,
            created_at: existing.created_at,
            last_seen: now,
            context_used: existing.context_used,
            context_window: existing.context_window,
        });
        log::info!("Session re-registered: {}", id);
    } else {
        ctx.db.session().insert(Session {
            id: id.clone(),
            base_name,
            host,
            client_id,
            created_at: now,
            last_seen: now,
            context_used: 0,
            context_window: 0,
        });
        log::info!("Session registered: {}", id);
    }
}

#[spacetimedb::reducer]
pub fn heartbeat(ctx: &ReducerContext, session_id: String) {
    ensure_session_exists(ctx, &session_id);
    let Some(existing) = ctx.db.session().id().find(&session_id) else {
        return;
    };
    ctx.db.session().id().update(Session {
        last_seen: ctx.timestamp,
        ..existing
    });
}

#[spacetimedb::reducer]
pub fn end_session(ctx: &ReducerContext, session_id: String) {
    ctx.db.session().id().delete(&session_id);
    ctx.db.session_activity().session_id().delete(&session_id);
    log::info!("Session ended: {}", session_id);
}

#[spacetimedb::reducer]
pub fn push_status(ctx: &ReducerContext, session_id: String, state: String) {
    ensure_session_exists(ctx, &session_id);
    let now = ctx.timestamp;

    if let Some(existing) = ctx.db.session_activity().session_id().find(&session_id) {
        ctx.db.session_activity().session_id().update(SessionActivity {
            session_id: session_id.clone(),
            state,
            last_tool_event: existing.last_tool_event,
            updated_at: now,
        });
    } else {
        ctx.db.session_activity().insert(SessionActivity {
            session_id: session_id.clone(),
            state,
            last_tool_event: None,
            updated_at: now,
        });
    }

    if let Some(existing) = ctx.db.session().id().find(&session_id) {
        ctx.db.session().id().update(Session {
            last_seen: now,
            ..existing
        });
    }
}

#[spacetimedb::reducer]
pub fn push_context_usage(
    ctx: &ReducerContext,
    session_id: String,
    used: u64,
    window: u64,
) {
    ensure_session_exists(ctx, &session_id);
    let Some(existing) = ctx.db.session().id().find(&session_id) else {
        return;
    };
    ctx.db.session().id().update(Session {
        last_seen: ctx.timestamp,
        context_used: used,
        context_window: window,
        ..existing
    });
}

#[spacetimedb::reducer]
pub fn push_tool_event(
    ctx: &ReducerContext,
    id: String,
    session_id: String,
    tool: String,
    detail: String,
) {
    ensure_session_exists(ctx, &session_id);
    let now = ctx.timestamp;

    ctx.db.tool_event().insert(ToolEvent {
        id,
        session_id: session_id.clone(),
        tool,
        detail: detail.clone(),
        started_at: now,
    });

    if ctx.db.session_activity().session_id().find(&session_id).is_some() {
        ctx.db.session_activity().session_id().update(SessionActivity {
            session_id: session_id.clone(),
            state: "tool_use".to_string(),
            last_tool_event: Some(detail),
            updated_at: now,
        });
    } else {
        ctx.db.session_activity().insert(SessionActivity {
            session_id: session_id.clone(),
            state: "tool_use".to_string(),
            last_tool_event: Some(detail),
            updated_at: now,
        });
    }

    if let Some(existing) = ctx.db.session().id().find(&session_id) {
        ctx.db.session().id().update(Session {
            last_seen: now,
            ..existing
        });
    }
}

#[spacetimedb::reducer]
pub fn push_message(
    ctx: &ReducerContext,
    id: String,
    session_id: String,
    role: String,
    text: String,
    source: String,
) {
    ensure_session_exists(ctx, &session_id);
    let now = ctx.timestamp;

    ctx.db.message().insert(Message {
        id,
        session_id: session_id.clone(),
        role,
        text,
        source,
        created_at: now,
    });

    if let Some(existing) = ctx.db.session().id().find(&session_id) {
        ctx.db.session().id().update(Session {
            last_seen: now,
            ..existing
        });
    }
}

#[spacetimedb::reducer]
pub fn push_image(
    ctx: &ReducerContext,
    id: String,
    session_id: String,
    caption: String,
    bytes: Vec<u8>,
) -> Result<(), String> {
    if bytes.len() > IMAGE_MAX_BYTES {
        return Err(format!(
            "image too large: {} bytes (max {})",
            bytes.len(),
            IMAGE_MAX_BYTES
        ));
    }

    ensure_session_exists(ctx, &session_id);
    let now = ctx.timestamp;

    ctx.db.message().insert(Message {
        id: id.clone(),
        session_id: session_id.clone(),
        role: "user".to_string(),
        text: caption,
        source: "flutter".to_string(),
        created_at: now,
    });

    ctx.db.message_image().insert(MessageImage {
        message_id: id,
        bytes,
    });

    if let Some(existing) = ctx.db.session().id().find(&session_id) {
        ctx.db.session().id().update(Session {
            last_seen: now,
            ..existing
        });
    }

    Ok(())
}

#[spacetimedb::reducer]
pub fn edit_message(ctx: &ReducerContext, id: String, text: String) {
    let Some(existing) = ctx.db.message().id().find(&id) else {
        log::warn!("edit_message: not found: {}", id);
        return;
    };
    ctx.db.message().id().update(Message { text, ..existing });
}

#[spacetimedb::reducer]
pub fn request_permission(
    ctx: &ReducerContext,
    id: String,
    session_id: String,
    tool: String,
    input: String,
) {
    ctx.db.permission_request().insert(PermissionRequest {
        id,
        session_id,
        tool,
        input,
        status: "pending".to_string(),
        created_at: ctx.timestamp,
        resolved_at: None,
    });
}

#[spacetimedb::reducer]
pub fn resolve_permission(ctx: &ReducerContext, id: String, status: String) {
    let Some(existing) = ctx.db.permission_request().id().find(&id) else {
        log::warn!("resolve_permission: not found: {}", id);
        return;
    };
    ctx.db.permission_request().id().update(PermissionRequest {
        status,
        resolved_at: Some(ctx.timestamp),
        ..existing
    });
}

#[spacetimedb::reducer]
pub fn sweep_old_messages(ctx: &ReducerContext) {
    let cutoff_micros = ctx
        .timestamp
        .to_micros_since_unix_epoch()
        .saturating_sub(MESSAGE_TTL_MICROS);

    let old_message_ids: Vec<String> = ctx
        .db
        .message()
        .iter()
        .filter(|m| m.created_at.to_micros_since_unix_epoch() < cutoff_micros)
        .map(|m| m.id.clone())
        .collect();

    for id in &old_message_ids {
        ctx.db.message().id().delete(id);
        ctx.db.message_image().message_id().delete(id);
    }

    let old_tool_event_ids: Vec<String> = ctx
        .db
        .tool_event()
        .iter()
        .filter(|t| t.started_at.to_micros_since_unix_epoch() < cutoff_micros)
        .map(|t| t.id.clone())
        .collect();

    for id in &old_tool_event_ids {
        ctx.db.tool_event().id().delete(id);
    }

    let resolved_permission_ids: Vec<String> = ctx
        .db
        .permission_request()
        .iter()
        .filter(|p| {
            p.status != "pending"
                && p.resolved_at
                    .map(|t: spacetimedb::Timestamp| {
                        t.to_micros_since_unix_epoch() < cutoff_micros
                    })
                    .unwrap_or(false)
        })
        .map(|p| p.id.clone())
        .collect();

    for id in &resolved_permission_ids {
        ctx.db.permission_request().id().delete(id);
    }

    log::info!(
        "sweep_old_messages: purged {} messages, {} tool_events, {} permissions",
        old_message_ids.len(),
        old_tool_event_ids.len(),
        resolved_permission_ids.len()
    );
}

#[spacetimedb::reducer]
pub fn delete_session(ctx: &ReducerContext, session_id: String) {
    let message_ids: Vec<String> = ctx
        .db
        .message()
        .iter()
        .filter(|m| m.session_id == session_id)
        .map(|m| m.id.clone())
        .collect();

    for id in &message_ids {
        ctx.db.message().id().delete(id);
        ctx.db.message_image().message_id().delete(id);
    }

    let tool_event_ids: Vec<String> = ctx
        .db
        .tool_event()
        .iter()
        .filter(|t| t.session_id == session_id)
        .map(|t| t.id.clone())
        .collect();

    for id in &tool_event_ids {
        ctx.db.tool_event().id().delete(id);
    }

    let permission_ids: Vec<String> = ctx
        .db
        .permission_request()
        .iter()
        .filter(|p| p.session_id == session_id)
        .map(|p| p.id.clone())
        .collect();

    for id in &permission_ids {
        ctx.db.permission_request().id().delete(id);
    }

    ctx.db.session_activity().session_id().delete(&session_id);
    ctx.db.session().id().delete(&session_id);

    log::info!(
        "delete_session({}): purged {} messages, {} tool_events, {} permissions",
        session_id,
        message_ids.len(),
        tool_event_ids.len(),
        permission_ids.len()
    );
}

#[spacetimedb::reducer]
pub fn clear_all_sessions(ctx: &ReducerContext) {
    let message_ids: Vec<String> = ctx.db.message().iter().map(|m| m.id.clone()).collect();
    for id in &message_ids {
        ctx.db.message().id().delete(id);
        ctx.db.message_image().message_id().delete(id);
    }

    let tool_event_ids: Vec<String> = ctx.db.tool_event().iter().map(|t| t.id.clone()).collect();
    for id in &tool_event_ids {
        ctx.db.tool_event().id().delete(id);
    }

    let permission_ids: Vec<String> = ctx
        .db
        .permission_request()
        .iter()
        .map(|p| p.id.clone())
        .collect();
    for id in &permission_ids {
        ctx.db.permission_request().id().delete(id);
    }

    let activity_ids: Vec<String> = ctx
        .db
        .session_activity()
        .iter()
        .map(|a| a.session_id.clone())
        .collect();
    for id in &activity_ids {
        ctx.db.session_activity().session_id().delete(id);
    }

    let session_ids: Vec<String> = ctx.db.session().iter().map(|s| s.id.clone()).collect();
    for id in &session_ids {
        ctx.db.session().id().delete(id);
    }

    log::info!(
        "clear_all_sessions: purged {} sessions, {} activities, {} messages, {} tool_events, {} permissions",
        session_ids.len(),
        activity_ids.len(),
        message_ids.len(),
        tool_event_ids.len(),
        permission_ids.len()
    );
}
