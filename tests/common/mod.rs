//! Shared rig for the HTTP test suites.
//!
//! Requests are hand-rolled over `TcpStream` rather than built with a client
//! crate: these tests exist to pin wire behaviour, and a client library would
//! normalise away the malformed and edge-case requests they need to send.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

pub type HttpResponse = (u16, Vec<(String, String)>, Vec<u8>);

/// Starts the daemon's file server in-process on an ephemeral port. The task
/// lives for the rest of the test binary.
pub fn start_daemon(vault: &Path) -> Option<u16> {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

    let rt = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    });

    let vault = vault.to_path_buf();
    let listener = rt
        .block_on(async { tokio::net::TcpListener::bind(("127.0.0.1", 0)).await })
        .ok()?;
    let port = listener.local_addr().ok()?.port();

    rt.spawn(async move {
        let _ = spacenotes::files_http::serve(vault, listener).await;
    });

    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Some(port);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

/// A scratch vault unique to this process, so a re-run never sees the previous
/// run's files and mistakes a create for an overwrite.
pub fn temp_vault(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spacenotes-test-{}-{}",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch vault");
    dir
}

/// A request carrying no body, and deliberately no `Content-Length`: the GET
/// and HEAD cases assert what the server does with a bare request line.
pub fn request(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
) -> std::io::Result<HttpResponse> {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;

    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes())?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    Ok(parse_response(&raw))
}

pub fn request_with_body(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> std::io::Result<HttpResponse> {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;

    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes())?;
    stream.write_all(body)?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    Ok(parse_response(&raw))
}

pub fn parse_response(raw: &[u8]) -> HttpResponse {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(raw.len());
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let body = raw.get(split + 4..).unwrap_or(&[]).to_vec();

    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);

    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();

    (status, headers, body)
}

pub fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
}
