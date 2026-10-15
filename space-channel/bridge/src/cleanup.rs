use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

const SERVER_NAME: &str = "space-channel";

pub fn run() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        eprintln!("cleanup: HOME is not set");
        return;
    };
    let mut removed = 0;
    removed += clean_settings(&home.join(".claude/settings.json"));
    for project in known_projects(&home) {
        removed += clean_mcp_json(&project.join(".mcp.json"));
        removed += clean_settings(&project.join(".claude/settings.json"));
        removed += clean_settings(&project.join(".claude/settings.local.json"));
    }
    println!("cleanup: removed {removed} stale space-channel entr{}", if removed == 1 { "y" } else { "ies" });
}

fn known_projects(home: &Path) -> Vec<PathBuf> {
    let Some(config) = read_json(&home.join(".claude.json")) else {
        return Vec::new();
    };
    let Some(projects) = config.get("projects").and_then(Value::as_object) else {
        return Vec::new();
    };
    let known: Vec<PathBuf> = projects.keys().map(PathBuf::from).collect();
    let mut seen = std::collections::HashSet::new();
    let mut dirs = Vec::new();
    let nearby = known
        .iter()
        .flat_map(|p| [Some(p.as_path()), p.parent()])
        .flatten()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|p| p.is_dir());
    for dir in known.iter().cloned().chain(nearby) {
        let key = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        if seen.insert(key) {
            dirs.push(dir);
        }
    }
    dirs
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_json(path: &Path, value: &Value) -> bool {
    let Ok(mut text) = serde_json::to_string_pretty(value) else {
        return false;
    };
    text.push('\n');
    let tmp = path.with_extension("json.space-channel-tmp");
    if std::fs::write(&tmp, text).is_err() {
        return false;
    }
    std::fs::rename(&tmp, path).is_ok()
}

fn clean_mcp_json(path: &Path) -> usize {
    let Some(mut doc) = read_json(path) else {
        return 0;
    };
    let Some(servers) = doc.get_mut("mcpServers").and_then(Value::as_object_mut) else {
        return 0;
    };
    if servers.remove(SERVER_NAME).is_none() {
        return 0;
    }
    let only_empty_servers = servers.is_empty() && doc.as_object().is_some_and(|o| o.len() == 1);
    let done = if only_empty_servers {
        std::fs::remove_file(path).is_ok()
    } else {
        write_json(path, &doc)
    };
    if !done {
        eprintln!("cleanup: could not update {}", path.display());
        return 0;
    }
    println!("cleanup: removed the {SERVER_NAME} MCP entry from {}", path.display());
    1
}

fn clean_settings(path: &Path) -> usize {
    let Some(mut doc) = read_json(path) else {
        return 0;
    };
    let Some(hooks) = doc.get_mut("hooks").and_then(Value::as_object_mut) else {
        return 0;
    };
    let removed = strip_hooks(hooks);
    if removed == 0 {
        return 0;
    }
    if !write_json(path, &doc) {
        eprintln!("cleanup: could not update {}", path.display());
        return 0;
    }
    println!("cleanup: removed {removed} {SERVER_NAME} hook(s) from {}", path.display());
    removed
}

fn strip_hooks(hooks: &mut Map<String, Value>) -> usize {
    let mut removed = 0;
    for groups in hooks.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            let Some(inner) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            let before = inner.len();
            inner.retain(|h| !is_space_channel_hook(h));
            removed += before - inner.len();
        }
        groups.retain(|g| g.get("hooks").and_then(Value::as_array).is_none_or(|h| !h.is_empty()));
    }
    hooks.retain(|_, groups| groups.as_array().is_none_or(|g| !g.is_empty()));
    removed
}

fn is_space_channel_hook(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains(SERVER_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sc-cleanup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn mcp_json_with_only_space_channel_is_deleted() {
        let dir = temp_dir("only");
        let path = dir.join(".mcp.json");
        std::fs::write(&path, json!({"mcpServers": {"space-channel": {"command": "x"}}}).to_string()).unwrap();
        assert_eq!(clean_mcp_json(&path), 1);
        assert!(!path.exists());
    }

    #[test]
    fn mcp_json_keeps_other_servers() {
        let dir = temp_dir("others");
        let path = dir.join(".mcp.json");
        std::fs::write(
            &path,
            json!({"mcpServers": {"space-channel": {"command": "x"}, "other": {"command": "y"}}}).to_string(),
        )
        .unwrap();
        assert_eq!(clean_mcp_json(&path), 1);
        let doc = read_json(&path).unwrap();
        assert_eq!(doc, json!({"mcpServers": {"other": {"command": "y"}}}));
    }

    #[test]
    fn mcp_json_without_space_channel_is_untouched() {
        let dir = temp_dir("none");
        let path = dir.join(".mcp.json");
        let original = "{\"mcpServers\":{}}";
        std::fs::write(&path, original).unwrap();
        assert_eq!(clean_mcp_json(&path), 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn settings_hooks_lose_only_space_channel_commands() {
        let mut hooks = json!({
            "PostToolUse": [
                {"matcher": "*", "hooks": [{"type": "command", "command": "space-channel hook-post"}]},
                {"matcher": "Edit", "hooks": [{"type": "command", "command": "fmt"}]}
            ],
            "Stop": [{"hooks": [{"type": "command", "command": "/x/space-channel hook-stop"}]}]
        });
        let removed = strip_hooks(hooks.as_object_mut().unwrap());
        assert_eq!(removed, 2);
        assert_eq!(
            hooks,
            json!({"PostToolUse": [{"matcher": "Edit", "hooks": [{"type": "command", "command": "fmt"}]}]})
        );
    }
}
