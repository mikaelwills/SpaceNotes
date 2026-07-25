use anyhow::Result;
use std::path::Path;

use crate::space_file::SpaceFile;

pub fn write_file_to_disk(vault_root: &Path, file: &SpaceFile) -> Result<()> {
    let file_path = vault_root.join(&file.path);

    // Security check (prevent writing outside vault)
    if !file_path.starts_with(vault_root) {
        anyhow::bail!("Security violation: Path {:?} is outside vault", file.path);
    }

    // Ensure parent folder exists
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // ATOMIC WRITE (Write to tmp -> Rename)
    // This guarantees we never have a half-written file if the app crashes
    let tmp_path = file_path.with_extension("tmp");
    std::fs::write(&tmp_path, &file.content)?;
    std::fs::rename(&tmp_path, &file_path)?;

    // Sync Timestamp
    // Sets the file modification time to match the Server's time
    // This helps "Startup Reconciliation" logic significantly
    let mtime = filetime::FileTime::from_unix_time(
        (file.modified_time / 1000) as i64,
        ((file.modified_time % 1000) * 1_000_000) as u32,
    );
    let _ = filetime::set_file_mtime(&file_path, mtime);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_vault(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "spacenotes-writer-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn download_writes_content_verbatim() {
        let vault = temp_vault("verbatim");
        let file = SpaceFile::new(
            "11111111-1111-1111-1111-111111111111".to_string(),
            "a.md".to_string(),
            "verbatim body\nno identity injected\n".to_string(),
            36,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        write_file_to_disk(&vault, &file).unwrap();

        let on_disk = std::fs::read_to_string(vault.join("a.md")).unwrap();
        assert_eq!(on_disk, file.content);
        let metadata = std::fs::metadata(vault.join("a.md")).unwrap();
        let mtime = filetime::FileTime::from_last_modification_time(&metadata);
        assert_eq!(mtime.unix_seconds(), 1_600_000_000);

        let _ = std::fs::remove_dir_all(&vault);
    }
}
