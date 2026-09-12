//! Behavioural baseline for `/files/` and `/thumbnails/`.
//!
//! One expectation table, run against two targets:
//!   `FILES_TARGET=nginx` boots nginx in docker with `tests/fixtures/nginx-baseline.conf`
//!   `FILES_TARGET=daemon` runs the Rust server in-process
//!
//! The table is written against nginx FIRST, so it records what nginx actually
//! does rather than what the config appears to say. The daemon then has to
//! match it. Deliberate differences are `Expect::Either`, so every divergence
//! is visible in one place instead of silently accepted.
//!
//! The baseline config is a frozen copy of how `nginx-client.conf` served
//! these routes before the daemon took them over. The live config now proxies
//! to the daemon, so it can no longer describe the behaviour being matched.
//!
//! Run: `cargo test --test files_http -- --nocapture`
//! Skips (does not fail) when its target isn't reachable.

use std::process::Command;
use std::time::Duration;

mod common;

use common::{request, request_with_body, start_daemon};

const NGINX_IMAGE: &str = "nginx:1.26";

/// What a single header must look like.
#[derive(Debug, Clone)]
enum HeaderRule {
    Exact(&'static str, &'static str),
    Present(&'static str),
    Absent(&'static str),
}

/// Body assertion. `Any` is for cases where the body is irrelevant (error
/// pages differ between servers and the client never reads them).
#[derive(Debug, Clone)]
enum BodyRule {
    Empty,
    Len(usize),
    Any,
}

#[derive(Debug, Clone)]
struct Case {
    name: &'static str,
    method: &'static str,
    path: &'static str,
    headers: Vec<(&'static str, String)>,
    /// Some(status) — must match. The `either` field carries the known
    /// nginx-vs-ServeDir divergences.
    status: u16,
    either_status: Option<u16>,
    expect_headers: Vec<HeaderRule>,
    body: BodyRule,
}

impl Case {
    fn get(name: &'static str, path: &'static str, status: u16) -> Self {
        Case {
            name,
            method: "GET",
            path,
            headers: vec![],
            status,
            either_status: None,
            expect_headers: vec![],
            body: BodyRule::Any,
        }
    }

    fn method(mut self, m: &'static str) -> Self {
        self.method = m;
        self
    }

    fn header(mut self, k: &'static str, v: &str) -> Self {
        self.headers.push((k, v.to_string()));
        self
    }

    fn either(mut self, s: u16) -> Self {
        self.either_status = Some(s);
        self
    }

    fn expect(mut self, r: HeaderRule) -> Self {
        self.expect_headers.push(r);
        self
    }

    fn body(mut self, b: BodyRule) -> Self {
        self.body = b;
        self
    }
}

/// The baseline table. Sizes come from the fixture vault:
/// `sub/Heaper test.md` = 13 bytes, `big.bin` = 1 MiB.
fn cases() -> Vec<Case> {
    let big = 1_048_576usize;

    vec![
        // --- plain reads -------------------------------------------------
        Case::get("get_full", "/files/sub/Heaper%20test.md", 200)
            .expect(HeaderRule::Exact("content-length", "13"))
            .expect(HeaderRule::Present("last-modified"))
            .expect(HeaderRule::Exact("accept-ranges", "bytes"))
            .body(BodyRule::Len(13)),
        Case::get("head_full", "/files/sub/Heaper%20test.md", 200)
            .method("HEAD")
            .expect(HeaderRule::Exact("content-length", "13"))
            .body(BodyRule::Empty),
        Case::get("get_thumbnail", "/thumbnails/11111111-2222-3333-4444-555555555555.jpg", 200)
            .expect(HeaderRule::Exact("content-length", "512")),
        Case::get("get_unicode", "/files/sub/Caf%C3%A9%20%E2%80%94%20notes%20%F0%9F%8E%B5.md", 200)
            .body(BodyRule::Len(13)),

        // --- ranges ------------------------------------------------------
        Case::get("range_first_ten", "/files/big.bin", 206)
            .header("Range", "bytes=0-9")
            .expect(HeaderRule::Exact("content-length", "10"))
            .expect(HeaderRule::Exact(
                "content-range",
                "bytes 0-9/1048576",
            ))
            .body(BodyRule::Len(10)),
        Case::get("range_open_ended", "/files/big.bin", 206)
            .header("Range", "bytes=1048570-")
            .expect(HeaderRule::Exact("content-length", "6"))
            .body(BodyRule::Len(6)),
        Case::get("range_suffix", "/files/big.bin", 206)
            .header("Range", "bytes=-5")
            .expect(HeaderRule::Exact("content-length", "5"))
            .body(BodyRule::Len(5)),
        Case::get("range_single_byte", "/files/big.bin", 206)
            .header("Range", "bytes=0-0")
            .expect(HeaderRule::Exact("content-length", "1")),
        Case::get("range_past_eof_clamped", "/files/big.bin", 206)
            .header("Range", &format!("bytes=0-{}", big + 5000))
            .expect(HeaderRule::Exact("content-length", "1048576")),
        Case::get("range_start_at_size", "/files/big.bin", 416)
            .header("Range", &format!("bytes={}-", big))
            .expect(HeaderRule::Exact("content-range", "bytes */1048576")),
        Case::get("range_malformed", "/files/big.bin", 416)
            .header("Range", "bytes=abc")
            .expect(HeaderRule::Exact("content-range", "bytes */1048576")),
        Case::get("head_with_range", "/files/big.bin", 206)
            .method("HEAD")
            .header("Range", "bytes=0-9")
            .body(BodyRule::Empty),

        // --- known divergences (unused by the client) ---------------------
        // nginx answers multipart/byteranges; ServeDir refuses multi-range.
        Case::get("range_multi", "/files/big.bin", 206)
            .header("Range", "bytes=0-1,3-4")
            .either(416),
        // nginx ignores a non-bytes unit and serves the whole file.
        Case::get("range_bad_unit", "/files/big.bin", 200)
            .header("Range", "items=0-1")
            .either(416),
        // nginx 403s a directory inside an alias; ServeDir 404s.
        Case::get("dir_with_slash", "/files/sub/", 403).either(404),
        // nginx normalises traversal and 400s before the location matches.
        Case::get("traversal_encoded", "/files/..%2F..%2Fetc%2Fpasswd", 400).either(404),

        // --- misses -------------------------------------------------------
        Case::get("missing_file", "/files/sub/nope.md", 404),
        Case::get("missing_thumbnail", "/thumbnails/00000000-0000-0000-0000-000000000000.jpg", 404),

        // --- methods ------------------------------------------------------
        Case::get("delete_rejected", "/files/sub/Heaper%20test.md", 405).method("DELETE"),
        Case::get("post_rejected", "/files/sub/Heaper%20test.md", 405).method("POST"),
    ]
}

/// PUT cases are separate because each one writes, so they need a scratch
/// vault and assertions about what landed on disk.
struct PutCase {
    name: &'static str,
    path: &'static str,
    body: &'static str,
    headers: Vec<(&'static str, String)>,
    status: u16,
    /// Known, accepted daemon divergence from the nginx status.
    either_status: Option<u16>,
    /// Relative path that must exist afterwards with this exact content.
    lands_at: Option<(&'static str, &'static str)>,
}

fn put_cases() -> Vec<PutCase> {
    vec![
        PutCase {
            name: "put_new_file",
            path: "/files/put-new.md",
            body: "fresh\n",
            headers: vec![],
            status: 201,
            either_status: None,
            lands_at: Some(("put-new.md", "fresh\n")),
        },
        // DELIBERATE DIVERGENCE. nginx overwrites silently (204); the daemon
        // refuses (409). The spec says an upload never replaces an existing
        // file, and the daemon is the only party that can enforce that without
        // a stale view or a race — a client checking first sees only what its
        // subscription has hydrated, which pre-hydration is nothing.
        PutCase {
            name: "put_overwrite",
            path: "/files/sub/Heaper%20test.md",
            body: "replaced\n",
            headers: vec![],
            status: 204,
            either_status: Some(409),
            lands_at: Some(("sub/Heaper test.md", "replaced\n")),
        },
        PutCase {
            name: "put_overwrite_opt_in",
            path: "/files/sub/Heaper%20test.md",
            body: "deliberate\n",
            headers: vec![("Overwrite", "allow".to_string())],
            status: 204,
            either_status: None,
            lands_at: Some(("sub/Heaper test.md", "deliberate\n")),
        },
        PutCase {
            name: "put_nested_creates_dirs",
            path: "/files/deep/deeper/new.md",
            body: "nested\n",
            headers: vec![],
            status: 201,
            either_status: None,
            lands_at: Some(("deep/deeper/new.md", "nested\n")),
        },
        PutCase {
            name: "put_empty_body",
            path: "/files/empty.md",
            body: "",
            headers: vec![],
            status: 201,
            either_status: None,
            lands_at: Some(("empty.md", "")),
        },
        PutCase {
            name: "put_unicode_name",
            path: "/files/Caf%C3%A9%20%F0%9F%8E%B5.md",
            body: "unicode\n",
            headers: vec![],
            status: 201,
            either_status: None,
            lands_at: Some(("Café 🎵.md", "unicode\n")),
        },
        // THE blocker for resumable upload: nginx refuses a ranged PUT.
        // Flips to 2xx once the daemon owns this route.
        PutCase {
            name: "put_content_range_rejected",
            path: "/files/ranged.md",
            body: "partial",
            headers: vec![("Content-Range", "bytes 0-6/100".to_string())],
            status: 501,
            either_status: None,
            lands_at: None,
        },
        // A plain Range header is IGNORED, not rejected — full overwrite.
        PutCase {
            name: "put_range_header_ignored",
            path: "/files/rangeheader.md",
            body: "whole\n",
            headers: vec![("Range", "bytes=0-4".to_string())],
            status: 201,
            either_status: None,
            lands_at: Some(("rangeheader.md", "whole\n")),
        },
        PutCase {
            name: "put_to_directory_conflicts",
            path: "/files/sub/",
            body: "nope\n",
            headers: vec![],
            status: 409,
            either_status: None,
            lands_at: None,
        },
        // MEASURED, not what the config implies: nginx normalises the
        // traversal out of /files/, the request falls through to the SPA
        // `try_files ... /index.html` rule, and PUT on that hits a redirect
        // cycle → 500. Safe (nothing is written outside the vault — asserted
        // below) but accidental. The daemon should return a clean 404, so
        // this is an expected divergence rather than a behaviour to copy.
        PutCase {
            name: "put_traversal_blocked",
            path: "/files/..%2Fevil.md",
            body: "evil\n",
            headers: vec![],
            status: 500,
            either_status: Some(404),
            lands_at: None,
        },
    ]
}

fn docker_available() -> bool {
    Command::new("docker")
        .arg("info")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Boots the nginx baseline and waits for it to actually answer.
///
/// Readiness is a real HTTP response, not an open socket: docker publishes the
/// port before nginx is listening behind it, so a bare TCP connect succeeds
/// while requests still fail. That showed up as all 20 cases reporting status
/// 0 whenever both tests started a container at the same moment.
fn start_nginx(name: &str, vault: &str, conf: &str) -> Option<u16> {
    let _ = Command::new("docker").args(["rm", "-f", name]).output();

    // client_body_temp_path must exist before nginx writes a PUT body to it.
    let out = Command::new("docker")
        .args([
            "run", "-d", "--name", name,
            "-p", "0:80",
            "-v", &format!("{vault}:/vault"),
            "-v", &format!("{conf}:/etc/nginx/conf.d/default.conf:ro"),
            "--entrypoint", "sh",
            NGINX_IMAGE,
            "-c", "mkdir -p /tmp/nginx-dav-tmp && exec nginx -g 'daemon off;'",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        eprintln!("docker run failed: {}", String::from_utf8_lossy(&out.stderr));
        return None;
    }

    let port = Command::new("docker")
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

fn stop_nginx(name: &str) {
    let _ = Command::new("docker").args(["rm", "-f", name]).output();
}

#[test]
fn put_baseline_matches_expectations() {
    let target = std::env::var("FILES_TARGET").unwrap_or_else(|_| "nginx".into());
    if target == "nginx" && !docker_available() {
        eprintln!("SKIP: docker not available");
        return;
    }

    let root = env!("CARGO_MANIFEST_DIR");
    // Writes go to a scratch copy so the fixture vault stays pristine.
    // Unique per run: a shared scratch dir meant a re-run saw the previous
    // run's files and got 204 (overwrite) where 201 (created) was expected.
    let scratch = std::env::temp_dir().join(format!(
        "spacenotes-put-{target}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    copy_dir(
        std::path::Path::new(&format!("{root}/tests/fixtures/vault")),
        &scratch,
    )
    .expect("copy fixture vault");

    let name = "spacenotes-files-put";
    let port = if target == "nginx" {
        let conf = format!("{root}/tests/fixtures/nginx-baseline.conf");
        match start_nginx(name, &scratch.to_string_lossy(), &conf) {
            Some(port) => port,
            None => {
                stop_nginx(name);
                panic!("could not start nginx baseline container");
            }
        }
    } else {
        start_daemon(&scratch).expect("could not start daemon file server")
    };

    let mut failures = Vec::new();
    let mut transcript = Vec::new();

    for case in put_cases() {
        let result = request_with_body(port, "PUT", case.path, &case.headers, case.body.as_bytes());
        let Ok((status, _headers, _body)) = result else {
            failures.push(format!("{}: request failed: {:?}", case.name, result.err()));
            continue;
        };

        transcript.push(format!("{:<30} {:>3}", case.name, status));

        let status_ok = status == case.status || case.either_status == Some(status);
        if !status_ok {
            failures.push(format!(
                "{}: status {}, expected {}{}",
                case.name,
                status,
                case.status,
                case.either_status
                    .map(|s| format!(" (or {s})"))
                    .unwrap_or_default()
            ));
            continue;
        }

        if case.either_status == Some(status) && status != case.status {
            continue;
        }

        if let Some((rel, want)) = case.lands_at {
            match std::fs::read_to_string(scratch.join(rel)) {
                Ok(got) if got == want => {}
                Ok(got) => failures.push(format!(
                    "{}: {rel} contains {got:?}, expected {want:?}",
                    case.name
                )),
                Err(e) => failures.push(format!("{}: {rel} unreadable: {e}", case.name)),
            }
        }
    }

    // Nothing may have escaped the vault.
    if scratch.parent().map(|p| p.join("evil.md").exists()).unwrap_or(false) {
        failures.push("traversal PUT wrote outside the vault".to_string());
    }

    if target == "nginx" {
        stop_nginx(name);
    }
    let _ = std::fs::remove_dir_all(&scratch);

    println!("\n--- {target} PUT transcript ---");
    for line in &transcript {
        println!("{line}");
    }

    assert!(
        failures.is_empty(),
        "\n{} PUT baseline mismatches:\n  {}\n",
        failures.len(),
        failures.join("\n  ")
    );
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

#[test]
fn baseline_matches_expectations() {
    let target = std::env::var("FILES_TARGET").unwrap_or_else(|_| "nginx".into());
    let root = env!("CARGO_MANIFEST_DIR");
    let vault = format!("{root}/tests/fixtures/vault");
    let conf = format!("{root}/tests/fixtures/nginx-baseline.conf");
    let name = "spacenotes-files-get";

    let port = match target.as_str() {
        "daemon" => match start_daemon(std::path::Path::new(&vault)) {
            Some(p) => p,
            None => panic!("could not start daemon file server"),
        },
        _ => {
            if !docker_available() {
                eprintln!("SKIP: docker not available");
                return;
            }
            match start_nginx(name, &vault, &conf) {
                Some(p) => p,
                None => {
                    stop_nginx(name);
                    panic!("could not start nginx baseline container");
                }
            }
        }
    };

    let mut failures = Vec::new();
    let mut transcript = Vec::new();

    for case in cases() {
        let result = request(port, case.method, case.path, &case.headers);
        let Ok((status, headers, body)) = result else {
            failures.push(format!("{}: request failed: {:?}", case.name, result.err()));
            continue;
        };

        transcript.push(format!(
            "{:<24} {:>3} len={}",
            case.name,
            status,
            body.len()
        ));

        let status_ok = status == case.status || case.either_status == Some(status);
        if !status_ok {
            failures.push(format!(
                "{}: status {}, expected {}{}",
                case.name,
                status,
                case.status,
                case.either_status
                    .map(|e| format!(" or {e}"))
                    .unwrap_or_default(),
            ));
            continue;
        }

        // Header and body rules only apply on the primary (non-`either`) path;
        // a divergent status legitimately carries different headers.
        if status != case.status {
            continue;
        }

        for rule in &case.expect_headers {
            let found = |k: &str| headers.iter().find(|(hk, _)| hk == k).map(|(_, v)| v.clone());
            match rule {
                HeaderRule::Exact(k, want) => match found(k) {
                    Some(got) if got == *want => {}
                    Some(got) => failures
                        .push(format!("{}: header {k} = {got:?}, expected {want:?}", case.name)),
                    None => failures.push(format!("{}: header {k} missing", case.name)),
                },
                HeaderRule::Present(k) => {
                    if found(k).is_none() {
                        failures.push(format!("{}: header {k} missing", case.name));
                    }
                }
                HeaderRule::Absent(k) => {
                    if found(k).is_some() {
                        failures.push(format!("{}: header {k} should be absent", case.name));
                    }
                }
            }
        }

        match case.body {
            BodyRule::Empty if !body.is_empty() => {
                failures.push(format!("{}: body should be empty, got {}", case.name, body.len()))
            }
            BodyRule::Len(n) if body.len() != n => failures.push(format!(
                "{}: body {} bytes, expected {n}",
                case.name,
                body.len()
            )),
            _ => {}
        }
    }

    if target != "daemon" {
        stop_nginx(name);
    }

    println!("\n--- {target} transcript ---");
    for line in &transcript {
        println!("{line}");
    }

    assert!(
        failures.is_empty(),
        "\n{} baseline mismatches:\n  {}\n",
        failures.len(),
        failures.join("\n  ")
    );
}
