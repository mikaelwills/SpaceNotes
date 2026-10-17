use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn plugin_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("SPACE_CHANNEL_PLUGIN_DIR") {
        return Some(PathBuf::from(dir));
    }
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let bin_dir = exe.parent()?;
    if bin_dir.file_name()? == "bin" {
        return Some(bin_dir.parent()?.to_path_buf());
    }
    Some(bin_dir.join("plugin"))
}

const MARKETPLACE_PLUGIN: &str = "space-channel@mikaelsmods";

fn installed_from_marketplace() -> bool {
    let Ok(home) = std::env::var("HOME") else {
        return false;
    };
    let path = Path::new(&home).join(".claude/plugins/installed_plugins.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    json.get("plugins").and_then(|p| p.get(MARKETPLACE_PLUGIN)).is_some()
}

pub fn run(args: &[String]) {
    let Some(agent) = args.first() else {
        eprintln!("usage: space-channel launch <workflow> [args...]");
        std::process::exit(2);
    };
    let Some(plugin) = plugin_dir() else {
        eprintln!("space-channel plugin dir not found (set SPACE_CHANNEL_PLUGIN_DIR)");
        std::process::exit(1);
    };
    if !Path::new(&plugin).join(".claude-plugin/plugin.json").exists() {
        eprintln!("space-channel plugin not found at {} (set SPACE_CHANNEL_PLUGIN_DIR)", plugin.display());
        std::process::exit(1);
    }
    let stdb_uri = std::env::var("SPACE_CHANNEL_STDB_URI").unwrap_or_else(|_| crate::DEFAULT_STDB_URI.into());
    let stdb_db = std::env::var("SPACE_CHANNEL_STDB_DB").unwrap_or_else(|_| crate::DEFAULT_STDB_DB.into());
    eprintln!(
        "space-channel ready (agent: {agent}, plugin: {}, build: {})",
        plugin.display(),
        crate::BUILD_SHA
    );
    let mut claude = Command::new("claude");
    if !installed_from_marketplace() {
        claude.arg("--plugin-dir").arg(&plugin);
    }
    let err = claude
        .arg("--dangerously-skip-permissions")
        .arg(format!("/{agent}"))
        .args(&args[1..])
        .env("WORKFLOW_NAME", agent)
        .env("SPACE_CHANNEL_AGENT", agent)
        .env("SPACE_CHANNEL_STDB_URI", stdb_uri)
        .env("SPACE_CHANNEL_STDB_DB", stdb_db)
        .exec();
    eprintln!("failed to start claude: {err}");
    std::process::exit(1);
}
