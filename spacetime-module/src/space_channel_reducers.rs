use spacetimedb::{ReducerContext, Table};

use crate::space_channel_tables::{
    message, message_image, permission_request, question_request, agent, agent_activity,
    tool_event, Message, MessageImage, PermissionRequest, QuestionRequest, Agent,
    AgentActivity, ToolEvent,
};

const MESSAGE_TTL_MICROS: i64 = 48 * 60 * 60 * 1_000_000;
const IMAGE_MAX_BYTES: usize = 4 * 1024 * 1024;

fn ensure_agent_exists(ctx: &ReducerContext, agent_id: &str) {
    if ctx.db.agent().id().find(&agent_id.to_string()).is_some() {
        return;
    }
    let now = ctx.timestamp;
    let (base_name, host) = match agent_id.find('@') {
        Some(idx) => (
            agent_id[..idx].to_string(),
            agent_id[idx + 1..].to_string(),
        ),
        None => (agent_id.to_string(), String::new()),
    };
    ctx.db.agent().insert(Agent {
        id: agent_id.to_string(),
        base_name,
        host,
        client_id: String::new(),
        created_at: now,
        last_seen: now,
        context_used: 0,
        context_window: 0,
    });
    log::info!("Auto-recreated agent row from activity: {}", agent_id);
}

#[spacetimedb::reducer]
pub fn register_agent(
    ctx: &ReducerContext,
    id: String,
    base_name: String,
    host: String,
    client_id: String,
) {
    let now = ctx.timestamp;

    if let Some(existing) = ctx.db.agent().id().find(&id) {
        ctx.db.agent().id().update(Agent {
            id: id.clone(),
            base_name,
            host,
            client_id,
            created_at: existing.created_at,
            last_seen: now,
            context_used: existing.context_used,
            context_window: existing.context_window,
        });
        log::info!("Agent re-registered: {}", id);
    } else {
        ctx.db.agent().insert(Agent {
            id: id.clone(),
            base_name,
            host,
            client_id,
            created_at: now,
            last_seen: now,
            context_used: 0,
            context_window: 0,
        });
        log::info!("Agent registered: {}", id);
    }
}

#[spacetimedb::reducer]
pub fn heartbeat(ctx: &ReducerContext, agent_id: String) {
    ensure_agent_exists(ctx, &agent_id);
    let Some(existing) = ctx.db.agent().id().find(&agent_id) else {
        return;
    };
    ctx.db.agent().id().update(Agent {
        last_seen: ctx.timestamp,
        ..existing
    });
}

#[spacetimedb::reducer]
pub fn end_agent(ctx: &ReducerContext, agent_id: String) {
    ctx.db.agent().id().delete(&agent_id);
    ctx.db.agent_activity().agent_id().delete(&agent_id);
    log::info!("Agent ended: {}", agent_id);
}

#[spacetimedb::reducer]
pub fn push_status(ctx: &ReducerContext, agent_id: String, state: String) {
    ensure_agent_exists(ctx, &agent_id);
    let now = ctx.timestamp;

    if let Some(existing) = ctx.db.agent_activity().agent_id().find(&agent_id) {
        ctx.db.agent_activity().agent_id().update(AgentActivity {
            agent_id: agent_id.clone(),
            state,
            last_tool_event: existing.last_tool_event,
            updated_at: now,
        });
    } else {
        ctx.db.agent_activity().insert(AgentActivity {
            agent_id: agent_id.clone(),
            state,
            last_tool_event: None,
            updated_at: now,
        });
    }

    if let Some(existing) = ctx.db.agent().id().find(&agent_id) {
        ctx.db.agent().id().update(Agent {
            last_seen: now,
            ..existing
        });
    }
}

#[spacetimedb::reducer]
pub fn push_context_usage(
    ctx: &ReducerContext,
    agent_id: String,
    used: u64,
    window: u64,
) {
    ensure_agent_exists(ctx, &agent_id);
    let Some(existing) = ctx.db.agent().id().find(&agent_id) else {
        return;
    };
    ctx.db.agent().id().update(Agent {
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
    agent_id: String,
    tool: String,
    detail: String,
) {
    ensure_agent_exists(ctx, &agent_id);
    let now = ctx.timestamp;

    ctx.db.tool_event().insert(ToolEvent {
        id,
        agent_id: agent_id.clone(),
        tool,
        detail: detail.clone(),
        started_at: now,
    });

    if ctx.db.agent_activity().agent_id().find(&agent_id).is_some() {
        ctx.db.agent_activity().agent_id().update(AgentActivity {
            agent_id: agent_id.clone(),
            state: "tool_use".to_string(),
            last_tool_event: Some(detail),
            updated_at: now,
        });
    } else {
        ctx.db.agent_activity().insert(AgentActivity {
            agent_id: agent_id.clone(),
            state: "tool_use".to_string(),
            last_tool_event: Some(detail),
            updated_at: now,
        });
    }

    if let Some(existing) = ctx.db.agent().id().find(&agent_id) {
        ctx.db.agent().id().update(Agent {
            last_seen: now,
            ..existing
        });
    }
}

#[spacetimedb::reducer]
pub fn push_message(
    ctx: &ReducerContext,
    id: String,
    agent_id: String,
    role: String,
    text: String,
    source: String,
) {
    ensure_agent_exists(ctx, &agent_id);
    let now = ctx.timestamp;

    if ctx.db.message().id().find(&id).is_none() {
        ctx.db.message().insert(Message {
            id,
            agent_id: agent_id.clone(),
            role,
            text,
            source,
            created_at: now,
        });
    }

    if let Some(existing) = ctx.db.agent().id().find(&agent_id) {
        ctx.db.agent().id().update(Agent {
            last_seen: now,
            ..existing
        });
    }
}

#[spacetimedb::reducer]
pub fn push_image(
    ctx: &ReducerContext,
    id: String,
    agent_id: String,
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

    ensure_agent_exists(ctx, &agent_id);
    let now = ctx.timestamp;

    if ctx.db.message().id().find(&id).is_none() {
        ctx.db.message().insert(Message {
            id: id.clone(),
            agent_id: agent_id.clone(),
            role: "user".to_string(),
            text: caption,
            source: "flutter".to_string(),
            created_at: now,
        });

        ctx.db.message_image().insert(MessageImage {
            message_id: id,
            bytes,
        });
    }

    if let Some(existing) = ctx.db.agent().id().find(&agent_id) {
        ctx.db.agent().id().update(Agent {
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
    agent_id: String,
    tool: String,
    input: String,
) {
    ctx.db.permission_request().insert(PermissionRequest {
        id,
        agent_id,
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
pub fn request_question(
    ctx: &ReducerContext,
    id: String,
    agent_id: String,
    question: String,
    header: String,
    options: String,
    multi_select: bool,
) {
    ctx.db.question_request().insert(QuestionRequest {
        id,
        agent_id,
        question,
        header,
        options,
        multi_select,
        status: "pending".to_string(),
        response: None,
        created_at: ctx.timestamp,
        resolved_at: None,
    });
}

#[spacetimedb::reducer]
pub fn respond_to_question(ctx: &ReducerContext, id: String, response: String) {
    let Some(existing) = ctx.db.question_request().id().find(&id) else {
        log::warn!("respond_to_question: not found: {}", id);
        return;
    };
    ctx.db.question_request().id().update(QuestionRequest {
        status: "answered".to_string(),
        response: Some(response),
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

    let answered_question_ids: Vec<String> = ctx
        .db
        .question_request()
        .iter()
        .filter(|q| {
            q.status != "pending"
                && q.resolved_at
                    .map(|t: spacetimedb::Timestamp| {
                        t.to_micros_since_unix_epoch() < cutoff_micros
                    })
                    .unwrap_or(false)
        })
        .map(|q| q.id.clone())
        .collect();

    for id in &answered_question_ids {
        ctx.db.question_request().id().delete(id);
    }

    log::info!(
        "sweep_old_messages: purged {} messages, {} tool_events, {} permissions, {} questions",
        old_message_ids.len(),
        old_tool_event_ids.len(),
        resolved_permission_ids.len(),
        answered_question_ids.len()
    );
}

#[spacetimedb::reducer]
pub fn delete_agent(ctx: &ReducerContext, agent_id: String) {
    let message_ids: Vec<String> = ctx
        .db
        .message()
        .iter()
        .filter(|m| m.agent_id == agent_id)
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
        .filter(|t| t.agent_id == agent_id)
        .map(|t| t.id.clone())
        .collect();

    for id in &tool_event_ids {
        ctx.db.tool_event().id().delete(id);
    }

    let permission_ids: Vec<String> = ctx
        .db
        .permission_request()
        .iter()
        .filter(|p| p.agent_id == agent_id)
        .map(|p| p.id.clone())
        .collect();

    for id in &permission_ids {
        ctx.db.permission_request().id().delete(id);
    }

    let question_ids: Vec<String> = ctx
        .db
        .question_request()
        .iter()
        .filter(|q| q.agent_id == agent_id)
        .map(|q| q.id.clone())
        .collect();

    for id in &question_ids {
        ctx.db.question_request().id().delete(id);
    }

    ctx.db.agent_activity().agent_id().delete(&agent_id);
    ctx.db.agent().id().delete(&agent_id);

    log::info!(
        "delete_agent({}): purged {} messages, {} tool_events, {} permissions, {} questions",
        agent_id,
        message_ids.len(),
        tool_event_ids.len(),
        permission_ids.len(),
        question_ids.len()
    );
}

#[spacetimedb::reducer]
pub fn clear_all_agents(ctx: &ReducerContext) {
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

    let question_ids: Vec<String> = ctx
        .db
        .question_request()
        .iter()
        .map(|q| q.id.clone())
        .collect();
    for id in &question_ids {
        ctx.db.question_request().id().delete(id);
    }

    let activity_ids: Vec<String> = ctx
        .db
        .agent_activity()
        .iter()
        .map(|a| a.agent_id.clone())
        .collect();
    for id in &activity_ids {
        ctx.db.agent_activity().agent_id().delete(id);
    }

    let agent_ids: Vec<String> = ctx.db.agent().iter().map(|s| s.id.clone()).collect();
    for id in &agent_ids {
        ctx.db.agent().id().delete(id);
    }

    log::info!(
        "clear_all_agents: purged {} agents, {} activities, {} messages, {} tool_events, {} permissions, {} questions",
        agent_ids.len(),
        activity_ids.len(),
        message_ids.len(),
        tool_event_ids.len(),
        permission_ids.len(),
        question_ids.len()
    );
}
