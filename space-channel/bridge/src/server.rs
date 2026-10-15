use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::signal::unix::{signal, SignalKind};

use crate::bridge::Bridge;

pub async fn serve(bridge: Arc<Bridge>, socket: &str) -> anyhow::Result<()> {
    let _ = std::fs::remove_file(socket);
    let listener = tokio::net::UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    bridge.log(&format!("listening on {socket}"));

    let app = Router::new().route("/call/{tool}", post(call)).with_state(bridge.clone());
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    tokio::select! {
        served = axum::serve(listener, app) => served?,
        _ = term.recv() => bridge.log("SIGTERM"),
        _ = int.recv() => bridge.log("SIGINT"),
    }
    Ok(())
}

async fn call(State(bridge): State<Arc<Bridge>>, Path(tool): Path<String>, body: String) -> Response {
    let args: Value = if body.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({ "error": format!("invalid JSON body: {e}") }))).into_response(),
        }
    };
    match bridge.call(&tool, args).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "error": error }))).into_response(),
    }
}
