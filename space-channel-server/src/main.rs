use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
};
use tokio::sync::{broadcast, RwLock};
use tracing::{info, warn, error};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum ClientMessage {
    Chat { text: String },
    Register { session: String, project: String, task: String },
    Reply { session: String, id: String, text: String },
    Edit { session: String, id: String, text: String },
    ToolEvent {
        session: String,
        project: String,
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
        project: Option<String>,
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
}

#[derive(Debug, Clone)]
struct ChannelSession {
    session_id: String,
    project: String,
    task: String,
}

struct AppState {
    to_flutter: broadcast::Sender<String>,
    to_master: broadcast::Sender<String>,
    sessions: RwLock<HashMap<String, ChannelSession>>,
    session_senders: RwLock<HashMap<String, broadcast::Sender<String>>>,
}

impl AppState {
    fn new() -> Self {
        let (to_flutter, _) = broadcast::channel(256);
        let (to_master, _) = broadcast::channel(256);
        Self {
            to_flutter,
            to_master,
            sessions: RwLock::new(HashMap::new()),
            session_senders: RwLock::new(HashMap::new()),
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
                        let _ = state_clone.to_master.send(forward.to_string());
                        let has_image = raw.get("image_base64").is_some();
                        info!("Routed Flutter message to master: {text} (image: {has_image})");
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

    let first_msg = match receiver.next().await {
        Some(Ok(Message::Text(text))) => text.to_string(),
        _ => {
            warn!("SpaceChannel disconnected before registering");
            return;
        }
    };

    let Ok(register) = serde_json::from_str::<ClientMessage>(&first_msg) else {
        warn!("SpaceChannel sent invalid register message");
        return;
    };

    let ClientMessage::Register { session, project, task } = register else {
        warn!("SpaceChannel first message was not a register");
        return;
    };

    let channel_session = ChannelSession {
        session_id: session.clone(),
        project: project.clone(),
        task: task.clone(),
    };

    info!(
        session = %session,
        project = %project,
        task = %task,
        "SpaceChannel registered"
    );

    let (session_tx, _) = broadcast::channel::<String>(64);
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session.clone(), channel_session);
        let mut senders = state.session_senders.write().await;
        senders.insert(session.clone(), session_tx.clone());
    }

    let connect_event = serde_json::json!({
        "type": "session",
        "action": "connected",
        "session": &session,
        "project": &project,
        "task": &task,
    });
    let _ = state.to_flutter.send(connect_event.to_string());

    let mut session_rx = session_tx.subscribe();

    let send_task = tokio::spawn({
        let session = session.clone();
        async move {
            loop {
                let Some(msg) = session_rx.recv().await.ok() else { break };
                if sender.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }
            session
        }
    });

    let recv_task = tokio::spawn({
        let state = state.clone();
        let session = session.clone();
        let project = project.clone();
        let task = task.clone();
        async move {
            while let Some(Ok(msg)) = receiver.next().await {
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
                            "project": &project,
                            "task": &task,
                            "id": id,
                            "text": text,
                            "ts": chrono_ts(),
                        });
                        let _ = state.to_flutter.send(forward.to_string());
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
                            "project": &project,
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
                            "project": &project,
                            "task": &task,
                            "request_id": request_id,
                            "tool_name": tool_name,
                            "description": description,
                            "input_preview": input_preview,
                        });
                        let _ = state.to_flutter.send(forward.to_string());
                        info!(session = %session, request_id = %request_id, tool = %tool_name, "Permission request forwarded to Flutter");
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

    {
        let mut sessions = state.sessions.write().await;
        sessions.remove(&session);
        let mut senders = state.session_senders.write().await;
        senders.remove(&session);
    }

    let disconnect_event = serde_json::json!({
        "type": "session",
        "action": "disconnected",
        "session": &session,
    });
    let _ = state.to_flutter.send(disconnect_event.to_string());

    info!(session = %session, "SpaceChannel disconnected");
}

async fn webhook_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: String,
) -> impl IntoResponse {
    let source = headers
        .get("X-Source")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("webhook")
        .to_string();

    let forward = serde_json::json!({
        "type": "webhook",
        "text": body,
        "source": source,
    });
    let _ = state.to_master.send(forward.to_string());

    let flutter_event = serde_json::json!({
        "type": "webhook",
        "source": source,
        "text": body,
        "ts": chrono_ts(),
    });
    let _ = state.to_flutter.send(flutter_event.to_string());

    "ok"
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
