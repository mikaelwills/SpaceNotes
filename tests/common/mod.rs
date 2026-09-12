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
    start_daemon_on(vault, "127.0.0.1")
}

/// Binds `0.0.0.0` so a container can reach it, which the nginx proxy test
/// needs and a loopback-only listener cannot provide.
pub fn start_daemon_reachable(vault: &Path) -> Option<u16> {
    start_daemon_on(vault, "0.0.0.0")
}

fn start_daemon_on(vault: &Path, addr: &str) -> Option<u16> {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

    let rt = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    });

    let vault = vault.to_path_buf();
    let listener = rt
        .block_on(async { tokio::net::TcpListener::bind((addr, 0)).await })
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

pub fn docker_available() -> bool {
    std::process::Command::new("docker")
        .arg("info")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Boots nginx with the live `nginx-client.conf`, proxying to a daemon already
/// running on the host.
///
/// This is the only rig that exercises the deployed shape. Testing the daemon
/// directly hides anything the proxy layer gets wrong — a route nginx does not
/// forward answers 405 from nginx while every direct test still passes.
pub fn start_nginx_proxy(name: &str, daemon_port: u16) -> Option<u16> {
    let root = env!("CARGO_MANIFEST_DIR");
    let live_conf = std::fs::read_to_string(format!("{root}/nginx-client.conf")).ok()?;
    let proxied = live_conf.replace("127.0.0.1:5057", &format!("host.docker.internal:{daemon_port}"));

    let conf_path = std::env::temp_dir().join(format!("{name}-{}.conf", std::process::id()));
    std::fs::write(&conf_path, proxied).ok()?;

    let _ = std::process::Command::new("docker")
        .args(["rm", "-f", name])
        .output();

    let out = std::process::Command::new("docker")
        .args([
            "run", "-d", "--name", name,
            "-p", "0:80",
            "-v", &format!("{}:/etc/nginx/conf.d/default.conf:ro", conf_path.display()),
            "--add-host", "host.docker.internal:host-gateway",
            "nginx:1.26",
        ])
        .output()
        .ok()?;

    if !out.status.success() {
        eprintln!("docker run failed: {}", String::from_utf8_lossy(&out.stderr));
        return None;
    }

    let port = std::process::Command::new("docker")
        .args(["port", name, "80/tcp"])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()?
                .rsplit(':')
                .next()?
                .trim()
                .parse::<u16>()
                .ok()
        })?;

    for _ in 0..100 {
        if let Ok((status, _, _)) = request(port, "GET", "/files/", &[]) {
            if status != 0 {
                return Some(port);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

pub fn stop_nginx(name: &str) {
    let _ = std::process::Command::new("docker")
        .args(["rm", "-f", name])
        .output();
}

pub fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
}
