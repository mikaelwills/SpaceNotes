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
    let after_marker = &rest[end_idx + 4..];
    let body = after_marker
        .strip_prefix("\n\n")
        .or_else(|| after_marker.strip_prefix("\n"))
        .unwrap_or(after_marker);

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
    let mut head_end = content.len().min(1024);
    while !content.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let head = &content[..head_end];
    if let Some(caps) = SPACETIME_ID_REGEX.captures(head) {
        let id = caps.get(1).unwrap().as_str().trim().to_string();
        tracing::warn!("Extracted ID via Regex (YAML malformed): {}", id);
        return Some(id);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn straddling_content() -> String {
        let prefix = "---\ntitle: Migration Note\ntags: [a, b]\n---\n\n";
        let em_dash = "—";
        let em_dash_start = 1022;
        let pad_before = em_dash_start - prefix.len();
        let mut content = String::new();
        content.push_str(prefix);
        content.push_str(&"x".repeat(pad_before));
        content.push_str(em_dash);
        content.push_str(&"y".repeat(500));
        assert_eq!(content.as_bytes()[em_dash_start], 0xE2);
        assert!(!content.is_char_boundary(1024));
        assert!(content.len() > 1024);
        content
    }

    #[test]
    fn multibyte_straddling_1024_returns_none_not_panic() {
        let content = straddling_content();
        assert_eq!(extract_spacetime_id(&content), None);
    }

    #[test]
    fn strategy_1_yaml_spacetime_id_extracted() {
        let id = "abc123-def456";
        let prefix = format!("---\nspacetime_id: {}\ntitle: Note\n---\n\n", id);
        let content = format!("{}{}—{}", prefix, "x".repeat(1200), "y".repeat(200));
        assert!(content.len() > 1024);
        assert_eq!(extract_spacetime_id(&content), Some(id.to_string()));
    }

    #[test]
    fn straddle_does_not_widen_scan_to_body_id() {
        let prefix = "---\n\ttitle: bad tab\n---\n\n";
        let em_dash = "—";
        let em_dash_start = 1022;
        let pad_before = em_dash_start - prefix.len();
        let mut content = String::new();
        content.push_str(prefix);
        content.push_str(&"x".repeat(pad_before));
        content.push_str(em_dash);
        content.push_str("\nlater in the body someone wrote:\nspacetime_id: deadbeef\n");
        assert!(!content.is_char_boundary(1024));
        assert!(content.len() > 1024);
        assert_eq!(extract_spacetime_id(&content), None);
    }

    #[test]
    fn strategy_2_regex_fallback_extracted() {
        let id = "0a1b2c3d-4e5f";
        let content = format!(
            "---\n\ttitle: bad tab indent\nspacetime_id: {}\n---\n\nbody text here",
            id
        );
        assert_eq!(extract_spacetime_id(&content), Some(id.to_string()));
    }
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
