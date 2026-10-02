use anyhow::Result;
use std::path::Path;
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

use crate::folder::Folder;
use crate::isolation::run_isolated;
use crate::space_file::SpaceFile;
use crate::sanitize::sanitize_path;

const TEXT_EXTENSIONS: [&str; 6] = ["md", "yaml", "yml", "json", "toml", "txt"];

const BINARY_EXTENSIONS: [&str; 19] = [
    "gpg",
    "mp3", "wav", "m4a", "aac", "flac", "ogg",
    "jpg", "jpeg", "png", "gif", "webp", "heic",
    "mp4", "mov", "m4v", "webm",
    "pdf", "csv",
];

/// Above this, a binary file's bytes are never read into `content` — the row
/// carries metadata only and the file is served separately (nginx `/files/`).
/// Below it, content is base64-encoded inline, same as `.gpg` today.
const INLINE_BINARY_MAX_BYTES: u64 = 20 * 1024;

fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
}

const TEXT_DOTFILES: [&str; 2] = [".gpg-id", ".gpg-pubkeys.asc"];

const ALLOWED_HIDDEN_DIRS: [&str; 1] = [".password-store"];

pub fn is_allowed_dotfile(name: &str) -> bool {
    TEXT_DOTFILES.contains(&name)
}

pub fn is_allowed_hidden_entry(name: &str) -> bool {
    TEXT_DOTFILES.contains(&name) || ALLOWED_HIDDEN_DIRS.contains(&name)
}

pub fn is_credential_store_path(rel_path: &str) -> bool {
    ALLOWED_HIDDEN_DIRS
        .iter()
        .any(|dir| rel_path == *dir || rel_path.starts_with(&format!("{dir}/")))
}

pub fn is_text(path: &Path) -> bool {
    let is_text_dotfile = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_allowed_dotfile);
    if is_text_dotfile {
        return true;
    }
    match extension_of(path) {
        Some(ext) => TEXT_EXTENSIONS.contains(&ext.as_str()),
        None => false,
    }
}

pub fn is_binary(path: &Path) -> bool {
    match extension_of(path) {
        Some(ext) => BINARY_EXTENSIONS.contains(&ext.as_str()),
        None => false,
    }
}

pub fn is_ingestible(path: &Path) -> bool {
    is_text(path) || is_binary(path)
}

pub fn encode_binary_content(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn decode_binary_content(content: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(content)
        .map_err(|e| anyhow::anyhow!("Content is not valid base64: {e}"))
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

    let metadata = std::fs::metadata(abs_path)?;
    let size = metadata.len();

    let content = if is_binary(abs_path) {
        if size >= INLINE_BINARY_MAX_BYTES {
            String::new()
        } else {
            let bytes = std::fs::read(abs_path)?;
            encode_binary_content(&bytes)
        }
    } else {
        let bytes = std::fs::read(abs_path)?;
        String::from_utf8(bytes).map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid UTF-8 (invalid byte at offset {}) - it is listed in TEXT_EXTENSIONS but is not a text file",
                rel_path,
                e.utf8_error().valid_up_to()
            )
        })?
    };
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

pub struct ScanOutcome<T> {
    pub found: Vec<T>,
    pub errors: usize,
}

fn vault_walker(vault_path: &Path) -> impl Iterator<Item = walkdir::Result<walkdir::DirEntry>> {
    // Optimization: filter_entry prevents descending into hidden directories
    WalkDir::new(vault_path).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        (!name.starts_with('.') || is_allowed_hidden_entry(&name)) && name != "@eaDir"
    })
}

pub fn scan_vault_files(vault_path: &Path) -> ScanOutcome<SpaceFile> {
    let mut files = Vec::new();
    let mut errors = 0;

    for entry in vault_walker(vault_path) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                errors += 1;
                tracing::warn!("Vault scan could not read an entry: {}", e);
                continue;
            }
        };
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

    ScanOutcome { found: files, errors }
}

pub fn scan_vault_folders(vault_path: &Path) -> Result<ScanOutcome<Folder>> {
    let mut folders = Vec::new();
    let mut errors = 0;

    for entry in vault_walker(vault_path) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                errors += 1;
                tracing::warn!("Vault folder scan could not read an entry: {}", e);
                continue;
            }
        };
        let path = entry.path();

        // Must be a directory, and must not be the root itself
        if !path.is_dir() || path == vault_path {
            continue;
        }

        // Get relative path - sanitize to prevent URI encoding issues
        let rel_path = sanitize_path(&path.strip_prefix(vault_path)?.to_string_lossy().to_string());

        folders.push(Folder::new(rel_path));
    }

    Ok(ScanOutcome { found: folders, errors })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_for_test(content: &str) -> Vec<u8> {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(content)
            .expect("row content must be valid base64")
    }

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

        let mut files = scan_vault_files(&vault).found;
        files.sort_by(|a, b| a.path.cmp(&b.path));

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].content, "body of a\n");
        assert!(files[0].id.is_empty());
        assert_eq!(files[0].extension, "md");

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn an_unreadable_subtree_is_reported_rather_than_silently_shortening_the_scan() {
        let vault = temp_vault("unreadable-subtree");
        std::fs::create_dir_all(vault.join("open")).unwrap();
        std::fs::create_dir_all(vault.join("closed")).unwrap();
        std::fs::write(vault.join("open/a.md"), "body\n").unwrap();
        std::fs::write(vault.join("closed/b.md"), "body\n").unwrap();

        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(vault.join("closed"), std::fs::Permissions::from_mode(0o000))
            .unwrap();

        let scan = scan_vault_files(&vault);

        std::fs::set_permissions(vault.join("closed"), std::fs::Permissions::from_mode(0o755))
            .unwrap();

        assert_eq!(scan.found.len(), 1, "only the readable subtree yields a file");
        assert!(
            scan.errors > 0,
            "an unreadable subtree must be counted, not dropped — a short list is otherwise indistinguishable from an empty vault"
        );

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_hidden_password_store_ingests_but_other_dot_directories_stay_pruned() {
        let vault = temp_vault("hiddenstore");
        std::fs::create_dir_all(vault.join(".password-store/site.com")).unwrap();
        std::fs::create_dir_all(vault.join(".git")).unwrap();
        std::fs::write(vault.join(".password-store/site.com/user.gpg"), [0x85u8, 0x02]).unwrap();
        std::fs::write(vault.join(".password-store/.gpg-id"), "ABC123!\n").unwrap();
        std::fs::write(
            vault.join(".password-store/.gpg-pubkeys.asc"),
            "-----BEGIN PGP PUBLIC KEY BLOCK-----\n",
        )
        .unwrap();
        std::fs::write(vault.join(".git/leak.md"), "must not ingest\n").unwrap();
        std::fs::write(vault.join("regular.md"), "note\n").unwrap();

        let files = scan_vault_files(&vault).found;
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();

        assert!(paths.iter().any(|p| p.ends_with("user.gpg")));
        assert!(paths.iter().any(|p| p.ends_with(".gpg-id")));
        assert!(
            paths.iter().any(|p| p.ends_with(".gpg-pubkeys.asc")),
            "the recipients' public keys must ingest with the store"
        );
        assert!(paths.contains(&"regular.md"));
        assert!(
            !paths.iter().any(|p| p.contains(".git")),
            "the allowlist must not open every dot-directory"
        );

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_pass_store_ingests_gpg_ciphertext_and_gpg_id_as_text() {
        let vault = temp_vault("passstore");
        std::fs::create_dir_all(vault.join("site.com")).unwrap();
        let ciphertext: &[u8] = &[0x85, 0x02, 0x0c, 0x03, 0xff, 0x00, 0xde, 0xad];
        std::fs::write(vault.join("site.com/user.gpg"), ciphertext).unwrap();
        std::fs::write(vault.join(".gpg-id"), "ABC123!\nDEF456!\n").unwrap();

        let mut files = scan_vault_files(&vault).found;
        files.sort_by(|a, b| a.path.cmp(&b.path));

        assert_eq!(files.len(), 2, "expected .gpg-id and the .gpg entry");

        let gpg_id = files.iter().find(|f| f.path == ".gpg-id").unwrap();
        assert_eq!(gpg_id.content, "ABC123!\nDEF456!\n");

        let entry = files.iter().find(|f| f.path.ends_with("user.gpg")).unwrap();
        assert_eq!(
            decode_for_test(&entry.content),
            ciphertext,
            "decoding the row content must reproduce the file bytes exactly"
        );
        assert_eq!(entry.extension, "gpg");
        assert_eq!(entry.size, ciphertext.len() as u64);

        assert_eq!(
            std::fs::read(vault.join("site.com/user.gpg")).unwrap(),
            ciphertext
        );

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn leftover_frontmatter_is_read_as_plain_content() {
        let vault = temp_vault("leftover");
        let content = "---\nspacetime_id: 11111111-1111-1111-1111-111111111111\n---\nbody\n";
        std::fs::write(vault.join("a.md"), content).unwrap();

        let files = scan_vault_files(&vault).found;

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
        std::fs::write(vault.join("ignored.exe"), "binary").unwrap();

        let mut files = scan_vault_files(&vault).found;
        files.sort_by(|a, b| a.path.cmp(&b.path));

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "config.yaml");
        assert_eq!(files[0].extension, "yaml");
        assert_eq!(files[0].content, "key: value\n");
        assert_eq!(files[1].extension, "json");

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn non_utf8_in_a_text_extension_errors_instead_of_ingesting_replacement_chars() {
        let vault = temp_vault("non-utf8");
        std::fs::write(vault.join("broken.md"), [0x68, 0x69, 0xFF, 0xFE]).unwrap();

        let err = read_file_at(&vault, &vault.join("broken.md")).unwrap_err();
        let msg = err.to_string();

        assert!(msg.contains("not valid UTF-8"), "unexpected error: {}", msg);
        assert!(msg.contains("offset 2"), "unexpected error: {}", msg);

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_file_outside_both_lists_is_not_ingested() {
        let vault = temp_vault("unlisted");
        let path = vault.join("app.exe");
        std::fs::write(&path, [0x52, 0x49, 0x46, 0x46, 0x00, 0xFF]).unwrap();

        assert!(!is_text(&path));
        assert!(!is_binary(&path));
        assert!(read_file_at(&vault, &path).unwrap().is_none());

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn credential_store_paths_are_recognised() {
        assert!(is_credential_store_path(".password-store"));
        assert!(is_credential_store_path(".password-store/github.com"));
        assert!(is_credential_store_path(".password-store/a/b/c"));
        assert!(!is_credential_store_path("All Notes"));
        assert!(!is_credential_store_path(".password-store-other"));
        assert!(!is_credential_store_path("notes/.password-store"));
    }

    #[test]
    fn a_small_binary_under_the_threshold_is_stored_inline() {
        let vault = temp_vault("small-binary");
        let path = vault.join("icon.png");
        let bytes = vec![0xFFu8; 100];
        std::fs::write(&path, &bytes).unwrap();

        let file = read_file_at(&vault, &path).unwrap().unwrap();

        assert!(!file.content.is_empty());
        assert_eq!(decode_binary_content(&file.content).unwrap(), bytes);
        assert_eq!(file.size, 100);

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_large_binary_at_or_over_the_threshold_stores_empty_content() {
        let vault = temp_vault("large-binary");
        let path = vault.join("clip.mp4");
        let bytes = vec![0xAAu8; INLINE_BINARY_MAX_BYTES as usize];
        std::fs::write(&path, &bytes).unwrap();

        let file = read_file_at(&vault, &path).unwrap().unwrap();

        assert_eq!(file.content, "");
        assert_eq!(file.size, INLINE_BINARY_MAX_BYTES);

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn newly_allowlisted_extensions_are_ingestible() {
        for ext in ["mp3", "jpg", "png", "mp4", "webm", "pdf", "csv"] {
            let vault = temp_vault(&format!("allowlist-{ext}"));
            let path = vault.join(format!("file.{ext}"));
            std::fs::write(&path, [0x00, 0x01]).unwrap();

            assert!(is_binary(&path), "{ext} should be recognised as binary");
            assert!(read_file_at(&vault, &path).unwrap().is_some());

            let _ = std::fs::remove_dir_all(&vault);
        }
    }
}
