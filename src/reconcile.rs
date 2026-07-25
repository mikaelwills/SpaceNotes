use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::client::SpacetimeClient;
use crate::isolation::run_isolated;
use crate::journal::{self, FileRecord, Journal};
use crate::note::Note;
use crate::scanner::scan_notes;
use crate::tracker::ContentTracker;
use crate::writer::write_note_to_disk;

enum Outcome {
    Downloaded,
    Uploaded,
    Unchanged,
    Skipped,
}

pub struct LadderOutcome {
    pub relinked: HashSet<String>,
    pub orphans: Vec<FileRecord>,
}

#[derive(Debug, PartialEq)]
enum OrphanDecision {
    ForgetLocally,
    PropagateDelete,
    Resurrect,
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(parent, _)| parent).unwrap_or("")
}

pub fn resolve_offline_identities(
    journal: &Journal,
    vault_path: &Path,
    records: &[FileRecord],
    now: i64,
) -> Result<LadderOutcome> {
    let live = journal.live_files()?;
    let disk_paths: HashSet<&str> = records.iter().map(|r| r.path.as_str()).collect();
    let live_by_path: HashMap<String, FileRecord> = live
        .iter()
        .filter(|r| disk_paths.contains(r.path.as_str()))
        .map(|r| (r.path.clone(), r.clone()))
        .collect();
    let mut vanished: Vec<FileRecord> = live
        .into_iter()
        .filter(|r| {
            !disk_paths.contains(r.path.as_str()) && !vault_path.join(&r.path).exists()
        })
        .collect();

    let mut relinked = HashSet::new();
    let mut leftover: Vec<&FileRecord> = Vec::new();

    for record in records {
        if let Some(row) = live_by_path.get(&record.path) {
            refresh_path_match(journal, row, record, now)?;
            continue;
        }
        if let Some(row) = take_inode_match(&mut vanished, record) {
            apply_relink(journal, &row, record, now, &mut relinked, "inode")?;
            continue;
        }
        if let Some(row) = take_hash_match(journal, &mut vanished, record, now)? {
            apply_relink(journal, &row, record, now, &mut relinked, "hash")?;
            continue;
        }
        leftover.push(record);
    }

    let mut file_name_counts: HashMap<String, usize> = HashMap::new();
    for record in &leftover {
        *file_name_counts
            .entry(basename(&record.path).to_string())
            .or_insert(0) += 1;
    }
    let mut row_name_counts: HashMap<String, usize> = HashMap::new();
    for row in &vanished {
        *row_name_counts
            .entry(basename(&row.path).to_string())
            .or_insert(0) += 1;
    }

    for record in leftover {
        let name = basename(&record.path);
        let one_to_one =
            file_name_counts.get(name) == Some(&1) && row_name_counts.get(name) == Some(&1);
        if one_to_one {
            if let Some(idx) = vanished.iter().position(|r| basename(&r.path) == name) {
                let row = vanished.remove(idx);
                apply_relink(journal, &row, record, now, &mut relinked, "basename")?;
                continue;
            }
        }
        journal.observe(record, "create")?;
    }

    Ok(LadderOutcome {
        relinked,
        orphans: vanished,
    })
}

fn refresh_path_match(
    journal: &Journal,
    row: &FileRecord,
    record: &FileRecord,
    now: i64,
) -> Result<()> {
    if record.uuid != row.uuid {
        tracing::error!(
            "Journal/frontmatter UUID mismatch at {}: journal {}, frontmatter {}; adopting frontmatter",
            record.path,
            row.uuid,
            record.uuid
        );
        journal.tombstone(&row.uuid, now)?;
        journal.observe(record, "create")?;
        return Ok(());
    }
    journal.upsert(record)
}

fn apply_relink(
    journal: &Journal,
    row: &FileRecord,
    record: &FileRecord,
    now: i64,
    relinked: &mut HashSet<String>,
    rung: &str,
) -> Result<()> {
    if record.uuid != row.uuid {
        tracing::error!(
            "Journal/frontmatter UUID mismatch for {} -> {}: journal {}, frontmatter {}; adopting frontmatter",
            row.path,
            record.path,
            row.uuid,
            record.uuid
        );
        journal.tombstone(&row.uuid, now)?;
        journal.observe(record, "create")?;
        return Ok(());
    }
    journal.relink(&row.uuid, &record.path, now)?;
    journal.upsert(record)?;
    relinked.insert(row.uuid.clone());
    tracing::info!(
        "Re-linked offline move via {}: {} -> {} (ID: {})",
        rung,
        row.path,
        record.path,
        row.uuid
    );
    Ok(())
}

fn take_inode_match(vanished: &mut Vec<FileRecord>, record: &FileRecord) -> Option<FileRecord> {
    let (Some(device), Some(inode)) = (record.device, record.inode) else {
        return None;
    };
    let matches: Vec<usize> = vanished
        .iter()
        .enumerate()
        .filter(|(_, r)| r.device == Some(device) && r.inode == Some(inode))
        .map(|(i, _)| i)
        .collect();
    if matches.len() == 1 {
        Some(vanished.remove(matches[0]))
    } else {
        None
    }
}

fn take_hash_match(
    journal: &Journal,
    vanished: &mut Vec<FileRecord>,
    record: &FileRecord,
    now: i64,
) -> Result<Option<FileRecord>> {
    let mut candidates: Vec<FileRecord> = vanished
        .iter()
        .filter(|r| r.content_hash == record.content_hash && r.size == record.size)
        .cloned()
        .collect();
    let tombstones = journal.relinkable_tombstones(
        &record.content_hash,
        record.size,
        now - journal::RELINK_WINDOW_MS,
    )?;
    for tombstone in tombstones {
        if !candidates.iter().any(|c| c.uuid == tombstone.uuid) {
            candidates.push(tombstone);
        }
    }
    let Some(chosen) = tiebreak(candidates, &record.path) else {
        return Ok(None);
    };
    vanished.retain(|r| r.uuid != chosen.uuid);
    Ok(Some(chosen))
}

fn tiebreak(candidates: Vec<FileRecord>, path: &str) -> Option<FileRecord> {
    if candidates.len() <= 1 {
        return candidates.into_iter().next();
    }
    let name = basename(path);
    let by_name: Vec<FileRecord> = candidates
        .iter()
        .filter(|c| basename(&c.path) == name)
        .cloned()
        .collect();
    if by_name.len() == 1 {
        return by_name.into_iter().next();
    }
    let pool = if by_name.is_empty() { candidates } else { by_name };
    let parent = parent_of(path);
    let mut by_parent: Vec<FileRecord> = pool
        .into_iter()
        .filter(|c| parent_of(&c.path) == parent)
        .collect();
    if by_parent.len() == 1 {
        by_parent.pop()
    } else {
        None
    }
}

fn decide_orphan(row: &FileRecord, server: Option<&Note>) -> OrphanDecision {
    match server {
        None => OrphanDecision::ForgetLocally,
        Some(server) if (server.modified_time as i64) > row.modified_time => {
            OrphanDecision::Resurrect
        }
        Some(_) => OrphanDecision::PropagateDelete,
    }
}

fn propagate_offline_deletes(
    client: &SpacetimeClient,
    tracker: &ContentTracker,
    journal: &Journal,
    server_map: &mut HashMap<String, Note>,
    orphans: Vec<FileRecord>,
    now: i64,
) {
    for row in orphans {
        match decide_orphan(&row, server_map.get(&row.uuid)) {
            OrphanDecision::ForgetLocally => {
                if let Err(e) = journal.tombstone(&row.uuid, now) {
                    tracing::error!("Journal tombstone failed for {}: {}", row.path, e);
                }
            }
            OrphanDecision::Resurrect => {
                tracing::info!(
                    "Offline delete superseded by newer server copy, resurrecting: {} (ID: {})",
                    row.path,
                    row.uuid
                );
            }
            OrphanDecision::PropagateDelete => {
                if let Err(e) = journal.tombstone(&row.uuid, now) {
                    tracing::error!("Journal tombstone failed for {}: {}", row.path, e);
                    continue;
                }
                client.delete_note(&row.uuid);
                tracker.remove(&row.uuid);
                server_map.remove(&row.uuid);
                tracing::info!("Propagated offline delete: {} (ID: {})", row.path, row.uuid);
            }
        }
    }
}

fn disk_records(vault_path: &Path, notes: &[Note]) -> Vec<FileRecord> {
    notes
        .iter()
        .filter_map(|note| {
            let abs = vault_path.join(&note.path);
            match journal::record_from_disk(vault_path, &abs, note.id.clone()) {
                Ok(record) => Some(record),
                Err(e) => {
                    tracing::warn!("Failed to build journal record for {}: {}", note.path, e);
                    None
                }
            }
        })
        .collect()
}

fn reconcile_one(
    vault_path: &Path,
    client: &SpacetimeClient,
    tracker: &ContentTracker,
    local_map: &HashMap<String, Note>,
    server_map: &HashMap<String, Note>,
    relinked: &HashSet<String>,
    id: &str,
) -> Result<Outcome> {
    match (local_map.get(id), server_map.get(id)) {
        (Some(local), Some(server)) => {
            let moved_locally = relinked.contains(id) && local.path != server.path;
            if server.modified_time > local.modified_time {
                if moved_locally {
                    let merged = Note::new(
                        server.id.clone(),
                        local.path.clone(),
                        server.content.clone(),
                        server.frontmatter.clone(),
                        server.size,
                        server.created_time,
                        server.modified_time,
                    );
                    write_note_to_disk(vault_path, &merged)?;
                    tracker.update(&merged.id, &merged.content);
                    client.upsert_note(&merged);
                    tracing::debug!(
                        "Downloaded newer into moved path: {} (ID: {})",
                        merged.path,
                        id
                    );
                } else {
                    write_note_to_disk(vault_path, server)?;
                    tracker.update(&server.id, &server.content);
                    tracing::debug!("Downloaded newer: {} (ID: {})", server.path, id);
                }
                Ok(Outcome::Downloaded)
            } else if local.modified_time > server.modified_time {
                client.upsert_note(local);
                tracker.update(&local.id, &local.content);
                tracing::debug!("Uploaded newer: {} (ID: {})", local.path, id);
                Ok(Outcome::Uploaded)
            } else if moved_locally {
                client.upsert_note(local);
                tracker.update(&local.id, &local.content);
                tracing::info!(
                    "Propagated offline move: {} -> {} (ID: {})",
                    server.path,
                    local.path,
                    id
                );
                Ok(Outcome::Uploaded)
            } else {
                tracker.update(&local.id, &local.content);
                Ok(Outcome::Unchanged)
            }
        }

        (None, Some(server)) => {
            write_note_to_disk(vault_path, server)?;
            tracker.update(&server.id, &server.content);
            tracing::debug!("Downloaded new: {} (ID: {})", server.path, id);
            Ok(Outcome::Downloaded)
        }

        (Some(local), None) => {
            client.upsert_note(local);
            tracker.update(&local.id, &local.content);
            tracing::debug!("Uploaded new: {} (ID: {})", local.path, id);
            Ok(Outcome::Uploaded)
        }

        (None, None) => unreachable!(),
    }
}

pub fn reconcile_on_startup(
    vault_path: &Path,
    client: &SpacetimeClient,
    tracker: &ContentTracker,
    journal: Option<&Journal>,
) -> Result<()> {
    let server_notes = client.get_all_notes();
    let local_notes = scan_notes(vault_path)?;

    let mut server_map: HashMap<String, Note> = server_notes
        .into_iter()
        .map(|n| (n.id.clone(), n))
        .collect();

    let mut relinked: HashSet<String> = HashSet::new();
    if let Some(journal) = journal {
        let now = journal::now_ms();
        let records = disk_records(vault_path, &local_notes);
        match resolve_offline_identities(journal, vault_path, &records, now) {
            Ok(outcome) => {
                relinked = outcome.relinked;
                propagate_offline_deletes(
                    client,
                    tracker,
                    journal,
                    &mut server_map,
                    outcome.orphans,
                    now,
                );
            }
            Err(e) => tracing::error!("Offline identity resolution failed: {:#}", e),
        }
    }

    let local_map: HashMap<String, Note> = local_notes
        .into_iter()
        .map(|n| (n.id.clone(), n))
        .collect();

    let all_ids: HashSet<&String> = server_map.keys().chain(local_map.keys()).collect();

    let mut downloaded = 0;
    let mut uploaded = 0;
    let mut unchanged = 0;

    for id in all_ids {
        let mut outcome: Result<Outcome> = Ok(Outcome::Skipped);

        let context = format!("reconcile note (ID: {})", id);
        run_isolated(context, || {
            outcome = reconcile_one(
                vault_path,
                client,
                tracker,
                &local_map,
                &server_map,
                &relinked,
                id,
            );
        });

        match outcome? {
            Outcome::Downloaded => downloaded += 1,
            Outcome::Uploaded => uploaded += 1,
            Outcome::Unchanged => unchanged += 1,
            Outcome::Skipped => {}
        }
    }

    tracing::info!(
        "Reconciliation complete: {} downloaded, {} uploaded, {} unchanged",
        downloaded,
        uploaded,
        unchanged
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontmatter::extract_spacetime_id;
    use crate::journal::{hash_bytes, now_ms, record_from_disk, seed_from_vault};
    use std::path::PathBuf;

    const ID_A: &str = "11111111-1111-1111-1111-111111111111";
    const ID_B: &str = "22222222-2222-2222-2222-222222222222";
    const ID_C: &str = "33333333-3333-3333-3333-333333333333";

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "spacenotes-reconcile-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn note_content(id: &str, file: &str) -> String {
        format!("---\nspacetime_id: {}\n---\nbody of {}\n", id, file)
    }

    fn write_note(vault: &Path, file: &str, id: &str) {
        std::fs::write(vault.join(file), note_content(id, file)).unwrap();
    }

    fn seeded(name: &str) -> (PathBuf, PathBuf, Journal) {
        let dir = temp_dir(name);
        let vault = dir.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        write_note(&vault, "a.md", ID_A);
        let journal = Journal::open(&dir.join("journal.db")).unwrap();
        seed_from_vault(&journal, &vault).unwrap();
        (dir, vault, journal)
    }

    fn record_for(vault: &Path, rel: &str) -> FileRecord {
        let abs = vault.join(rel);
        let content = std::fs::read_to_string(&abs).unwrap();
        let uuid = extract_spacetime_id(&content).unwrap();
        record_from_disk(vault, &abs, uuid).unwrap()
    }

    fn fabricated_record(uuid: &str, path: &str, hash: &str, size: i64, inode: i64) -> FileRecord {
        let now = now_ms();
        FileRecord {
            uuid: uuid.to_string(),
            path: path.to_string(),
            kind: "md".to_string(),
            content_hash: hash.to_string(),
            size,
            device: Some(1),
            inode: Some(inode),
            created_time: now,
            modified_time: now,
            last_seen_at: now,
            deleted_at: None,
        }
    }

    #[test]
    fn offline_move_relinks_by_inode() {
        let (dir, vault, journal) = seeded("move-inode");
        std::fs::rename(vault.join("a.md"), vault.join("moved.md")).unwrap();

        let record = record_for(&vault, "moved.md");
        let outcome =
            resolve_offline_identities(&journal, &vault, &[record], now_ms()).unwrap();

        assert!(outcome.relinked.contains(ID_A));
        assert!(outcome.orphans.is_empty());
        assert_eq!(journal.by_path("moved.md").unwrap().unwrap().uuid, ID_A);
        assert!(journal.by_path("a.md").unwrap().is_none());
        assert_eq!(journal.event_count("relink"), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offline_move_and_edit_relinks_by_inode() {
        let (dir, vault, journal) = seeded("move-edit-inode");
        std::fs::rename(vault.join("a.md"), vault.join("moved.md")).unwrap();
        let edited = format!("---\nspacetime_id: {}\n---\nedited body\n", ID_A);
        std::fs::write(vault.join("moved.md"), &edited).unwrap();

        let record = record_for(&vault, "moved.md");
        let outcome =
            resolve_offline_identities(&journal, &vault, &[record], now_ms()).unwrap();

        assert!(outcome.relinked.contains(ID_A));
        assert!(outcome.orphans.is_empty());
        let row = journal.by_path("moved.md").unwrap().unwrap();
        assert_eq!(row.uuid, ID_A);
        assert_eq!(row.content_hash, hash_bytes(edited.as_bytes()));
        assert_eq!(journal.event_count("relink"), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offline_move_with_inode_churn_relinks_by_hash() {
        let (dir, vault, journal) = seeded("move-hash");
        std::fs::copy(vault.join("a.md"), vault.join("moved.md")).unwrap();
        std::fs::remove_file(vault.join("a.md")).unwrap();

        let record = record_for(&vault, "moved.md");
        let original_inode = journal.by_path("a.md").unwrap().unwrap().inode;
        assert_ne!(record.inode, original_inode);

        let outcome =
            resolve_offline_identities(&journal, &vault, &[record], now_ms()).unwrap();

        assert!(outcome.relinked.contains(ID_A));
        assert!(outcome.orphans.is_empty());
        assert_eq!(journal.by_path("moved.md").unwrap().unwrap().uuid, ID_A);
        assert_eq!(journal.event_count("relink"), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ambiguous_hash_match_is_treated_as_new() {
        let dir = temp_dir("ambiguous-hash");
        let vault = dir.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        let journal = Journal::open(&dir.join("journal.db")).unwrap();

        journal
            .upsert(&fabricated_record(ID_A, "x/dup.md", "deadbeef", 42, 1))
            .unwrap();
        journal
            .upsert(&fabricated_record(ID_B, "y/dup.md", "deadbeef", 42, 2))
            .unwrap();

        let incoming = fabricated_record(ID_C, "z/dup.md", "deadbeef", 42, 99);
        let outcome =
            resolve_offline_identities(&journal, &vault, &[incoming], now_ms()).unwrap();

        assert!(outcome.relinked.is_empty());
        assert_eq!(outcome.orphans.len(), 2);
        assert_eq!(journal.by_path("z/dup.md").unwrap().unwrap().uuid, ID_C);
        assert_eq!(journal.by_path("x/dup.md").unwrap().unwrap().uuid, ID_A);
        assert_eq!(journal.by_path("y/dup.md").unwrap().unwrap().uuid, ID_B);
        assert_eq!(journal.event_count("relink"), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn basename_heuristic_relinks_one_to_one() {
        let dir = temp_dir("basename");
        let vault = dir.join("vault");
        std::fs::create_dir_all(vault.join("notes")).unwrap();
        std::fs::create_dir_all(vault.join("archive")).unwrap();
        write_note(&vault, "notes/spec.md", ID_A);
        let journal = Journal::open(&dir.join("journal.db")).unwrap();
        seed_from_vault(&journal, &vault).unwrap();

        let edited = format!("---\nspacetime_id: {}\n---\nedited spec body\n", ID_A);
        std::fs::write(vault.join("archive/spec.md"), &edited).unwrap();
        std::fs::remove_file(vault.join("notes/spec.md")).unwrap();

        let record = record_for(&vault, "archive/spec.md");
        let original = journal.by_path("notes/spec.md").unwrap().unwrap();
        assert_ne!(record.inode, original.inode);
        assert_ne!(record.content_hash, original.content_hash);

        let outcome =
            resolve_offline_identities(&journal, &vault, &[record], now_ms()).unwrap();

        assert!(outcome.relinked.contains(ID_A));
        assert!(outcome.orphans.is_empty());
        assert_eq!(journal.by_path("archive/spec.md").unwrap().unwrap().uuid, ID_A);
        assert_eq!(journal.event_count("relink"), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn frontmatter_wins_over_ladder_disagreement() {
        let (dir, vault, journal) = seeded("oracle-mismatch");
        std::fs::write(vault.join("a.md"), note_content(ID_B, "a.md")).unwrap();

        let record = record_for(&vault, "a.md");
        let outcome =
            resolve_offline_identities(&journal, &vault, &[record], now_ms()).unwrap();

        assert!(outcome.relinked.is_empty());
        assert_eq!(journal.by_path("a.md").unwrap().unwrap().uuid, ID_B);
        assert!(journal.by_uuid(ID_A).unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offline_delete_yields_orphan_row_without_tombstoning() {
        let dir = temp_dir("offline-delete");
        let vault = dir.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        write_note(&vault, "a.md", ID_A);
        write_note(&vault, "b.md", ID_B);
        let journal = Journal::open(&dir.join("journal.db")).unwrap();
        seed_from_vault(&journal, &vault).unwrap();
        std::fs::remove_file(vault.join("b.md")).unwrap();

        let record = record_for(&vault, "a.md");
        let outcome =
            resolve_offline_identities(&journal, &vault, &[record], now_ms()).unwrap();

        assert!(outcome.relinked.is_empty());
        assert_eq!(outcome.orphans.len(), 1);
        assert_eq!(outcome.orphans[0].uuid, ID_B);
        assert!(journal.by_path("b.md").unwrap().is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recent_tombstone_relinks_by_hash_within_window() {
        let (dir, vault, journal) = seeded("tombstone-relink");
        journal.tombstone(ID_A, now_ms()).unwrap();
        std::fs::rename(vault.join("a.md"), vault.join("moved.md")).unwrap();

        let record = record_for(&vault, "moved.md");
        let outcome =
            resolve_offline_identities(&journal, &vault, &[record], now_ms()).unwrap();

        assert!(outcome.relinked.contains(ID_A));
        let row = journal.by_path("moved.md").unwrap().unwrap();
        assert_eq!(row.uuid, ID_A);
        assert!(row.deleted_at.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn server_note(id: &str, path: &str, modified_time: u64) -> Note {
        Note::new(
            id.to_string(),
            path.to_string(),
            "body".to_string(),
            "{}".to_string(),
            4,
            modified_time,
            modified_time,
        )
    }

    #[test]
    fn orphan_matching_server_copy_propagates_delete() {
        let mut row = fabricated_record(ID_A, "b.md", "deadbeef", 4, 1);
        row.modified_time = 1000;
        let server = server_note(ID_A, "b.md", 1000);
        assert_eq!(
            decide_orphan(&row, Some(&server)),
            OrphanDecision::PropagateDelete
        );
    }

    #[test]
    fn orphan_with_newer_server_copy_resurrects() {
        let mut row = fabricated_record(ID_A, "b.md", "deadbeef", 4, 1);
        row.modified_time = 1000;
        let server = server_note(ID_A, "b.md", 2000);
        assert_eq!(
            decide_orphan(&row, Some(&server)),
            OrphanDecision::Resurrect
        );
    }

    #[test]
    fn orphan_unknown_to_server_is_forgotten_locally() {
        let row = fabricated_record(ID_A, "b.md", "deadbeef", 4, 1);
        assert_eq!(decide_orphan(&row, None), OrphanDecision::ForgetLocally);
    }
}
