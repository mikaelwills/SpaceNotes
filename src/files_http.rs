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

use std::path::{Path, PathBuf};

use anyhow::Result;
use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

use crate::vault_path;

#[derive(Clone)]
struct FilesState {
    vault_path: PathBuf,
}

/// Routes `/files/` and `/thumbnails/` at the vault.
///
/// `ServeDir` gives Range, 416 with `Content-Range: bytes */len`, suffix
/// ranges, HEAD, and traversal rejection without extra work — all verified
/// against the nginx baseline rather than assumed.
pub fn router(vault_path: PathBuf) -> Router {
    let thumbnails = vault_path.join(".thumbnails");
    let state = FilesState {
        vault_path: vault_path.clone(),
    };

    let files = ServeDir::new(&vault_path).append_index_html_on_directories(false);

    Router::new()
        .route("/files/*path", any(files_route))
        .with_state(state)
        .fallback_service(
            Router::new()
                .nest_service("/files", files)
                .nest_service(
                    "/thumbnails",
                    ServeDir::new(thumbnails).append_index_html_on_directories(false),
                ),
        )
}

/// Reads go to `ServeDir`; writes are handled here. Splitting on method
/// inside one route keeps a single source of truth for the `/files/` prefix.
async fn files_route(State(state): State<FilesState>, request: Request) -> Response {
    match *request.method() {
        axum::http::Method::PUT => put_file(state, request).await,
        _ => {
            let files =
                ServeDir::new(&state.vault_path).append_index_html_on_directories(false);
            match tower::ServiceExt::oneshot(files, strip_files_prefix(request)).await {
                Ok(response) => response.into_response(),
                Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
    }
}

/// `ServeDir` resolves against its own root, so the `/files` prefix that the
/// router matched on has to come back off before handing the request over.
fn strip_files_prefix(mut request: Request) -> Request {
    let uri = request.uri();
    let path = uri.path();
    let stripped = path.strip_prefix("/files").unwrap_or(path);
    let stripped = if stripped.is_empty() { "/" } else { stripped };

    let rebuilt = match uri.query() {
        Some(q) => format!("{stripped}?{q}"),
        None => stripped.to_string(),
    };

    if let Ok(new_uri) = rebuilt.parse::<Uri>() {
        *request.uri_mut() = new_uri;
    }
    request
}

/// Whole-file upload. A `Content-Range` PUT keeps nginx's 501 until the
/// resumable endpoint exists, so a client can never mistake a full overwrite
/// for a partial write.
async fn put_file(state: FilesState, request: Request) -> Response {
    let headers = request.headers().clone();

    if headers.contains_key(header::CONTENT_RANGE) {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }

    let raw_path = request.uri().path().to_string();
    let relative = match decode_vault_relative(&raw_path) {
        Some(relative) => relative,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    if relative.is_empty() || raw_path.ends_with('/') {
        return StatusCode::CONFLICT.into_response();
    }

    let target = match vault_path::resolve_vault_path(&state.vault_path, &relative) {
        Ok(target) => target,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    if target.is_dir() {
        return StatusCode::CONFLICT.into_response();
    }

    let existed = target.exists();

    match stream_to_file(&target, request.into_body()).await {
        Ok(()) => {}
        Err(error) => {
            tracing::warn!("PUT {relative} failed: {error}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    apply_date_header(&target, &headers);

    if existed {
        StatusCode::NO_CONTENT.into_response()
    } else {
        let location = format!("/files/{raw}", raw = raw_path.trim_start_matches("/files/"));
        (StatusCode::CREATED, [(header::LOCATION, location)]).into_response()
    }
}

/// Percent-decodes each segment separately, so an encoded separator inside a
/// segment stays data rather than becoming a path boundary.
fn decode_vault_relative(path: &str) -> Option<String> {
    let trimmed = path.trim_start_matches("/files").trim_start_matches('/');

    let mut out = Vec::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() {
            continue;
        }
        let decoded = percent_decode(segment)?;
        if decoded.contains('/') || decoded.contains('\0') {
            return None;
        }
        out.push(decoded);
    }
    Some(out.join("/"))
}

fn percent_decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }

    String::from_utf8(out).ok()
}

/// Writes to a sibling `.tmp` and renames, the same guarantee
/// `writer::write_file_to_disk` gives: an interrupted upload never leaves a
/// half-written file under the real name.
async fn stream_to_file(target: &Path, body: Body) -> Result<()> {
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let tmp = append_tmp_suffix(target);
    let mut handle = tokio::fs::File::create(&tmp).await?;

    let mut stream = body.into_data_stream();
    let mut write_result = Ok(());

    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                if let Err(error) = handle.write_all(&bytes).await {
                    write_result = Err(anyhow::Error::from(error));
                    break;
                }
            }
            Err(error) => {
                write_result = Err(anyhow::anyhow!("body stream failed: {error}"));
                break;
            }
        }
    }

    if write_result.is_err() {
        drop(handle);
        let _ = tokio::fs::remove_file(&tmp).await;
        return write_result;
    }

    handle.sync_all().await?;
    drop(handle);
    tokio::fs::rename(&tmp, target).await?;
    Ok(())
}

fn append_tmp_suffix(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    target.with_file_name(name)
}

/// Honours a client-supplied `Date` so an uploaded file keeps its own
/// timestamp instead of the moment it happened to arrive.
fn apply_date_header(target: &Path, headers: &HeaderMap) {
    let Some(raw) = headers.get(header::DATE).and_then(|v| v.to_str().ok()) else {
        return;
    };
    let Ok(parsed) = httpdate::parse_http_date(raw) else {
        return;
    };
    let Ok(since_epoch) = parsed.duration_since(std::time::UNIX_EPOCH) else {
        return;
    };

    let mtime = filetime::FileTime::from_unix_time(
        since_epoch.as_secs() as i64,
        since_epoch.subsec_nanos(),
    );
    let _ = filetime::set_file_mtime(target, mtime);
}

/// Serves until the process ends. Spawned alongside the watcher.
pub async fn serve(vault_path: PathBuf, listener: TcpListener) -> Result<()> {
    let addr = listener.local_addr()?;
    tracing::info!("File server listening on {addr}");
    axum::serve(listener, router(vault_path)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_each_segment_independently() {
        assert_eq!(
            decode_vault_relative("/files/sub/Heaper%20test.md").unwrap(),
            "sub/Heaper test.md"
        );
        assert_eq!(
            decode_vault_relative("/files/Caf%C3%A9%20%F0%9F%8E%B5.md").unwrap(),
            "Café 🎵.md"
        );
    }

    #[test]
    fn an_encoded_separator_never_becomes_a_path_boundary() {
        assert!(decode_vault_relative("/files/..%2F..%2Fetc%2Fpasswd").is_none());
        assert!(decode_vault_relative("/files/a%2Fb.md").is_none());
    }

    #[test]
    fn malformed_percent_escapes_are_refused() {
        assert!(decode_vault_relative("/files/bad%zz.md").is_none());
        assert!(decode_vault_relative("/files/truncated%2").is_none());
    }

    #[test]
    fn a_traversal_segment_survives_decoding_and_is_caught_by_the_resolver() {
        let relative = decode_vault_relative("/files/../evil.md").unwrap();
        assert_eq!(relative, "../evil.md");
        assert!(vault_path::resolve_vault_path(Path::new("/vault"), &relative).is_err());
    }
}
