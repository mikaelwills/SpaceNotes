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

use crate::uploads;
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
        .route("/uploads", axum::routing::post(create_upload))
        .route("/uploads/:id", any(upload_route))
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

#[derive(serde::Deserialize)]
struct CreateUpload {
    path: String,
    size: u64,
}

/// Opens an upload session. Collision is settled here and only here: a resume
/// trusts the uniqueness established at this moment rather than re-checking.
async fn create_upload(
    State(state): State<FilesState>,
    axum::Json(body): axum::Json<CreateUpload>,
) -> Response {
    let target = match vault_path::resolve_vault_path(&state.vault_path, &body.path) {
        Ok(target) => target,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    if target.exists() {
        return StatusCode::CONFLICT.into_response();
    }

    let session = uploads::UploadSession {
        id: uploads::new_id(),
        path: body.path,
        size: body.size,
        created_ms: uploads::now_ms(),
    };

    if let Err(error) = uploads::write_session(&state.vault_path, &session) {
        tracing::error!("Could not open upload session: {error}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    (
        StatusCode::CREATED,
        [
            (header::LOCATION, format!("/uploads/{}", session.id)),
            (UPLOAD_OFFSET, "0".to_string()),
        ],
        axum::Json(serde_json::json!({ "id": session.id })),
    )
        .into_response()
}

const UPLOAD_OFFSET: header::HeaderName = header::HeaderName::from_static("upload-offset");
const UPLOAD_LENGTH: header::HeaderName = header::HeaderName::from_static("upload-length");

async fn upload_route(
    State(state): State<FilesState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    request: Request,
) -> Response {
    if !uploads::is_valid_id(&id) {
        return StatusCode::NOT_FOUND.into_response();
    }

    match *request.method() {
        axum::http::Method::HEAD => head_upload(state, id),
        axum::http::Method::PATCH => patch_upload(state, id, request).await,
        axum::http::Method::DELETE => {
            uploads::forget(&state.vault_path, &id);
            StatusCode::NO_CONTENT.into_response()
        }
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

/// Reports how many bytes actually survived, which is what a resuming client
/// asks before sending anything.
fn head_upload(state: FilesState, id: String) -> Response {
    let Ok(session) = uploads::read_session(&state.vault_path, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let offset = uploads::current_offset(&state.vault_path, &id);

    (
        StatusCode::OK,
        [
            (UPLOAD_OFFSET, offset.to_string()),
            (UPLOAD_LENGTH, session.size.to_string()),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
    )
        .into_response()
}

/// Appends one chunk at the client's stated offset. A mismatch is a 409
/// carrying the real offset, so a confused client can resynchronise instead
/// of corrupting the file.
async fn patch_upload(state: FilesState, id: String, request: Request) -> Response {
    let (parts, body) = request.into_parts();

    let Ok(session) = uploads::read_session(&state.vault_path, &id) else {
        drain(body).await;
        return StatusCode::NOT_FOUND.into_response();
    };

    let claimed = parts
        .headers
        .get(UPLOAD_OFFSET)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    let Some(claimed) = claimed else {
        drain(body).await;
        return StatusCode::BAD_REQUEST.into_response();
    };

    let actual = uploads::current_offset(&state.vault_path, &id);
    if claimed != actual {
        drain(body).await;
        return (
            StatusCode::CONFLICT,
            [(UPLOAD_OFFSET, actual.to_string())],
        )
            .into_response();
    }

    let remaining = session.size - actual;
    if declared_length(&parts.headers).is_some_and(|declared| declared > remaining) {
        drain(body).await;
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            [(UPLOAD_OFFSET, actual.to_string())],
        )
            .into_response();
    }

    let part = uploads::part_path(&state.vault_path, &id);
    let written = match append_chunk(&part, body, remaining).await {
        Ok(written) => written,
        Err(error) => {
            tracing::warn!("Upload {id} chunk failed: {error}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(
                    UPLOAD_OFFSET,
                    uploads::current_offset(&state.vault_path, &id).to_string(),
                )],
            )
                .into_response();
        }
    };

    let offset = actual + written;

    if offset < session.size {
        return (StatusCode::NO_CONTENT, [(UPLOAD_OFFSET, offset.to_string())])
            .into_response();
    }

    match finish_upload(&state.vault_path, &session).await {
        Ok(()) => (
            StatusCode::CREATED,
            [
                (header::LOCATION, format!("/files/{}", session.path)),
                (UPLOAD_OFFSET, offset.to_string()),
            ],
        )
            .into_response(),
        Err(error) => {
            tracing::error!("Upload {id} could not be finalised: {error}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Reads and discards a body being refused. Answering before the client has
/// finished sending closes the connection under it, and the client sees a
/// reset instead of the status explaining what went wrong.
async fn drain(body: Body) {
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        if chunk.is_err() {
            return;
        }
    }
}

fn declared_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
}

/// Appends to the part file, refusing to write past the declared size so a
/// runaway client cannot fill the vault.
async fn append_chunk(part: &Path, body: Body, remaining: u64) -> Result<u64> {
    if let Some(parent) = part.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut handle = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(part)
        .await?;

    let mut stream = body.into_data_stream();
    let mut written = 0u64;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| anyhow::anyhow!("body stream failed: {e}"))?;
        if written + chunk.len() as u64 > remaining {
            handle.sync_all().await?;
            anyhow::bail!("chunk exceeds declared upload size");
        }
        handle.write_all(&chunk).await?;
        written += chunk.len() as u64;
    }

    handle.sync_all().await?;
    Ok(written)
}

/// Moves the finished part file into the vault. The rename is atomic and on
/// the same filesystem, so the watcher sees one complete file appear.
async fn finish_upload(vault_root: &Path, session: &uploads::UploadSession) -> Result<()> {
    let target = vault_path::resolve_vault_path(vault_root, &session.path)?;

    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let part = uploads::part_path(vault_root, &session.id);
    tokio::fs::rename(&part, &target).await?;
    let _ = tokio::fs::remove_file(uploads::meta_path(vault_root, &session.id)).await;
    Ok(())
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
