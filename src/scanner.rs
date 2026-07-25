use anyhow::Result;
use std::path::Path;
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

use crate::folder::Folder;
use crate::isolation::run_isolated;
use crate::space_file::SpaceFile;
use crate::sanitize::sanitize_path;

const INGEST_EXTENSIONS: [&str; 6] = ["md", "yaml", "yml", "json", "toml", "txt"];

pub fn is_ingestible(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => INGEST_EXTENSIONS.contains(&ext.to_lowercase().as_str()),
        None => false,
    }
}

pub fn read_file_at(vault_path: &Path, abs_path: &Path) -> Result<Option<SpaceFile>> {
    // Validation
    if !abs_path.exists() || !abs_path.is_file() {
        return Ok(None);
    }

    if !is_ingestible(abs_path) {
        return Ok(None);
    }

    // Relative path - sanitize to prevent URI encoding issues
    let rel_path = sanitize_path(&abs_path
        .strip_prefix(vault_path)?
        .to_string_lossy()
        .to_string());

    let bytes = std::fs::read(abs_path)?;
    let content = String::from_utf8_lossy(&bytes).into_owned();

    let metadata = std::fs::metadata(abs_path)?;

    let size = metadata.len();
    let modified = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)?
        .as_millis() as u64;
    let created = metadata
        .created()
        .unwrap_or_else(|_| metadata.modified().unwrap())
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(modified);

    Ok(Some(SpaceFile::new(String::new(), rel_path, content, size, created, modified)))
}

pub fn scan_files(vault_path: &Path) -> Result<Vec<SpaceFile>> {
    let mut files = Vec::new();

    // Optimization: filter_entry prevents descending into hidden directories
    let walker = WalkDir::new(vault_path).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        !name.starts_with('.') && name != "@eaDir"
    });

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();

        if !path.is_file() || !is_ingestible(path) {
            continue;
        }

        let context = format!("scan file {:?}", path);
        run_isolated(context, || {
            match read_file_at(vault_path, path) {
                Ok(Some(file)) => files.push(file),
                Ok(None) => {}
                Err(e) => tracing::warn!("Failed to read {:?}: {}", path, e),
            }
        });
    }

    Ok(files)
}

pub fn scan_folders(vault_path: &Path) -> Result<Vec<Folder>> {
    let mut folders = Vec::new();

    // Optimization: filter_entry prevents descending into hidden directories
    let walker = WalkDir::new(vault_path).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        !name.starts_with('.') && name != "@eaDir"
    });

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();

        // Must be a directory, and must not be the root itself
        if !path.is_dir() || path == vault_path {
            continue;
        }

        // Get relative path - sanitize to prevent URI encoding issues
        let rel_path = sanitize_path(&path.strip_prefix(vault_path)?.to_string_lossy().to_string());

        folders.push(Folder::new(rel_path));
    }

    Ok(folders)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_vault(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("spacenotes-scan-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scan_returns_all_files_with_verbatim_content() {
        let vault = temp_vault("verbatim");
        std::fs::write(vault.join("a.md"), "body of a\n").unwrap();
        std::fs::write(vault.join("b.md"), "body of b\n").unwrap();

        let mut files = scan_files(&vault).unwrap();
        files.sort_by(|a, b| a.path.cmp(&b.path));

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].content, "body of a\n");
        assert!(files[0].id.is_empty());
        assert_eq!(files[0].extension, "md");

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn leftover_frontmatter_is_read_as_plain_content() {
        let vault = temp_vault("leftover");
        let content = "---\nspacetime_id: 11111111-1111-1111-1111-111111111111\n---\nbody\n";
        std::fs::write(vault.join("a.md"), content).unwrap();

        let files = scan_files(&vault).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].content, content);
        assert!(files[0].id.is_empty());

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn scan_ingests_non_md() {
        let vault = temp_vault("non-md");
        std::fs::write(vault.join("config.yaml"), "key: value\n").unwrap();
        std::fs::write(vault.join("data.json"), "{}\n").unwrap();
        std::fs::write(vault.join("ignored.png"), "binary").unwrap();

        let mut files = scan_files(&vault).unwrap();
        files.sort_by(|a, b| a.path.cmp(&b.path));

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "config.yaml");
        assert_eq!(files[0].extension, "yaml");
        assert_eq!(files[0].content, "key: value\n");
        assert_eq!(files[1].extension, "json");

        let _ = std::fs::remove_dir_all(&vault);
    }
}
