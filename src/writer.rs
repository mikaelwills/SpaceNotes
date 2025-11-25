use anyhow::{Context, Result};
use std::path::Path;

use crate::note::Note;

pub fn write_note_to_disk(vault_root: &Path, note: &Note) -> Result<()> {
    let file_path = vault_root.join(&note.path);

    // Security check (prevent writing outside vault)
    if !file_path.starts_with(vault_root) {
        anyhow::bail!("Security violation: Path {:?} is outside vault", note.path);
    }

    // Ensure parent folder exists
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Reconstruct YAML Frontmatter + Body
    // Always include spacetime_id in frontmatter
    let content = {
        // 1. Prepare JSON Object
        let mut json_val: serde_json::Value = if !note.frontmatter.is_empty() && note.frontmatter != "{}" {
            serde_json::from_str(&note.frontmatter).unwrap_or(serde_json::Value::Object(Default::default()))
        } else {
            serde_json::Value::Object(Default::default())
        };

        // 2. Force Insert UUID
        if let serde_json::Value::Object(ref mut map) = json_val {
            map.insert("spacetime_id".to_string(), serde_json::Value::String(note.id.clone()));
        }

        // 3. Serialize & Sanitize
        let yaml_str = serde_yaml::to_string(&json_val)
            .context("Failed to serialize frontmatter")?;

        // Strip existing markers to control the sandwich manually
        let clean_yaml = yaml_str.trim_start_matches("---\n").trim();

        // 4. Strict Formatting (The Fix)
        // Ensures exactly one newline before and after delimiters
        format!("---\n{}\n---\n\n{}", clean_yaml, note.content)
    };

    // ATOMIC WRITE (Write to tmp -> Rename)
    // This guarantees we never have a half-written file if the app crashes
    let tmp_path = file_path.with_extension("tmp");
    std::fs::write(&tmp_path, &content)?;
    std::fs::rename(&tmp_path, &file_path)?;

    // Sync Timestamp
    // Sets the file modification time to match the Server's time
    // This helps "Startup Reconciliation" logic significantly
    let mtime = filetime::FileTime::from_unix_time(
        (note.modified_time / 1000) as i64,
        ((note.modified_time % 1000) * 1_000_000) as u32,
    );
    let _ = filetime::set_file_mtime(&file_path, mtime);

    Ok(())
}
