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

    Ok(())
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
