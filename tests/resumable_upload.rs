//! Resumable upload protocol, end to end against the daemon.
//!
//! nginx has no equivalent, so there is no baseline to match here — these
//! assert the protocol's own contract, and above all the one case it exists
//! for: a transfer that dies partway must resume from what survived rather
//! than start again.
//!
//! Run: `cargo test --test resumable_upload -- --nocapture`

use std::io::{Read, Write};
use std::sync::OnceLock;
use std::time::Duration;

fn start_daemon(vault: &std::path::Path) -> u16 {
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
        .expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();

    rt.spawn(async move {
        let _ = spacenotes::files_http::serve(vault, listener).await;
    });

    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return port;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("daemon did not start");
}

fn temp_vault(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spacenotes-resume-{}-{}",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create vault");
    dir
}

fn request(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let mut stream =
        std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to daemon");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("set timeout");

    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).expect("write request");
    stream.write_all(body).expect("write body");

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    parse_response(&raw)
}

/// Sends a PATCH but hangs up partway through the declared body, which is
/// what a dropped wifi connection looks like to the server.
fn truncated_patch(port: u16, id: &str, offset: u64, declared: usize, actually_sent: &[u8]) {
    let mut stream =
        std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to daemon");

    let req = format!(
        "PATCH /uploads/{id} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Upload-Offset: {offset}\r\nContent-Length: {declared}\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).expect("write request");
    stream.write_all(actually_sent).expect("write partial body");
    stream.flush().expect("flush");
    drop(stream);

    std::thread::sleep(Duration::from_millis(300));
}

fn parse_response(raw: &[u8]) -> (u16, Vec<(String, String)>, Vec<u8>) {
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

fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
}

fn open_upload(port: u16, path: &str, size: usize) -> String {
    let body = format!(r#"{{"path":"{path}","size":{size}}}"#);
    let (status, _, body) = request(
        port,
        "POST",
        "/uploads",
        &[("Content-Type", "application/json".to_string())],
        body.as_bytes(),
    );
    assert_eq!(status, 201, "POST /uploads should create a session");

    let text = String::from_utf8_lossy(&body);
    text.split(r#""id":""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("response carries an id")
        .to_string()
}

fn payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

#[test]
fn an_interrupted_upload_resumes_from_what_survived() {
    let vault = temp_vault("interrupted");
    let port = start_daemon(&vault);

    let data = payload(3 * 1024 * 1024);
    let id = open_upload(port, "Music/take 1.wav", data.len());

    let first = &data[..1024 * 1024];
    truncated_patch(port, &id, 0, first.len() + 512 * 1024, first);

    let (status, headers, _) = request(port, "HEAD", &format!("/uploads/{id}"), &[], &[]);
    assert_eq!(status, 200);
    let resumed_at: u64 = header(&headers, "upload-offset")
        .expect("HEAD reports an offset")
        .parse()
        .expect("offset is a number");

    assert!(
        resumed_at > 0,
        "an interrupted upload must keep the bytes that arrived, got offset 0"
    );
    assert!(
        resumed_at <= first.len() as u64,
        "server claimed more bytes ({resumed_at}) than were ever sent ({})",
        first.len()
    );

    let (status, headers, _) = request(
        port,
        "PATCH",
        &format!("/uploads/{id}"),
        &[("Upload-Offset", resumed_at.to_string())],
        &data[resumed_at as usize..],
    );

    assert_eq!(status, 201, "final chunk should complete the upload");
    assert_eq!(
        header(&headers, "upload-offset").as_deref(),
        Some(data.len().to_string().as_str())
    );

    let landed = std::fs::read(vault.join("Music/take 1.wav")).expect("file landed in vault");
    assert_eq!(landed.len(), data.len());
    assert_eq!(landed, data, "resumed upload must be byte-identical");

    let _ = std::fs::remove_dir_all(&vault);
}

#[test]
fn a_chunked_upload_lands_byte_identical() {
    let vault = temp_vault("chunked");
    let port = start_daemon(&vault);

    let data = payload(5 * 1024 * 1024);
    let id = open_upload(port, "big.wav", data.len());

    let chunk = 1024 * 1024;
    let mut offset = 0usize;

    while offset < data.len() {
        let end = (offset + chunk).min(data.len());
        let (status, headers, _) = request(
            port,
            "PATCH",
            &format!("/uploads/{id}"),
            &[("Upload-Offset", offset.to_string())],
            &data[offset..end],
        );

        if end == data.len() {
            assert_eq!(status, 201, "last chunk completes");
        } else {
            assert_eq!(status, 204, "intermediate chunk accepted");
        }

        offset = header(&headers, "upload-offset")
            .expect("every response reports the new offset")
            .parse()
            .expect("offset is a number");
    }

    assert_eq!(std::fs::read(vault.join("big.wav")).unwrap(), data);
    let _ = std::fs::remove_dir_all(&vault);
}

#[test]
fn a_wrong_offset_is_refused_and_reports_the_real_one() {
    let vault = temp_vault("bad-offset");
    let port = start_daemon(&vault);

    let data = payload(64 * 1024);
    let id = open_upload(port, "a.wav", data.len());

    request(
        port,
        "PATCH",
        &format!("/uploads/{id}"),
        &[("Upload-Offset", "0".to_string())],
        &data[..1024],
    );

    let (status, headers, _) = request(
        port,
        "PATCH",
        &format!("/uploads/{id}"),
        &[("Upload-Offset", "99999".to_string())],
        &data[1024..2048],
    );

    assert_eq!(status, 409, "a mismatched offset must not be appended");
    assert_eq!(
        header(&headers, "upload-offset").as_deref(),
        Some("1024"),
        "the refusal must carry the real offset so a client can resynchronise"
    );

    let _ = std::fs::remove_dir_all(&vault);
}

#[test]
fn an_upload_cannot_write_past_its_declared_size() {
    let vault = temp_vault("overflow");
    let port = start_daemon(&vault);

    let id = open_upload(port, "small.wav", 1024);
    let (status, _, _) = request(
        port,
        "PATCH",
        &format!("/uploads/{id}"),
        &[("Upload-Offset", "0".to_string())],
        &payload(64 * 1024),
    );

    assert_eq!(
        status, 413,
        "an oversized body must be refused with a status, not a dropped connection"
    );
    assert!(
        !vault.join("small.wav").exists(),
        "an oversized upload must never land in the vault"
    );

    let _ = std::fs::remove_dir_all(&vault);
}

#[test]
fn a_partial_upload_never_appears_in_the_vault() {
    let vault = temp_vault("no-partial");
    let port = start_daemon(&vault);

    let data = payload(2 * 1024 * 1024);
    let id = open_upload(port, "Music/pending.wav", data.len());

    request(
        port,
        "PATCH",
        &format!("/uploads/{id}"),
        &[("Upload-Offset", "0".to_string())],
        &data[..1024 * 1024],
    );

    assert!(
        !vault.join("Music/pending.wav").exists(),
        "a half-finished upload must not be visible under its real name"
    );
    assert!(
        vault.join(".uploads").exists(),
        "in-flight bytes live in the hidden uploads dir"
    );

    let _ = std::fs::remove_dir_all(&vault);
}

#[test]
fn opening_an_upload_over_an_existing_file_is_refused() {
    let vault = temp_vault("collision");
    std::fs::write(vault.join("taken.wav"), b"already here").unwrap();
    let port = start_daemon(&vault);

    let body = br#"{"path":"taken.wav","size":10}"#;
    let (status, _, _) = request(
        port,
        "POST",
        "/uploads",
        &[("Content-Type", "application/json".to_string())],
        body,
    );

    assert_eq!(status, 409, "collision is settled when the upload opens");
    assert_eq!(
        std::fs::read(vault.join("taken.wav")).unwrap(),
        b"already here"
    );

    let _ = std::fs::remove_dir_all(&vault);
}

#[test]
fn an_upload_path_cannot_escape_the_vault() {
    let vault = temp_vault("traversal");
    let port = start_daemon(&vault);

    let body = br#"{"path":"../escaped.wav","size":10}"#;
    let (status, _, _) = request(
        port,
        "POST",
        "/uploads",
        &[("Content-Type", "application/json".to_string())],
        body,
    );

    assert_eq!(status, 400);
    assert!(!vault
        .parent()
        .map(|p| p.join("escaped.wav").exists())
        .unwrap_or(false));

    let _ = std::fs::remove_dir_all(&vault);
}

#[test]
fn an_unknown_upload_id_is_not_found() {
    let vault = temp_vault("unknown");
    let port = start_daemon(&vault);

    let (status, _, _) = request(
        port,
        "HEAD",
        "/uploads/3f2504e0-4f89-11d3-9a0c-0305e82c3301",
        &[],
        &[],
    );
    assert_eq!(status, 404);

    let (status, _, _) = request(port, "HEAD", "/uploads/..%2F..%2Fetc", &[], &[]);
    assert_eq!(status, 404);

    let _ = std::fs::remove_dir_all(&vault);
}
