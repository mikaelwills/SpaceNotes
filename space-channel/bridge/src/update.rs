use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

const LOCK_STALE: Duration = Duration::from_secs(180);

fn share_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let plugin = exe.parent()?.parent()?;
    Some(plugin.parent()?.to_path_buf())
}

fn installed_version(share: &PathBuf) -> Option<String> {
    let output = Command::new(share.join("plugin/bin/space-channel")).arg("version").output().ok()?;
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn take_lock(share: &PathBuf) -> Option<PathBuf> {
    let lock = share.join(".updating");
    if let Ok(meta) = fs::metadata(&lock) {
        let age = meta.modified().ok().and_then(|m| SystemTime::now().duration_since(m).ok()).unwrap_or_default();
        if age < LOCK_STALE {
            return None;
        }
        let _ = fs::remove_dir(&lock);
    }
    fs::create_dir(&lock).ok()?;
    Some(lock)
}

const INSTALL_WAIT: Duration = Duration::from_secs(240);
const INSTALL_POLL: Duration = Duration::from_secs(2);

pub fn wait_installed(sha: &str) -> bool {
    let Some(share) = share_dir() else {
        return false;
    };
    let deadline = std::time::Instant::now() + INSTALL_WAIT;
    while std::time::Instant::now() < deadline {
        if installed_version(&share).as_deref() == Some(sha) && !share.join(".updating").exists() {
            return true;
        }
        std::thread::sleep(INSTALL_POLL);
    }
    false
}

pub fn install_release(sha: &str, install_url: &str, log: &dyn Fn(&str)) {
    let Some(share) = share_dir() else {
        log("update: cannot locate the plugin install directory");
        return;
    };
    if installed_version(&share).as_deref() == Some(sha) {
        return;
    }
    let Some(lock) = take_lock(&share) else {
        return;
    };
    log(&format!("update: release {sha} published, installing from {install_url}"));
    let script = format!("curl -fsSL '{install_url}' | sh");
    let status = Command::new("sh")
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = fs::remove_dir(&lock);
    match status {
        Ok(s) if s.success() => log(&format!("update: installed {sha}")),
        Ok(s) => log(&format!("update: installer exited with {s}")),
        Err(err) => log(&format!("update: installer failed to start: {err}")),
    }
}
