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

pub fn credential_write_is_valid(vault_root: &Path, file: &SpaceFile) -> bool {
    match validate_credential_write(vault_root, file) {
        Ok(_) => true,
        Err(reason) => {
            tracing::warn!(
                "Refusing credential write for {}: {}",
                file.path,
                reason
            );
            false
        }
    }
}

fn validate_credential_write(vault_root: &Path, file: &SpaceFile) -> Result<Vec<u8>> {
    if !crate::scanner::is_credential_store_path(&file.path) {
        anyhow::bail!("Refusing credential write outside the credential store");
    }

    let bytes = crate::scanner::decode_binary_content(&file.content)?;
    if bytes.is_empty() {
        anyhow::bail!("Refusing credential write with empty content");
    }

    let mut found = crate::pgp_probe::recipient_key_ids(&bytes)?;
    found.sort();

    let gpg_id_path = vault_root.join(".password-store").join(".gpg-id");
    let gpg_id = std::fs::read_to_string(&gpg_id_path)
        .map_err(|e| anyhow::anyhow!("Cannot read {}: {e}", gpg_id_path.display()))?;
    let mut wanted = crate::pgp_probe::parse_gpg_id(&gpg_id);
    if wanted.is_empty() {
        anyhow::bail!("Refusing credential write: .gpg-id names no recipients");
    }
    wanted.sort();

    if found != wanted {
        anyhow::bail!(
            "Refusing credential write: recipients {:?} do not match .gpg-id {:?}",
            found,
            wanted
        );
    }

    Ok(bytes)
}

pub fn write_file_to_disk(vault_root: &Path, file: &SpaceFile) -> Result<WriteOutcome> {
    let file_path = resolve_vault_path(vault_root, &file.path)?;

    let payload: std::borrow::Cow<'_, [u8]> = if crate::scanner::is_binary(&file_path) {
        match validate_credential_write(vault_root, file) {
            Ok(bytes) => std::borrow::Cow::Owned(bytes),
            Err(reason) => {
                tracing::debug!(
                    "Skipping server->disk write for binary file {}: {}",
                    file.path,
                    reason
                );
                return Ok(WriteOutcome::SkippedBinary);
            }
        }
    } else {
        std::borrow::Cow::Borrowed(file.content.as_bytes())
    };

    // Ensure parent folder exists
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // ATOMIC WRITE (Write to tmp -> Rename)
    // This guarantees we never have a half-written file if the app crashes
    let tmp_path = append_tmp_suffix(&file_path);
    write_and_sync(&tmp_path, &payload)?;
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

fn write_and_sync(tmp_path: &Path, payload: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut handle = std::fs::File::create(tmp_path)?;
    handle.write_all(payload)?;
    handle.sync_all()?;
    Ok(())
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
pub(crate) mod tests {
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
        seed_store(&vault);
        let file = SpaceFile::new(
            "44444444-4444-4444-4444-444444444444".to_string(),
            ".password-store/site.com/user.gpg".to_string(),
            String::new(),
            8,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        let outcome = write_file_to_disk(&vault, &file).unwrap();

        assert_eq!(outcome, WriteOutcome::SkippedBinary);
        assert!(!vault
            .join(".password-store/site.com/user.gpg")
            .exists());

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
        seed_store(&vault);
        let rel = ".password-store/site.com/user.gpg";
        let ciphertext = valid_ciphertext();
        std::fs::create_dir_all(vault.join(".password-store/site.com")).unwrap();
        std::fs::write(vault.join(rel), &ciphertext).unwrap();

        let file = SpaceFile::new(
            "22222222-2222-2222-2222-222222222222".to_string(),
            rel.to_string(),
            String::new(),
            0,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        let outcome = write_file_to_disk(&vault, &file).unwrap();

        assert_eq!(outcome, WriteOutcome::SkippedBinary);
        let on_disk = std::fs::read(vault.join(rel)).unwrap();
        assert_eq!(on_disk, ciphertext);

        let _ = std::fs::remove_dir_all(&vault);
    }

    pub(crate) fn store_gpg_id() -> &'static str {
        "C582F8C66A659D51!\n633FB31FF42971F1!\n4CC2B0682D695565!\n29F11110A0624877!\n"
    }

    pub(crate) fn seed_store(vault: &Path) {
        std::fs::create_dir_all(vault.join(".password-store")).unwrap();
        std::fs::write(vault.join(".password-store/.gpg-id"), store_gpg_id()).unwrap();
    }

    pub(crate) fn pkesk_packet(key_id: [u8; 8]) -> Vec<u8> {
        let mut packet = vec![0xc1, 0x0c, 0x03];
        packet.extend_from_slice(&key_id);
        packet.extend_from_slice(&[0x12, 0x00, 0x00]);
        packet
    }

    pub(crate) fn valid_ciphertext() -> Vec<u8> {
        let ids: [[u8; 8]; 4] = [
            [0xC5, 0x82, 0xF8, 0xC6, 0x6A, 0x65, 0x9D, 0x51],
            [0x63, 0x3F, 0xB3, 0x1F, 0xF4, 0x29, 0x71, 0xF1],
            [0x4C, 0xC2, 0xB0, 0x68, 0x2D, 0x69, 0x55, 0x65],
            [0x29, 0xF1, 0x11, 0x10, 0xA0, 0x62, 0x48, 0x77],
        ];
        let mut bytes = Vec::new();
        for id in ids {
            bytes.extend_from_slice(&pkesk_packet(id));
        }
        bytes.extend_from_slice(&[0xd2, 0x03, 0x01, 0x00, 0x00]);
        bytes
    }

    pub(crate) fn valid_row_content() -> String {
        crate::scanner::encode_binary_content(&valid_ciphertext())
    }

    #[test]
    fn a_valid_in_store_credential_is_written_to_disk() {
        let vault = temp_vault("cred-write-happy");
        seed_store(&vault);
        let rel = ".password-store/novel.example.com/user.gpg";

        let file = SpaceFile::new(
            "aaaaaaaa-0000-0000-0000-000000000001".to_string(),
            rel.to_string(),
            valid_row_content(),
            0,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        let outcome = write_file_to_disk(&vault, &file).unwrap();

        assert_eq!(outcome, WriteOutcome::Written);
        let on_disk = std::fs::read(vault.join(rel)).unwrap();
        assert_eq!(on_disk, valid_ciphertext());

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_credential_write_creates_a_missing_host_directory() {
        let vault = temp_vault("cred-write-mkdir");
        seed_store(&vault);
        let rel = ".password-store/brand.new.host/user.gpg";
        assert!(!vault.join(".password-store/brand.new.host").exists());

        let file = SpaceFile::new(
            "aaaaaaaa-0000-0000-0000-000000000002".to_string(),
            rel.to_string(),
            valid_row_content(),
            0,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        assert_eq!(
            write_file_to_disk(&vault, &file).unwrap(),
            WriteOutcome::Written
        );
        assert!(vault.join(".password-store/brand.new.host").is_dir());
        assert_eq!(
            std::fs::read(vault.join(rel)).unwrap(),
            valid_ciphertext()
        );

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_credential_row_with_undecodable_base64_leaves_the_file_untouched() {
        let vault = temp_vault("cred-write-badb64");
        seed_store(&vault);
        let rel = ".password-store/site.com/user.gpg";
        let existing = valid_ciphertext();
        std::fs::create_dir_all(vault.join(".password-store/site.com")).unwrap();
        std::fs::write(vault.join(rel), &existing).unwrap();

        let file = SpaceFile::new(
            "aaaaaaaa-0000-0000-0000-000000000003".to_string(),
            rel.to_string(),
            "!!!! not base64 !!!!".to_string(),
            0,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        assert_eq!(
            write_file_to_disk(&vault, &file).unwrap(),
            WriteOutcome::SkippedBinary
        );
        assert_eq!(std::fs::read(vault.join(rel)).unwrap(), existing);

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_credential_row_that_is_not_an_openpgp_message_leaves_the_file_untouched() {
        let vault = temp_vault("cred-write-notpgp");
        seed_store(&vault);
        let rel = ".password-store/site.com/user.gpg";
        let existing = valid_ciphertext();
        std::fs::create_dir_all(vault.join(".password-store/site.com")).unwrap();
        std::fs::write(vault.join(rel), &existing).unwrap();

        let file = SpaceFile::new(
            "aaaaaaaa-0000-0000-0000-000000000004".to_string(),
            rel.to_string(),
            crate::scanner::encode_binary_content(b"this is just plain text"),
            0,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        assert_eq!(
            write_file_to_disk(&vault, &file).unwrap(),
            WriteOutcome::SkippedBinary
        );
        assert_eq!(std::fs::read(vault.join(rel)).unwrap(), existing);

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_credential_row_whose_recipients_do_not_match_gpg_id_leaves_the_file_untouched() {
        let vault = temp_vault("cred-write-mismatch");
        seed_store(&vault);
        let rel = ".password-store/site.com/user.gpg";
        let existing = valid_ciphertext();
        std::fs::create_dir_all(vault.join(".password-store/site.com")).unwrap();
        std::fs::write(vault.join(rel), &existing).unwrap();

        let mut wrong = pkesk_packet([0xDE, 0xAD, 0xBE, 0xEF, 0xDE, 0xAD, 0xBE, 0xEF]);
        wrong.extend_from_slice(&[0xd2, 0x03, 0x01, 0x00, 0x00]);

        let file = SpaceFile::new(
            "aaaaaaaa-0000-0000-0000-000000000005".to_string(),
            rel.to_string(),
            crate::scanner::encode_binary_content(&wrong),
            0,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        assert_eq!(
            write_file_to_disk(&vault, &file).unwrap(),
            WriteOutcome::SkippedBinary
        );
        assert_eq!(std::fs::read(vault.join(rel)).unwrap(), existing);

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_credential_row_missing_one_gpg_id_recipient_is_refused() {
        let vault = temp_vault("cred-write-partial");
        seed_store(&vault);
        let rel = ".password-store/site.com/user.gpg";

        let ids: [[u8; 8]; 3] = [
            [0xC5, 0x82, 0xF8, 0xC6, 0x6A, 0x65, 0x9D, 0x51],
            [0x63, 0x3F, 0xB3, 0x1F, 0xF4, 0x29, 0x71, 0xF1],
            [0x4C, 0xC2, 0xB0, 0x68, 0x2D, 0x69, 0x55, 0x65],
        ];
        let mut partial = Vec::new();
        for id in ids {
            partial.extend_from_slice(&pkesk_packet(id));
        }
        partial.extend_from_slice(&[0xd2, 0x03, 0x01, 0x00, 0x00]);

        let file = SpaceFile::new(
            "aaaaaaaa-0000-0000-0000-000000000006".to_string(),
            rel.to_string(),
            crate::scanner::encode_binary_content(&partial),
            0,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        assert_eq!(
            write_file_to_disk(&vault, &file).unwrap(),
            WriteOutcome::SkippedBinary
        );
        assert!(!vault.join(rel).exists());

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_gpg_outside_the_credential_store_is_never_written() {
        let vault = temp_vault("cred-write-outside");
        seed_store(&vault);
        let rel = "site.com/user.gpg";

        let file = SpaceFile::new(
            "aaaaaaaa-0000-0000-0000-000000000007".to_string(),
            rel.to_string(),
            valid_row_content(),
            0,
            1_600_000_000_000,
            1_600_000_000_000,
        );

        assert_eq!(
            write_file_to_disk(&vault, &file).unwrap(),
            WriteOutcome::SkippedBinary
        );
        assert!(!vault.join(rel).exists());

        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn a_credential_write_round_trips_the_scanner_encoding() {
        let ciphertext = valid_ciphertext();
        let encoded = crate::scanner::encode_binary_content(&ciphertext);
        let decoded = crate::scanner::decode_binary_content(&encoded).unwrap();
        assert_eq!(decoded, ciphertext);
    }

    #[test]
    fn dart_base64_of_a_real_entry_decodes_to_the_same_bytes_in_rust() {
        let ciphertext = valid_ciphertext();
        let dart_produced = "wQwDxYL4xmplnVESAADBDANjP7Mf9Clx8RIAAMEMA0zCsGgtaVVlEgAAwQwDKfERE\
                             KBiSHcSAADSAwEAAA==";
        let decoded = crate::scanner::decode_binary_content(dart_produced).unwrap();
        assert_eq!(decoded, ciphertext);
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
