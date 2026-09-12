//! Resolving a vault-relative path to somewhere on disk.
//!
//! A leaf module on purpose: both the SpacetimeDB writer and the file HTTP
//! server need this check, and the server must not drag the scanner, folder
//! and isolation modules into the library surface just to reach it.

use anyhow::Result;
use std::path::{Component, Path, PathBuf};

/// Joins a relative path onto the vault root, rejecting anything that would
/// land outside it or name the root itself.
pub fn resolve_vault_path(vault_root: &Path, rel: &str) -> Result<PathBuf> {
    let mut resolved = vault_root.to_path_buf();
    let mut depth = 0usize;
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(part) => {
                resolved.push(part);
                depth += 1;
            }
            Component::CurDir => {}
            _ => anyhow::bail!("Security violation: Path {:?} escapes the vault", rel),
        }
    }
    if depth == 0 {
        anyhow::bail!("Security violation: Path {:?} resolves to the vault root", rel);
    }
    Ok(resolved)
}

/// The scratch name a file is written under before being renamed into place.
///
/// Appends rather than replacing the extension, so `a.md` and `a.txt` cannot
/// collide on a shared `a.tmp`.
pub fn append_tmp_suffix(file_path: &Path) -> PathBuf {
    let mut name = file_path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    file_path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_vault_path_joins_normal_relative_paths() {
        let vault = Path::new("/vault");
        assert_eq!(
            resolve_vault_path(vault, "notes/a.md").unwrap(),
            Path::new("/vault/notes/a.md")
        );
        assert_eq!(
            resolve_vault_path(vault, "notes/./a.md").unwrap(),
            Path::new("/vault/notes/a.md")
        );
        assert_eq!(
            resolve_vault_path(vault, ".gpg-id").unwrap(),
            Path::new("/vault/.gpg-id")
        );
    }

    #[test]
    fn temp_name_appends_so_siblings_sharing_a_stem_cannot_collide() {
        assert_eq!(
            append_tmp_suffix(Path::new("/v/a.md")),
            Path::new("/v/a.md.tmp")
        );
        assert_ne!(
            append_tmp_suffix(Path::new("/v/a.md")),
            append_tmp_suffix(Path::new("/v/a.txt"))
        );
        assert_eq!(
            append_tmp_suffix(Path::new("/v/.gpg-id")),
            Path::new("/v/.gpg-id.tmp")
        );
    }

    #[test]
    fn resolve_vault_path_rejects_traversal_absolute_and_empty_paths() {
        let vault = Path::new("/vault");
        assert!(resolve_vault_path(vault, "../a.md").is_err());
        assert!(resolve_vault_path(vault, "notes/../../a.md").is_err());
        assert!(resolve_vault_path(vault, "notes/../a.md").is_err());
        assert!(resolve_vault_path(vault, "/etc/passwd").is_err());
        assert!(resolve_vault_path(vault, "").is_err());
        assert!(resolve_vault_path(vault, ".").is_err());
    }
}
