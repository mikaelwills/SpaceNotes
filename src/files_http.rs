//! HTTP server for vault file bytes.
//!
//! Takes over `/files/` and `/thumbnails/` from nginx, which keeps serving the
//! web app. The split is by capability, not tidiness: nginx cannot do
//! resumable upload at all (`ngx_http_dav` returns 501 for a PUT carrying
//! `Content-Range`), and file serving wants vault and database knowledge nginx
//! structurally cannot have.
//!
//! Behavioural parity with nginx is pinned by `tests/files_http.rs`, which
//! runs the same expectation table against both.

use std::path::PathBuf;

use anyhow::Result;
use axum::Router;
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

/// Routes `/files/` and `/thumbnails/` at the vault.
///
/// `ServeDir` gives Range, 416 with `Content-Range: bytes */len`, suffix
/// ranges, HEAD, and traversal rejection without extra work — all verified
/// against the nginx baseline rather than assumed.
pub fn router(vault_path: PathBuf) -> Router {
    let thumbnails = vault_path.join(".thumbnails");

    Router::new()
        .nest_service(
            "/files",
            ServeDir::new(&vault_path).append_index_html_on_directories(false),
        )
        .nest_service(
            "/thumbnails",
            ServeDir::new(thumbnails).append_index_html_on_directories(false),
        )
}

/// Serves until the process ends. Spawned alongside the watcher.
pub async fn serve(vault_path: PathBuf, listener: TcpListener) -> Result<()> {
    let addr = listener.local_addr()?;
    tracing::info!("File server listening on {addr}");
    axum::serve(listener, router(vault_path)).await?;
    Ok(())
}
