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
        // Parse existing frontmatter or create empty object
        let mut json_val: serde_json::Value = if !note.frontmatter.is_empty() && note.frontmatter != "{}" {
            serde_json::from_str(&note.frontmatter).unwrap_or(serde_json::Value::Object(Default::default()))
        } else {
            serde_json::Value::Object(Default::default())
        };

        // Ensure spacetime_id is in the frontmatter
        if let serde_json::Value::Object(ref mut map) = json_val {
            map.insert("spacetime_id".to_string(), serde_json::Value::String(note.id.clone()));
        }

        let yaml_str = serde_yaml::to_string(&json_val)
            .context("Failed to convert frontmatter to YAML")?;

        // serde_yaml adds "---" at the start, we just need to close it
        format!("{}\n---\n{}", yaml_str, note.content)
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
