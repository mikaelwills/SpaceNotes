use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{broadcast, watch, RwLock};
use tokio::time::{interval, timeout};
use tracing::{info, warn, error};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum ClientMessage {
    Chat { text: String },
    Register { session: String, #[serde(default)] task: String },
    Reply { session: String, id: String, text: String },
    Edit { session: String, id: String, text: String },
    ToolEvent {
        session: String,
        #[serde(default)]
        task: String,
        tool: String,
        #[serde(default)]
        input: serde_json::Value,
        #[serde(default)]
        hook_event: Option<String>,
        #[serde(default)]
        ts: u64,
    },
    Session {
        action: String,
        session: String,
        #[serde(default)]
        task: Option<String>,
    },
    PermissionRequest {
        session: String,
        request_id: String,
        tool_name: String,
        description: String,
        #[serde(default)]
        input_preview: String,
    },
    PermissionResponse {
        session: String,
        request_id: String,
        behavior: String,
    },
    Log {
        session: String,
        line: String,
    },
    Status {
        session: String,
        state: String,
    },
    Msg {
        session: String,
        text: String,
        #[serde(default)]
        id: String,
        #[serde(default)]
        from: String,
    },
}

#[derive(Debug, Clone)]
struct ChannelSession {
    session_id: String,
    task: String,
}

static CONN_ID: AtomicU64 = AtomicU64::new(0);

const PING_INTERVAL: Duration = Duration::from_secs(30);
const PONG_TIMEOUT: Duration = Duration::from_secs(60);

struct AppState {
    to_flutter: broadcast::Sender<String>,
    sessions: RwLock<HashMap<String, ChannelSession>>,
    session_senders: RwLock<HashMap<String, broadcast::Sender<String>>>,
    session_conn_ids: RwLock<HashMap<String, u64>>,
    session_cancel: RwLock<HashMap<String, watch::Sender<()>>>,
    message_history: RwLock<HashMap<String, VecDeque<String>>>,
    session_logs: RwLock<HashMap<String, VecDeque<String>>>,
}

impl AppState {
    const MAX_HISTORY: usize = 20;
    const MAX_LOGS: usize = 1000;

    fn new() -> Self {
        let (to_flutter, _) = broadcast::channel(256);
        Self {
            to_flutter,
            sessions: RwLock::new(HashMap::new()),
            session_senders: RwLock::new(HashMap::new()),
            session_conn_ids: RwLock::new(HashMap::new()),
            session_cancel: RwLock::new(HashMap::new()),
            message_history: RwLock::new(HashMap::new()),
            session_logs: RwLock::new(HashMap::new()),
        }
    }

    async fn push_history(&self, session_id: &str, msg: String) {
        let mut history = self.message_history.write().await;
        let buf = history.entry(session_id.to_string()).or_insert_with(VecDeque::new);
        buf.push_back(msg);
        if buf.len() > Self::MAX_HISTORY {
            buf.pop_front();
        }
    }

    async fn push_log(&self, session_id: &str, line: String) {
        let mut logs = self.session_logs.write().await;
        let buf = logs.entry(session_id.to_string()).or_insert_with(VecDeque::new);
        buf.push_back(line);
        if buf.len() > Self::MAX_LOGS {
            buf.pop_front();
        }
    }

    async fn send_to_session(&self, session_id: &str, msg: String) {
        let senders = self.session_senders.read().await;
        if let Some(tx) = senders.get(session_id) {
            let _ = tx.send(msg);
        } else {
            warn!("No session sender found for session: {session_id}");
        }
    }

    async fn ensure_webhook_session(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.write().await;
        if sessions.contains_key(session_id) {
            return false;
        }
        sessions.insert(session_id.to_string(), ChannelSession {
            session_id: session_id.to_string(),
            task: String::new(),
        });
        let connect_event = serde_json::json!({
            "type": "session",
            "action": "connected",
            "session": session_id,
            "task": "",
        });
        let _ = self.to_flutter.send(connect_event.to_string());
        info!(session = %session_id, "Webhook session auto-registered");
        true
    }
}

async fn flutter_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_flutter_socket(socket, state))
}

async fn handle_flutter_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();

    info!("Flutter client connected");

    {
        let sessions = state.sessions.read().await;
        let history = state.message_history.read().await;
        for cs in sessions.values() {
            let event = serde_json::json!({
                "type": "session",
                "action": "connected",
                "session": &cs.session_id,
                "task": &cs.task,
            });
            if sender.send(Message::Text(event.to_string().into())).await.is_err() {
                info!("Flutter client disconnected during session sync");
                return;
            }

            if let Some(buf) = history.get(&cs.session_id) {
                if !buf.is_empty() {
                    let messages: Vec<serde_json::Value> = buf.iter()
                        .filter_map(|m| serde_json::from_str(m).ok())
                        .collect();
                    let batch = serde_json::json!({
                        "type": "history_batch",
                        "session": &cs.session_id,
                        "messages": messages,
                    });
                    if sender.send(Message::Text(batch.to_string().into())).await.is_err() {
                        info!("Flutter client disconnected during history sync");
                        return;
                    }
                }
            }
        }
        info!("Sent {} existing sessions to Flutter client", sessions.len());
    }

    let mut flutter_rx = state.to_flutter.subscribe();

    let send_task = tokio::spawn(async move {
        while let Ok(msg) = flutter_rx.recv().await {
            if sender.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    let state_clone = state.clone();
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            let Message::Text(text) = msg else { continue };
            let text_str: &str = &text;
            info!("Flutter received: {text_str}");
            let Ok(raw) = serde_json::from_str::<serde_json::Value>(text_str) else {
                warn!("Flutter sent invalid JSON: {text_str}");
                continue;
            };
            let msg_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let text = raw.get("text").and_then(|v| v.as_str())
                .or_else(|| raw.get("content").and_then(|v| v.as_str()))
                .unwrap_or("");
            match msg_type {
                "chat" | "msg" | "" => {
                    if !text.is_empty() || raw.get("image_base64").is_some() {
                        let target_session = raw.get("session").and_then(|v| v.as_str()).unwrap_or("");
                        let mut forward = serde_json::json!({
                            "type": "chat",
                            "text": text,
                        });
                        if let Some(img) = raw.get("image_base64") {
                            forward["image_base64"] = img.clone();
                        }
                        if let Some(mime) = raw.get("image_mime") {
                            forward["image_mime"] = mime.clone();
                        }
                        if target_session.is_empty() {
                            let senders = state_clone.session_senders.read().await;
                            for tx in senders.values() {
                                let _ = tx.send(forward.to_string());
                            }
                            info!("Broadcast Flutter message to all sessions: {text}");
                        } else {
                            state_clone.send_to_session(target_session, forward.to_string()).await;
                            info!("Routed Flutter message to session {target_session}: {text}");
                        }
                    }
                }
                "permission_response" => {
                    let target_session = raw.get("session").and_then(|v| v.as_str()).unwrap_or("");
                    let request_id = raw.get("request_id").and_then(|v| v.as_str()).unwrap_or("");
                    let behavior = raw.get("behavior").and_then(|v| v.as_str()).unwrap_or("deny");
                    if target_session.is_empty() || request_id.is_empty() {
                        warn!("Flutter permission_response missing session or request_id");
                        continue;
                    }
                    let forward = serde_json::json!({
                        "type": "permission_response",
                        "request_id": request_id,
                        "behavior": behavior,
                    });
                    state_clone.send_to_session(target_session, forward.to_string()).await;
                    info!(session = %target_session, request_id = %request_id, behavior = %behavior, "Permission response routed to session");
                }
                _ => {
                    warn!("Flutter sent unknown message type: {msg_type}");
                }
            }
        }
    });

    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }

    info!("Flutter client disconnected");
}

async fn channel_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_channel_socket(socket, state))
}

async fn handle_channel_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();

    info!("SpaceChannel client connected, waiting for register...");

    let first_msg = match timeout(Duration::from_secs(10), receiver.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => text.to_string(),
        Ok(_) => {
            warn!("SpaceChannel disconnected before registering");
            return;
        }
        Err(_) => {
            warn!("SpaceChannel registration timed out (10s)");
            return;
        }
    };

    let Ok(register) = serde_json::from_str::<ClientMessage>(&first_msg) else {
        warn!("SpaceChannel sent invalid register message");
        return;
    };

    let ClientMessage::Register { session, task } = register else {
        warn!("SpaceChannel first message was not a register");
        return;
    };

    let channel_session = ChannelSession {
        session_id: session.clone(),
        task: task.clone(),
    };

    let conn_id = CONN_ID.fetch_add(1, Ordering::Relaxed);

    info!(
        session = %session,
        task = %task,
        conn_id = conn_id,
        "SpaceChannel registered"
    );

    let (cancel_tx, cancel_rx) = watch::channel(());
    let session_tx = {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session.clone(), channel_session);
        let mut senders = state.session_senders.write().await;
        let (session_tx, _) = broadcast::channel::<String>(64);
        senders.insert(session.clone(), session_tx.clone());
        let mut conn_ids = state.session_conn_ids.write().await;
        conn_ids.insert(session.clone(), conn_id);
        let mut cancels = state.session_cancel.write().await;
        if let Some(old_cancel) = cancels.insert(session.clone(), cancel_tx) {
            let _ = old_cancel.send(());
            info!(session = %session, "Cancelled previous connection");
        }
        session_tx
    };

    let connect_event = serde_json::json!({
        "type": "session",
        "action": "connected",
        "session": &session,
        "task": &task,
    });
    let _ = state.to_flutter.send(connect_event.to_string());

    let ack = serde_json::json!({ "type": "register_ack" }).to_string();
    if sender.send(Message::Text(ack.into())).await.is_err() {
        warn!(session = %session, "Failed to send register_ack");
        return;
    }

    let mut session_rx = session_tx.subscribe();

    let last_pong = Arc::new(RwLock::new(tokio::time::Instant::now()));

    let send_task = tokio::spawn({
        let session = session.clone();
        let last_pong = last_pong.clone();
        let mut cancel_rx = cancel_rx;
        async move {
            let mut ping_interval = interval(PING_INTERVAL);
            ping_interval.tick().await;
            loop {
                tokio::select! {
                    msg = session_rx.recv() => {
                        let Some(msg) = msg.ok() else { break };
                        if sender.send(Message::Text(msg.into())).await.is_err() {
                            break;
                        }
                    }
                    _ = ping_interval.tick() => {
                        if sender.send(Message::Ping(vec![].into())).await.is_err() {
                            info!(session = %session, "Ping send failed, client gone");
                            break;
                        }
                        let elapsed = last_pong.read().await.elapsed();
                        if elapsed > PONG_TIMEOUT {
                            info!(session = %session, elapsed_secs = elapsed.as_secs(), "No pong received, timing out");
                            break;
                        }
                    }
                    _ = cancel_rx.changed() => {
                        info!(session = %session, "Superseded, sending close 4001");
                        let _ = sender.send(Message::Close(Some(axum::extract::ws::CloseFrame {
                            code: 4001,
                            reason: "superseded".into(),
                        }))).await;
                        break;
                    }
                }
            }
            session
        }
    });

    let recv_task = tokio::spawn({
        let state = state.clone();
        let session = session.clone();
        let task = task.clone();
        let last_pong = last_pong.clone();
        async move {
            while let Some(Ok(msg)) = receiver.next().await {
                match &msg {
                    Message::Pong(_) => {
                        *last_pong.write().await = tokio::time::Instant::now();
                        continue;
                    }
                    Message::Text(_) => {}
                    _ => continue,
                }
                let Message::Text(text) = msg else { continue };
                let text_str: &str = &text;
                let Ok(parsed) = serde_json::from_str::<ClientMessage>(text_str) else {
                    warn!("SpaceChannel sent unparseable message");
                    continue;
                };
                match parsed {
                    ClientMessage::Reply { id, text, .. } => {
                        let forward = serde_json::json!({
                            "type": "msg",
                            "from": "assistant",
                            "session": &session,
                            "task": &task,
                            "id": id,
                            "text": text,
                            "ts": chrono_ts(),
                        });
                        let msg_str = forward.to_string();
                        state.push_history(&session, msg_str.clone()).await;
                        let _ = state.to_flutter.send(msg_str);
                    }
                    ClientMessage::Edit { id, text, .. } => {
                        let forward = serde_json::json!({
                            "type": "edit",
                            "session": &session,
                            "id": id,
                            "text": text,
                        });
                        let _ = state.to_flutter.send(forward.to_string());
                    }
                    ClientMessage::ToolEvent { tool, input, hook_event, .. } => {
                        let forward = serde_json::json!({
                            "type": "tool_event",
                            "session": &session,
                            "task": &task,
                            "tool": tool,
                            "input": input,
                            "hook_event": hook_event.as_deref().unwrap_or("PreToolUse"),
                            "ts": chrono_ts(),
                        });
                        let _ = state.to_flutter.send(forward.to_string());
                    }
                    ClientMessage::PermissionRequest { request_id, tool_name, description, input_preview, .. } => {
                        let forward = serde_json::json!({
                            "type": "permission_request",
                            "session": &session,
                            "task": &task,
                            "request_id": request_id,
                            "tool_name": tool_name,
                            "description": description,
                            "input_preview": input_preview,
                        });
                        let _ = state.to_flutter.send(forward.to_string());
                        info!(session = %session, request_id = %request_id, tool = %tool_name, "Permission request forwarded to Flutter");
                    }
                    ClientMessage::Log { line, .. } => {
                        state.push_log(&session, line).await;
                    }
                    ClientMessage::Status { state: status_state, .. } => {
                        if status_state == "disconnected" {
                            let mut sessions = state.sessions.write().await;
                            sessions.remove(&session);
                            let mut history = state.message_history.write().await;
                            history.remove(&session);
                            let disconnect_event = serde_json::json!({
                                "type": "session",
                                "action": "disconnected",
                                "session": &session,
                            });
                            let _ = state.to_flutter.send(disconnect_event.to_string());
                            info!(session = %session, "WS session disconnected via status");
                        } else {
                            let forward = serde_json::json!({
                                "type": "status",
                                "session": &session,
                                "state": status_state,
                            });
                            let _ = state.to_flutter.send(forward.to_string());
                            info!(session = %session, state = %status_state, "WS status forwarded to Flutter");
                        }
                    }
                    ClientMessage::Msg { text, id, from, .. } => {
                        let forward = serde_json::json!({
                            "type": "msg",
                            "from": from,
                            "session": &session,
                            "task": &task,
                            "id": id,
                            "text": text,
                            "ts": chrono_ts(),
                        });
                        let msg_str = forward.to_string();
                        state.push_history(&session, msg_str.clone()).await;
                        let _ = state.to_flutter.send(msg_str);
                        info!(session = %session, "WS msg forwarded to Flutter");
                    }
                    _ => {}
                }
            }
        }
    });

    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }

    let is_current = {
        let conn_ids = state.session_conn_ids.read().await;
        conn_ids.get(&session).copied() == Some(conn_id)
    };

    if is_current {
        let mut sessions = state.sessions.write().await;
        sessions.remove(&session);
        let mut senders = state.session_senders.write().await;
        senders.remove(&session);
        let mut conn_ids = state.session_conn_ids.write().await;
        conn_ids.remove(&session);
        let mut cancels = state.session_cancel.write().await;
        cancels.remove(&session);

        let disconnect_event = serde_json::json!({
            "type": "session",
            "action": "disconnected",
            "session": &session,
        });
        let _ = state.to_flutter.send(disconnect_event.to_string());
    }

    info!(session = %session, conn_id = conn_id, is_current = is_current, "SpaceChannel disconnected");
}

async fn webhook_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: String,
) -> impl IntoResponse {
    let header_source = headers
        .get("X-Source")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("webhook")
        .to_string();

    let parsed = serde_json::from_str::<serde_json::Value>(&body);

    let (source, text, target_session) = match parsed {
        Ok(json) => {
            let src = json["source"].as_str().unwrap_or(&header_source).to_string();
            let txt = json["text"].as_str().unwrap_or("").to_string();
            let sess = json["session"].as_str().map(|s| s.to_string());
            (src, txt, sess)
        }
        Err(_) => (header_source, body, None),
    };

    let ts = chrono_ts();

    if let Some(session_name) = &target_session {
        state.ensure_webhook_session(session_name).await;
    }

    let flutter_event = serde_json::json!({
        "type": "webhook",
        "source": source,
        "text": text,
        "session": target_session.as_deref().unwrap_or(""),
        "ts": ts,
    });
    let msg_str = flutter_event.to_string();
    if let Some(session_name) = &target_session {
        state.push_history(session_name, msg_str.clone()).await;
    }
    let _ = state.to_flutter.send(msg_str);

    if let Some(session_name) = &target_session {
        let session_event = serde_json::json!({
            "type": "webhook",
            "source": source,
            "text": text,
            "ts": ts,
        });
        state.send_to_session(session_name, session_event.to_string()).await;
        info!(session = %session_name, source = %source, "Webhook routed to session");
    }

    "ok"
}

async fn trello_webhook_head() -> impl IntoResponse {
    axum::http::StatusCode::OK
}

async fn trello_webhook_post(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: String,
) -> impl IntoResponse {
    let Ok(secret) = std::env::var("TRELLO_WEBHOOK_SECRET") else {
        warn!("TRELLO_WEBHOOK_SECRET not set");
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "not configured").into_response();
    };

    let Ok(callback_url) = std::env::var("TRELLO_CALLBACK_URL") else {
        warn!("TRELLO_CALLBACK_URL not set");
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "not configured").into_response();
    };

    let signature = headers
        .get("x-trello-webhook")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !verify_trello_signature(&body, &callback_url, &secret, signature) {
        warn!("Trello webhook rejected: invalid signature");
        return (axum::http::StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }

    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&body) else {
        warn!("Trello webhook: invalid JSON body");
        return (axum::http::StatusCode::BAD_REQUEST, "invalid json").into_response();
    };

    let action_type = payload["action"]["type"].as_str().unwrap_or("");

    let text = match action_type {
        "createCard" => format_trello_card_event("created", &payload),
        "updateCard" => format_trello_update_event(&payload),
        "commentCard" => format_trello_comment_event(&payload),
        "addLabelToCard" => format_trello_card_event("labelled", &payload),
        _ => {
            info!(action_type, "Trello webhook: filtered out");
            return (axum::http::StatusCode::OK, "filtered").into_response();
        }
    };

    let Some(text) = text else {
        return (axum::http::StatusCode::OK, "filtered").into_response();
    };

    let ts = chrono_ts();
    let flutter_event = serde_json::json!({
        "type": "webhook",
        "source": "trello",
        "text": text,
        "session": "lmc-website",
        "ts": ts,
    });
    let _ = state.to_flutter.send(flutter_event.to_string());

    let session_event = serde_json::json!({
        "type": "webhook",
        "source": "trello",
        "text": text,
        "ts": ts,
    });
    state.send_to_session("lmc-website", session_event.to_string()).await;

    info!(action_type, "Trello webhook forwarded");
    (axum::http::StatusCode::OK, "ok").into_response()
}

fn verify_trello_signature(body: &str, callback_url: &str, secret: &str, signature: &str) -> bool {
    let mut mac = match Hmac::<Sha1>::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body.as_bytes());
    mac.update(callback_url.as_bytes());
    let result = mac.finalize().into_bytes();
    let expected = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, result);
    expected == signature
}

fn format_trello_card_event(verb: &str, payload: &serde_json::Value) -> Option<String> {
    let card_name = payload["action"]["data"]["card"]["name"].as_str()?;
    let list_name = payload["action"]["data"]["list"]["name"].as_str().unwrap_or("unknown list");
    let member = payload["action"]["memberCreator"]["fullName"].as_str().unwrap_or("Someone");
    Some(format!("{member} {verb} card \"{card_name}\" in {list_name}"))
}

fn format_trello_update_event(payload: &serde_json::Value) -> Option<String> {
    let card_name = payload["action"]["data"]["card"]["name"].as_str()?;
    let member = payload["action"]["memberCreator"]["fullName"].as_str().unwrap_or("Someone");

    if let Some(list_after) = payload["action"]["data"]["listAfter"]["name"].as_str() {
        let list_before = payload["action"]["data"]["listBefore"]["name"].as_str().unwrap_or("unknown");
        return Some(format!("{member} moved \"{card_name}\" from {list_before} to {list_after}"));
    }

    if payload["action"]["data"]["old"].get("closed").is_some() {
        let closed = payload["action"]["data"]["card"]["closed"].as_bool().unwrap_or(false);
        let action = if closed { "archived" } else { "unarchived" };
        return Some(format!("{member} {action} \"{card_name}\""));
    }

    None
}

fn format_trello_comment_event(payload: &serde_json::Value) -> Option<String> {
    let card_name = payload["action"]["data"]["card"]["name"].as_str()?;
    let member = payload["action"]["memberCreator"]["fullName"].as_str().unwrap_or("Someone");
    let comment = payload["action"]["data"]["text"].as_str().unwrap_or("");
    let preview = if comment.len() > 100 { &comment[..100] } else { comment };
    Some(format!("{member} commented on \"{card_name}\": {preview}"))
}

async fn logs_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(session): axum::extract::Path<String>,
) -> impl IntoResponse {
    let logs = state.session_logs.read().await;
    match logs.get(&session) {
        Some(buf) => {
            let lines: Vec<&str> = buf.iter().map(|s| s.as_str()).collect();
            lines.join("\n")
        }
        None => format!("No logs for session: {session}"),
    }
}

fn chrono_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let state = Arc::new(AppState::new());

    let flutter_app = Router::new()
        .route("/ws", axum::routing::get(flutter_ws_handler))
        .with_state(state.clone());

    let channel_app = Router::new()
        .route("/ws", axum::routing::get(channel_ws_handler))
        .with_state(state.clone());

    let webhook_app = Router::new()
        .route("/webhook", axum::routing::post(webhook_handler))
        .route("/trello-webhook", axum::routing::head(trello_webhook_head).post(trello_webhook_post))
        .route("/logs/{session}", axum::routing::get(logs_handler))
        .with_state(state.clone());

    let flutter_addr = SocketAddr::from(([0, 0, 0, 0], 5054));
    let channel_addr = SocketAddr::from(([0, 0, 0, 0], 5055));
    let webhook_addr = SocketAddr::from(([0, 0, 0, 0], 5056));

    info!("Flutter WebSocket on {flutter_addr}");
    info!("SpaceChannel WebSocket on {channel_addr}");
    info!("Webhook HTTP on {webhook_addr}");

    let flutter_server = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(flutter_addr).await.unwrap();
        axum::serve(listener, flutter_app).await.unwrap();
    });

    let channel_server = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(channel_addr).await.unwrap();
        axum::serve(listener, channel_app).await.unwrap();
    });

    let webhook_server = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(webhook_addr).await.unwrap();
        axum::serve(listener, webhook_app).await.unwrap();
    });

    tokio::select! {
        r = flutter_server => { error!("Flutter server exited: {r:?}"); }
        r = channel_server => { error!("Channel server exited: {r:?}"); }
        r = webhook_server => { error!("Webhook server exited: {r:?}"); }
    }
}
