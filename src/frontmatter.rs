use serde_json::Value;
use serde_yaml::Value as YamlValue;
use std::collections::BTreeMap;

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

/// Extracts the spacetime_id from frontmatter, if present
pub fn extract_spacetime_id(content: &str) -> Option<String> {
    if !content.starts_with("---") {
        return None;
    }

    let rest = &content[3..];
    let end_idx = rest.find("\n---")?;
    let yaml_str = rest[..end_idx].trim();

    let yaml_value: YamlValue = serde_yaml::from_str(yaml_str).ok()?;
    yaml_value.get("spacetime_id")?.as_str().map(String::from)
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

    format!("---\n{}---{}", yaml_out, body)
}
