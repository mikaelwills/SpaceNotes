use serde_json::Value;
use serde_yaml::Value as YamlValue;
use std::collections::BTreeMap;
use regex::Regex;
use once_cell::sync::Lazy;

pub fn parse_frontmatter(content: &str) -> (String, String) {
    // Check for frontmatter start
    if !content.starts_with("---") {
        return (content.to_string(), "{}".to_string());
    }

    // Slice safely
    let rest = &content[3..];

    // Find the closing delimiter
    let Some(end_idx) = rest.find("\n---") else {
        return (content.to_string(), "{}".to_string());
    };

    let yaml_str = rest[..end_idx].trim();
    let body = rest[end_idx + 4..].trim_start();

    // Parse properly using serde_yaml
    let frontmatter = match serde_yaml::from_str::<Value>(yaml_str) {
        Ok(yaml_value) => {
            serde_json::to_string(&yaml_value).unwrap_or_else(|_| "{}".to_string())
        }
        Err(_) => {
            tracing::warn!("Malformed frontmatter found");
            "{}".to_string()
        }
    };

    (body.to_string(), frontmatter)
}

// Compile regex once
static SPACETIME_ID_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^spacetime_id:\s*([a-f0-9\-]+)").unwrap()
});

/// Extracts the spacetime_id from frontmatter, if present
/// Uses a hybrid approach: strict YAML parsing first, then regex fallback
pub fn extract_spacetime_id(content: &str) -> Option<String> {
    // STRATEGY 1: Strict YAML Parsing (Preferred)
    if content.starts_with("---") {
        if let Some(end_idx) = content[3..].find("\n---") {
            let yaml_str = &content[3..end_idx + 3];
            if let Ok(json) = serde_yaml::from_str::<Value>(yaml_str) {
                if let Some(id) = json.get("spacetime_id").and_then(|v| v.as_str()) {
                    return Some(id.to_string());
                }
            }
        }
    }

    // STRATEGY 2: Loose Regex Fallback (The Safety Net)
    // If YAML fails (e.g. user added a tab), check raw text so we don't double-inject.
    // Scan first 1KB only.
    let head = if content.len() > 1024 { &content[..1024] } else { content };
    if let Some(caps) = SPACETIME_ID_REGEX.captures(head) {
        let id = caps.get(1).unwrap().as_str().trim().to_string();
        tracing::warn!("Extracted ID via Regex (YAML malformed): {}", id);
        return Some(id);
    }

    None
}

/// Injects or updates spacetime_id in the frontmatter
/// Returns the modified content
pub fn inject_spacetime_id(content: &str, id: &str) -> String {
    // Case 1: No frontmatter exists - create one
    if !content.starts_with("---") {
        return format!("---\nspacetime_id: {}\n---\n\n{}", id, content);
    }

    // Case 2: Frontmatter exists - parse and inject ID
    let rest = &content[3..];
    let Some(end_idx) = rest.find("\n---") else {
        // Malformed frontmatter - treat as no frontmatter
        return format!("---\nspacetime_id: {}\n---\n\n{}", id, content);
    };

    let yaml_str = rest[..end_idx].trim();
    let body = &rest[end_idx + 4..];

    // Parse existing frontmatter
    let mut yaml_map: BTreeMap<String, YamlValue> = match serde_yaml::from_str(yaml_str) {
        Ok(YamlValue::Mapping(map)) => {
            map.into_iter()
                .filter_map(|(k, v)| {
                    k.as_str().map(|s| (s.to_string(), v))
                })
                .collect()
        }
        _ => BTreeMap::new(),
    };

    // Insert/update spacetime_id
    yaml_map.insert("spacetime_id".to_string(), YamlValue::String(id.to_string()));

    // Serialize back to YAML
    let yaml_out = serde_yaml::to_string(&yaml_map).unwrap_or_default();

    format!("---\n{}\n---{}", yaml_out, body)
}
