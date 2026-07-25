use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use std::path::Path;
use walkdir::WalkDir;

use crate::client::SpacetimeClient;
use crate::journal::{self, Journal};
use crate::note::Note;
use crate::sanitize::sanitize_path;
use crate::tracker::ContentTracker;

pub struct MigrationStats {
    pub stripped: usize,
    pub adopted: usize,
    pub clean: usize,
    pub failed: usize,
}

enum FileOutcome {
    Stripped(Note),
    Adopted,
    Clean,
}

pub fn run(
    journal: &Journal,
    client: &SpacetimeClient,
    tracker: &ContentTracker,
    vault_path: &Path,
) -> Result<MigrationStats> {
    let known_note_id = |path: &str| client.get_note_by_path(path).map(|n| n.id);
    sweep(journal, vault_path, &known_note_id, |note| {
        client.upsert_note(note);
        tracker.update(&note.id, &note.content);
    })
}

fn sweep(
    journal: &Journal,
    vault_path: &Path,
    known_note_id: &dyn Fn(&str) -> Option<String>,
    mut on_stripped: impl FnMut(&Note),
) -> Result<MigrationStats> {
    let mut stats = MigrationStats {
        stripped: 0,
        adopted: 0,
        clean: 0,
        failed: 0,
    };

    let walker = WalkDir::new(vault_path).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        !name.starts_with('.') && name != "@eaDir"
    });

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() || path.extension().map_or(true, |e| e != "md") {
            continue;
        }
        match migrate_file(journal, vault_path, path, known_note_id) {
            Ok(FileOutcome::Stripped(note)) => {
                on_stripped(&note);
                stats.stripped += 1;
            }
            Ok(FileOutcome::Adopted) => stats.adopted += 1,
            Ok(FileOutcome::Clean) => stats.clean += 1,
            Err(e) => {
                tracing::error!("Frontmatter migration failed for {:?}: {:#}", path, e);
                stats.failed += 1;
            }
        }
    }

    if stats.stripped == 0 && stats.failed == 0 {
        journal.set_meta_if_absent(
            "md_frontmatter_migrated_at",
            &journal::now_ms().to_string(),
        )?;
    }
    Ok(stats)
}

fn migrate_file(
    journal: &Journal,
    vault_path: &Path,
    abs_path: &Path,
    known_note_id: &dyn Fn(&str) -> Option<String>,
) -> Result<FileOutcome> {
    let bytes = std::fs::read(abs_path)?;
    let text = String::from_utf8(bytes).context("File is not valid UTF-8")?;
    let rel_path = sanitize_path(&abs_path.strip_prefix(vault_path)?.to_string_lossy());

    let Some(uuid) = extract_spacetime_id(&text) else {
        return ensure_row(journal, vault_path, abs_path, &rel_path, known_note_id);
    };

    let record = journal::record_from_disk(vault_path, abs_path, uuid.clone())?;
    journal.upsert(&record)?;

    let stripped = strip_spacetime_id(&text);
    if stripped == text {
        anyhow::bail!(
            "Extracted spacetime_id {} but strip produced identical content: {}",
            uuid,
            rel_path
        );
    }

    let metadata = std::fs::metadata(abs_path)?;
    let tmp_path = abs_path.with_extension("tmp");
    std::fs::write(&tmp_path, &stripped)?;
    std::fs::rename(&tmp_path, abs_path)?;
    let mtime = filetime::FileTime::from_last_modification_time(&metadata);
    filetime::set_file_mtime(abs_path, mtime)?;

    let post = journal::record_from_disk(vault_path, abs_path, uuid.clone())?;
    journal.upsert(&post)?;
    journal.log_event(
        journal::now_ms(),
        "migrate",
        Some(&uuid),
        Some(&rel_path),
        Some(&rel_path),
    )?;
    tracing::info!("Stripped frontmatter identity: {} (ID: {})", rel_path, uuid);

    let note = Note::new(
        uuid,
        rel_path,
        stripped,
        String::new(),
        post.size as u64,
        post.created_time as u64,
        post.modified_time as u64,
    );
    Ok(FileOutcome::Stripped(note))
}

fn ensure_row(
    journal: &Journal,
    vault_path: &Path,
    abs_path: &Path,
    rel_path: &str,
    known_note_id: &dyn Fn(&str) -> Option<String>,
) -> Result<FileOutcome> {
    if journal.by_path(rel_path)?.is_some() {
        return Ok(FileOutcome::Clean);
    }
    let Some(server_id) = known_note_id(rel_path) else {
        return Ok(FileOutcome::Clean);
    };
    let record = journal::record_from_disk(vault_path, abs_path, server_id.clone())?;
    journal.observe(&record, "seed")?;
    tracing::info!(
        "Adopted server identity during migration: {} (ID: {})",
        rel_path,
        server_id
    );
    Ok(FileOutcome::Adopted)
}

static SPACETIME_ID_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^spacetime_id:\s*([a-f0-9\-]+)").unwrap());

static SPACETIME_ID_LINE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^spacetime_id:[^\n]*(\n|$)").unwrap());

fn extract_spacetime_id(content: &str) -> Option<String> {
    if content.starts_with("---") {
        if let Some(end_idx) = content[3..].find("\n---") {
            let yaml_str = &content[3..end_idx + 3];
            if let Ok(json) = serde_yaml::from_str::<Value>(yaml_str) {
                if let Some(id) = json.get("spacetime_id").and_then(|v| v.as_str()) {
                    return Some(id.to_string());
                }
            }
        }
    }

    let mut head_end = content.len().min(1024);
    while !content.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let head = &content[..head_end];
    if let Some(caps) = SPACETIME_ID_REGEX.captures(head) {
        let id = caps.get(1).unwrap().as_str().trim().to_string();
        tracing::warn!("Extracted ID via Regex (YAML malformed): {}", id);
        return Some(id);
    }

    None
}

fn strip_spacetime_id(content: &str) -> String {
    if let Some(stripped) = strip_from_frontmatter(content) {
        return stripped;
    }
    strip_head_line(content)
}

fn strip_from_frontmatter(content: &str) -> Option<String> {
    let rest = content.strip_prefix("---")?;
    let end_idx = rest.find("\n---")?;
    let block = &rest[..end_idx];
    let m = SPACETIME_ID_LINE.find(block)?;
    let (start, end) = line_splice_range(block, m.start(), m.end());

    let mut kept_block = String::with_capacity(block.len());
    kept_block.push_str(&block[..start]);
    kept_block.push_str(&block[end..]);

    if kept_block.trim().is_empty() {
        let after = &rest[end_idx + 4..];
        let body = after
            .strip_prefix("\n\n")
            .or_else(|| after.strip_prefix('\n'))
            .unwrap_or(after);
        return Some(body.to_string());
    }
    Some(format!("---{}{}", kept_block, &rest[end_idx..]))
}

fn strip_head_line(content: &str) -> String {
    let mut head_end = content.len().min(1024);
    while !content.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let Some(m) = SPACETIME_ID_LINE.find(&content[..head_end]) else {
        return content.to_string();
    };
    let (start, end) = line_splice_range(content, m.start(), m.end());
    format!("{}{}", &content[..start], &content[end..])
}

fn line_splice_range(text: &str, start: usize, end: usize) -> (usize, usize) {
    if !text[start..end].ends_with('\n') && text[..start].ends_with('\n') {
        (start - 1, end)
    } else {
        (start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const ID_A: &str = "11111111-1111-1111-1111-111111111111";
    const ID_B: &str = "22222222-2222-2222-2222-222222222222";

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "spacenotes-migrate-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn setup(name: &str) -> (PathBuf, PathBuf, Journal) {
        let dir = temp_dir(name);
        let vault = dir.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        let journal = Journal::open(&dir.join("journal.db")).unwrap();
        (dir, vault, journal)
    }

    fn no_known_id(_path: &str) -> Option<String> {
        None
    }

    fn run_sweep(journal: &Journal, vault: &Path) -> MigrationStats {
        sweep(journal, vault, &no_known_id, |_| {}).unwrap()
    }

    #[test]
    fn strip_removes_id_only_frontmatter_entirely() {
        let (dir, vault, journal) = setup("id-only");
        let content = format!("---\nspacetime_id: {}\n---\n\nbody line\nsecond line\n", ID_A);
        std::fs::write(vault.join("a.md"), &content).unwrap();

        let stats = run_sweep(&journal, &vault);

        assert_eq!(stats.stripped, 1);
        assert_eq!(stats.failed, 0);
        let on_disk = std::fs::read_to_string(vault.join("a.md")).unwrap();
        assert_eq!(on_disk, "body line\nsecond line\n");
        assert_eq!(journal.by_path("a.md").unwrap().unwrap().uuid, ID_A);
        assert_eq!(journal.event_count("migrate"), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_preserves_other_user_keys() {
        let (dir, vault, journal) = setup("user-keys");
        let content = format!(
            "---\ntitle: My Note\nspacetime_id: {}\ntags: [a, b]\n---\n\nbody\n",
            ID_A
        );
        std::fs::write(vault.join("a.md"), &content).unwrap();

        let stats = run_sweep(&journal, &vault);

        assert_eq!(stats.stripped, 1);
        let on_disk = std::fs::read_to_string(vault.join("a.md")).unwrap();
        assert_eq!(on_disk, "---\ntitle: My Note\ntags: [a, b]\n---\n\nbody\n");
        assert_eq!(journal.by_path("a.md").unwrap().unwrap().uuid, ID_A);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn second_sweep_is_a_no_op_and_records_completion() {
        let (dir, vault, journal) = setup("idempotent");
        let content = format!("---\nspacetime_id: {}\n---\n\nbody\n", ID_A);
        std::fs::write(vault.join("a.md"), &content).unwrap();

        let first = run_sweep(&journal, &vault);
        let after_first = std::fs::read_to_string(vault.join("a.md")).unwrap();
        assert!(journal
            .get_meta("md_frontmatter_migrated_at")
            .unwrap()
            .is_none());

        let second = run_sweep(&journal, &vault);
        let after_second = std::fs::read_to_string(vault.join("a.md")).unwrap();

        assert_eq!(first.stripped, 1);
        assert_eq!(second.stripped, 0);
        assert_eq!(second.clean, 1);
        assert_eq!(after_first, after_second);
        assert_eq!(journal.by_path("a.md").unwrap().unwrap().uuid, ID_A);
        assert_eq!(journal.event_count("migrate"), 1);
        assert!(journal
            .get_meta("md_frontmatter_migrated_at")
            .unwrap()
            .is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn journal_row_committed_before_strip() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, vault, journal) = setup("row-before-strip");
        let content = format!("---\nspacetime_id: {}\n---\n\nbody\n", ID_A);
        std::fs::write(vault.join("a.md"), &content).unwrap();
        std::fs::set_permissions(&vault, std::fs::Permissions::from_mode(0o555)).unwrap();

        let stats = run_sweep(&journal, &vault);

        std::fs::set_permissions(&vault, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.stripped, 0);
        let on_disk = std::fs::read_to_string(vault.join("a.md")).unwrap();
        assert_eq!(on_disk, content);
        assert_eq!(journal.by_path("a.md").unwrap().unwrap().uuid, ID_A);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_preserves_original_mtime() {
        let (dir, vault, journal) = setup("mtime");
        let content = format!("---\nspacetime_id: {}\n---\n\nbody\n", ID_A);
        let abs = vault.join("a.md");
        std::fs::write(&abs, &content).unwrap();
        let original = filetime::FileTime::from_unix_time(1_600_000_000, 0);
        filetime::set_file_mtime(&abs, original).unwrap();

        let stats = run_sweep(&journal, &vault);

        assert_eq!(stats.stripped, 1);
        let metadata = std::fs::metadata(&abs).unwrap();
        let after = filetime::FileTime::from_last_modification_time(&metadata);
        assert_eq!(after.unix_seconds(), original.unix_seconds());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_without_frontmatter_is_left_byte_identical() {
        let (dir, vault, journal) = setup("no-frontmatter");
        let content = "plain body\nno identity here\n";
        let abs = vault.join("a.md");
        std::fs::write(&abs, content).unwrap();
        let before = std::fs::metadata(&abs).unwrap();

        let stats = run_sweep(&journal, &vault);

        assert_eq!(stats.stripped, 0);
        assert_eq!(stats.clean, 1);
        let bytes = std::fs::read(&abs).unwrap();
        assert_eq!(bytes, content.as_bytes());
        let after = std::fs::metadata(&abs).unwrap();
        assert_eq!(
            before.modified().unwrap(),
            after.modified().unwrap()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_id_file_known_to_server_adopts_server_uuid() {
        let (dir, vault, journal) = setup("adopt");
        std::fs::write(vault.join("a.md"), "plain body\n").unwrap();
        let known = |path: &str| (path == "a.md").then(|| ID_B.to_string());

        let stats = sweep(&journal, &vault, &known, |_| {}).unwrap();

        assert_eq!(stats.adopted, 1);
        assert_eq!(stats.stripped, 0);
        assert_eq!(journal.by_path("a.md").unwrap().unwrap().uuid, ID_B);
        let on_disk = std::fs::read_to_string(vault.join("a.md")).unwrap();
        assert_eq!(on_disk, "plain body\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stripped_note_is_upserted_with_empty_frontmatter() {
        let (dir, vault, journal) = setup("upsert");
        let content = format!("---\nspacetime_id: {}\n---\n\nbody\n", ID_A);
        std::fs::write(vault.join("a.md"), &content).unwrap();

        let mut upserted = Vec::new();
        sweep(&journal, &vault, &no_known_id, |note| {
            upserted.push(note.clone());
        })
        .unwrap();

        assert_eq!(upserted.len(), 1);
        assert_eq!(upserted[0].id, ID_A);
        assert_eq!(upserted[0].path, "a.md");
        assert_eq!(upserted[0].content, "body\n");
        assert_eq!(upserted[0].frontmatter, "");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_yaml_still_strips_id_line() {
        let (dir, vault, journal) = setup("malformed");
        let content = format!(
            "---\n\ttitle: bad tab indent\nspacetime_id: {}\n---\n\nbody\n",
            ID_A
        );
        std::fs::write(vault.join("a.md"), &content).unwrap();

        let stats = run_sweep(&journal, &vault);

        assert_eq!(stats.stripped, 1);
        let on_disk = std::fs::read_to_string(vault.join("a.md")).unwrap();
        assert_eq!(on_disk, "---\n\ttitle: bad tab indent\n---\n\nbody\n");
        assert_eq!(journal.by_path("a.md").unwrap().unwrap().uuid, ID_A);

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn straddling_content() -> String {
        let prefix = "---\ntitle: Migration Note\ntags: [a, b]\n---\n\n";
        let em_dash = "—";
        let em_dash_start = 1022;
        let pad_before = em_dash_start - prefix.len();
        let mut content = String::new();
        content.push_str(prefix);
        content.push_str(&"x".repeat(pad_before));
        content.push_str(em_dash);
        content.push_str(&"y".repeat(500));
        assert_eq!(content.as_bytes()[em_dash_start], 0xE2);
        assert!(!content.is_char_boundary(1024));
        assert!(content.len() > 1024);
        content
    }

    #[test]
    fn multibyte_straddling_1024_returns_none_not_panic() {
        let content = straddling_content();
        assert_eq!(extract_spacetime_id(&content), None);
    }

    #[test]
    fn strategy_1_yaml_spacetime_id_extracted() {
        let id = "abc123-def456";
        let prefix = format!("---\nspacetime_id: {}\ntitle: Note\n---\n\n", id);
        let content = format!("{}{}—{}", prefix, "x".repeat(1200), "y".repeat(200));
        assert!(content.len() > 1024);
        assert_eq!(extract_spacetime_id(&content), Some(id.to_string()));
    }

    #[test]
    fn straddle_does_not_widen_scan_to_body_id() {
        let prefix = "---\n\ttitle: bad tab\n---\n\n";
        let em_dash = "—";
        let em_dash_start = 1022;
        let pad_before = em_dash_start - prefix.len();
        let mut content = String::new();
        content.push_str(prefix);
        content.push_str(&"x".repeat(pad_before));
        content.push_str(em_dash);
        content.push_str("\nlater in the body someone wrote:\nspacetime_id: deadbeef\n");
        assert!(!content.is_char_boundary(1024));
        assert!(content.len() > 1024);
        assert_eq!(extract_spacetime_id(&content), None);
    }

    #[test]
    fn strategy_2_regex_fallback_extracted() {
        let id = "0a1b2c3d-4e5f";
        let content = format!(
            "---\n\ttitle: bad tab indent\nspacetime_id: {}\n---\n\nbody text here",
            id
        );
        assert_eq!(extract_spacetime_id(&content), Some(id.to_string()));
    }
}
