use anyhow::Result;
use std::path::{Component, Path, PathBuf};

use crate::space_file::SpaceFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    SkippedBinary,
}

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

pub fn write_file_to_disk(vault_root: &Path, file: &SpaceFile) -> Result<WriteOutcome> {
    let file_path = resolve_vault_path(vault_root, &file.path)?;

    if crate::scanner::is_binary(&file_path) {
        tracing::debug!(
            "Skipping server->disk write for binary file {} (content is not stored in the database)",
            file.path
        );
        return Ok(WriteOutcome::SkippedBinary);
    }

    // Ensure parent folder exists
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // ATOMIC WRITE (Write to tmp -> Rename)
    // This guarantees we never have a half-written file if the app crashes
    let tmp_path = append_tmp_suffix(&file_path);
    std::fs::write(&tmp_path, &file.content)?;
    std::fs::rename(&tmp_path, &file_path)?;

    // Stamping the server's time on disk keeps startup reconciliation cheap. A row carrying
    // microseconds instead of milliseconds would land in the year 199 million here, and the
    // scanner would then read that back as truth — so an implausible value is left alone
    // rather than written into the filesystem.
    if let Some(mtime) = plausible_mtime(file.modified_time) {
        let _ = filetime::set_file_mtime(&file_path, mtime);
    } else {
        tracing::warn!(
            "Refusing to stamp implausible modified_time {} on {}",
            file.modified_time,
            file.path
        );
    }

    Ok(WriteOutcome::Written)
}

fn append_tmp_suffix(file_path: &Path) -> std::path::PathBuf {
    let mut name = file_path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    file_path.with_file_name(name)
}

const MAX_PLAUSIBLE_MS: u64 = 4_102_444_800_000;

fn plausible_mtime(modified_time_ms: u64) -> Option<filetime::FileTime> {
    if modified_time_ms == 0 || modified_time_ms > MAX_PLAUSIBLE_MS {
        return None;
    }
    Some(filetime::FileTime::from_unix_time(
        (modified_time_ms / 1000) as i64,
        ((modified_time_ms % 1000) * 1_000_000) as u32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plausible_mtime_accepts_a_real_millisecond_epoch() {
        let ms = 1_785_000_000_000u64;
        let mtime = plausible_mtime(ms).expect("should accept");
        assert_eq!(mtime.unix_seconds(), 1_785_000_000);
    }

    #[test]
    fn plausible_mtime_rejects_a_microsecond_value() {
        // 6299814578409652 is what a us-valued row produced: year 199 million on disk.
        assert!(plausible_mtime(6_299_814_578_409_652).is_none());
    }

    #[test]
    fn plausible_mtime_rejects_zero() {
        assert!(plausible_mtime(0).is_none());
    }

    #[test]
    fn plausible_mtime_splits_subsecond_millis_into_nanos() {
        let mtime = plausible_mtime(1_785_000_000_123).expect("should accept");
        assert_eq!(mtime.unix_seconds(), 1_785_000_000);
        assert_eq!(mtime.nanoseconds(), 123_000_000);
    }
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
    fn resolve_vault_path_rejects_traversal_absolute_and_empty_paths() {
        let vault = Path::new("/vault");
        assert!(resolve_vault_path(vault, "../a.md").is_err());
        assert!(resolve_vault_path(vault, "notes/../../a.md").is_err());
        assert!(resolve_vault_path(vault, "notes/../a.md").is_err());
        assert!(resolve_vault_path(vault, "/etc/passwd").is_err());
        assert!(resolve_vault_path(vault, "").is_err());
        assert!(resolve_vault_path(vault, ".").is_err());
    }

    #[test]
    fn a_row_path_with_traversal_never_writes_outside_the_vault() {
        let vault = temp_vault("traversal-write");
        let escape_target = vault.parent().unwrap().join("escaped-by-traversal.md");
        let _ = std::fs::remove_file(&escape_target);

        let file = SpaceFile::new(
            "33333333-3333-3333-3333-333333333333".to_string(),
            "../escaped-by-traversal.md".to_string(),
            "stolen".to_string(),
            6,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        let err = write_file_to_disk(&vault, &file).unwrap_err();
        assert!(err.to_string().contains("escapes the vault"));
        assert!(!escape_target.exists());

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn skipped_binary_write_reports_the_skip_instead_of_success() {
        let vault = temp_vault("skip-outcome");
        let file = SpaceFile::new(
            "44444444-4444-4444-4444-444444444444".to_string(),
            "site.com/user.gpg".to_string(),
            String::new(),
            8,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        let outcome = write_file_to_disk(&vault, &file).unwrap();

        assert_eq!(outcome, WriteOutcome::SkippedBinary);
        assert!(!vault.join("site.com/user.gpg").exists());

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_text_write_reports_written() {
        let vault = temp_vault("written-outcome");
        let file = SpaceFile::new(
            "55555555-5555-5555-5555-555555555555".to_string(),
            "a.md".to_string(),
            "body\n".to_string(),
            5,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        assert_eq!(
            write_file_to_disk(&vault, &file).unwrap(),
            WriteOutcome::Written
        );

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn binary_file_on_disk_is_never_overwritten_by_an_empty_row() {
        let vault = temp_vault("binary-guard");
        let ciphertext: &[u8] = &[0x85, 0x02, 0x0c, 0x03, 0xff, 0x00, 0xde, 0xad];
        std::fs::write(vault.join("secret.gpg"), ciphertext).unwrap();

        let file = SpaceFile::new(
            "22222222-2222-2222-2222-222222222222".to_string(),
            "secret.gpg".to_string(),
            String::new(),
            0,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        write_file_to_disk(&vault, &file).unwrap();

        let on_disk = std::fs::read(vault.join("secret.gpg")).unwrap();
        assert_eq!(on_disk, ciphertext);

        let _ = std::fs::remove_dir_all(&vault);
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
