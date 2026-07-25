use anyhow::Result;
use notify_debouncer_full::notify::event::{EventKind, ModifyKind, RenameMode};
use notify_debouncer_full::notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use crate::client::SpacetimeClient;
use crate::folder::Folder;
use crate::frontmatter::inject_spacetime_id;
use crate::isolation::run_isolated;
use crate::journal::{self, Journal};
use crate::note::Note;
use crate::sanitize::sanitize_path;
use crate::scanner::{read_note_at, scan_for_note_by_id};
use crate::tracker::ContentTracker;

enum Action {
    UpsertNote(Note),
    DeleteNote { id: String, path: String },
    UpsertFolder(String),
    FolderVanished(String),
    Reconcile,
}

struct EventContext<'a> {
    vault_path: &'a Path,
    journal: Option<&'a Journal>,
    tracker: &'a ContentTracker,
    known_note_id: &'a dyn Fn(&str) -> Option<String>,
}

fn is_ignored(path: &Path) -> bool {
    path.iter().any(|name| {
        name.to_str()
            .map_or(false, |s| s.starts_with('.') || s == "@eaDir")
    })
}

fn is_md(path: &Path) -> bool {
    path.extension().map_or(false, |e| e == "md")
}

fn rel_path(vault_path: &Path, abs: &Path) -> Option<String> {
    abs.strip_prefix(vault_path)
        .ok()
        .map(|r| sanitize_path(&r.to_string_lossy()))
}

fn dispatch_event(
    ctx: &EventContext,
    kind: &EventKind,
    paths: &[PathBuf],
    needs_rescan: bool,
) -> Vec<Action> {
    if needs_rescan {
        return vec![Action::Reconcile];
    }
    match kind {
        EventKind::Create(_) => paths.iter().flat_map(|p| handle_present(ctx, p)).collect(),
        EventKind::Modify(ModifyKind::Name(mode)) => handle_rename(ctx, mode, paths),
        EventKind::Modify(_) => paths.iter().flat_map(|p| handle_present(ctx, p)).collect(),
        EventKind::Remove(_) => paths.iter().flat_map(|p| handle_absent(ctx, p)).collect(),
        EventKind::Access(_) => Vec::new(),
        EventKind::Any | EventKind::Other => paths
            .iter()
            .flat_map(|p| handle_by_existence(ctx, p))
            .collect(),
    }
}

fn handle_rename(ctx: &EventContext, mode: &RenameMode, paths: &[PathBuf]) -> Vec<Action> {
    match mode {
        RenameMode::Both if paths.len() == 2 => rename_both(ctx, &paths[0], &paths[1]),
        RenameMode::From => paths.iter().flat_map(|p| handle_absent(ctx, p)).collect(),
        RenameMode::To => paths.iter().flat_map(|p| handle_present(ctx, p)).collect(),
        _ => paths
            .iter()
            .flat_map(|p| handle_by_existence(ctx, p))
            .collect(),
    }
}

fn rename_both(ctx: &EventContext, from: &Path, to: &Path) -> Vec<Action> {
    if is_ignored(to) {
        return handle_absent(ctx, from);
    }
    if is_ignored(from) {
        return handle_present(ctx, to);
    }
    let (Some(from_rel), Some(to_rel)) = (rel_path(ctx.vault_path, from), rel_path(ctx.vault_path, to))
    else {
        return Vec::new();
    };

    if to.is_dir() {
        return rename_folder(ctx, from_rel, to_rel);
    }
    match (is_md(from), is_md(to)) {
        (true, true) => rename_md(ctx, &from_rel, to),
        (true, false) => delete_md(ctx, from_rel),
        (false, true) => handle_present(ctx, to),
        (false, false) => Vec::new(),
    }
}

fn rename_md(ctx: &EventContext, from_rel: &str, to_abs: &Path) -> Vec<Action> {
    let Some(journal) = ctx.journal else {
        return upsert_md(ctx, to_abs);
    };
    let Some(row) = journal.by_path(from_rel).ok().flatten() else {
        return upsert_md(ctx, to_abs);
    };

    let mut note = match read_note_at(ctx.vault_path, to_abs) {
        Ok(Some(note)) => note,
        Ok(None) => return delete_md(ctx, from_rel.to_string()),
        Err(e) => {
            tracing::error!("Error processing rename target {:?}: {}", to_abs, e);
            return Vec::new();
        }
    };

    if !note.id.is_empty() && note.id != row.uuid {
        tracing::error!(
            "Journal/frontmatter UUID mismatch for {} -> {}: journal {}, frontmatter {}; adopting frontmatter",
            from_rel,
            note.path,
            row.uuid,
            note.id
        );
        if let Err(e) = journal.tombstone(&row.uuid, journal::now_ms()) {
            tracing::error!("Journal tombstone failed for {}: {}", from_rel, e);
        }
        record_in_journal(ctx, to_abs, &note.id);
        return vec![Action::UpsertNote(note)];
    }

    if let Err(e) = journal.rekey(&row.uuid, &note.path, journal::now_ms()) {
        tracing::error!("Journal rekey failed {} -> {}: {}", from_rel, note.path, e);
    }
    if note.id.is_empty() {
        note.id = row.uuid.clone();
    }
    record_in_journal(ctx, to_abs, &note.id);
    ctx.tracker.is_modified(&note.id, &note.content);
    tracing::info!(
        "Renamed note: {} -> {} (ID: {})",
        from_rel,
        note.path,
        note.id
    );
    vec![Action::UpsertNote(note)]
}

fn rename_folder(ctx: &EventContext, from_rel: String, to_rel: String) -> Vec<Action> {
    let mut actions = vec![Action::UpsertFolder(to_rel.clone())];
    if let Some(journal) = ctx.journal {
        match journal.rekey_prefix(&from_rel, &to_rel, journal::now_ms()) {
            Ok(moved) => {
                for (uuid, new_rel) in moved {
                    let abs = ctx.vault_path.join(&new_rel);
                    match read_note_at(ctx.vault_path, &abs) {
                        Ok(Some(mut note)) => {
                            if note.id.is_empty() {
                                note.id = uuid;
                            }
                            actions.push(Action::UpsertNote(note));
                        }
                        Ok(None) => {
                            tracing::warn!("Rekeyed note missing on disk: {}", new_rel)
                        }
                        Err(e) => tracing::error!("Error reading {}: {}", new_rel, e),
                    }
                }
            }
            Err(e) => tracing::error!(
                "Journal folder rekey failed {} -> {}: {}",
                from_rel,
                to_rel,
                e
            ),
        }
    }
    actions.push(Action::FolderVanished(from_rel));
    actions
}

fn handle_present(ctx: &EventContext, abs: &Path) -> Vec<Action> {
    if is_ignored(abs) {
        return Vec::new();
    }
    if abs.is_dir() {
        let Some(rel) = rel_path(ctx.vault_path, abs) else {
            return Vec::new();
        };
        return vec![Action::UpsertFolder(rel)];
    }
    if is_md(abs) {
        return upsert_md(ctx, abs);
    }
    Vec::new()
}

fn handle_absent(ctx: &EventContext, abs: &Path) -> Vec<Action> {
    if is_ignored(abs) {
        return Vec::new();
    }
    let Some(rel) = rel_path(ctx.vault_path, abs) else {
        return Vec::new();
    };
    if is_md(abs) {
        return delete_md(ctx, rel);
    }
    if abs.extension().is_none() {
        return vec![Action::FolderVanished(rel)];
    }
    Vec::new()
}

fn handle_by_existence(ctx: &EventContext, abs: &Path) -> Vec<Action> {
    if abs.exists() {
        handle_present(ctx, abs)
    } else {
        handle_absent(ctx, abs)
    }
}

fn upsert_md(ctx: &EventContext, abs: &Path) -> Vec<Action> {
    let mut note = match read_note_at(ctx.vault_path, abs) {
        Ok(Some(note)) => note,
        Ok(None) => return handle_absent(ctx, abs),
        Err(e) => {
            tracing::error!("Error processing {:?}: {}", abs, e);
            return Vec::new();
        }
    };

    if !note.id.is_empty() && !ctx.tracker.has_changed(&note.id, &note.content) {
        record_in_journal(ctx, abs, &note.id);
        tracing::debug!("Watcher ignoring echo: {}", note.path);
        return Vec::new();
    }

    if note.id.is_empty() {
        let Some(new_id) = mint_or_adopt(ctx, abs, &note) else {
            return Vec::new();
        };
        note.id = new_id;
    }

    record_in_journal(ctx, abs, &note.id);

    if ctx.tracker.is_modified(&note.id, &note.content) {
        vec![Action::UpsertNote(note)]
    } else {
        tracing::debug!("Watcher ignoring echo: {}", note.path);
        Vec::new()
    }
}

fn mint_or_adopt(ctx: &EventContext, abs: &Path, note: &Note) -> Option<String> {
    if let Some(existing) = (ctx.known_note_id)(&note.path) {
        tracing::warn!(
            "Safety Stop: Note {} has no UUID on disk, but DB knows it as {}. Skipping injection to prevent split-brain.",
            note.path,
            existing
        );
        return None;
    }

    let Ok(raw_content) = std::fs::read_to_string(abs) else {
        tracing::error!("Failed to read {} for UUID injection", note.path);
        return None;
    };
    if raw_content.contains("spacetime_id:") {
        tracing::error!(
            "CRITICAL: spacetime_id found in text but parsing failed. Skipping injection for safety: {}",
            note.path
        );
        return None;
    }

    let new_id = Uuid::new_v4().to_string();
    tracing::info!("Injecting UUID {} into {}", new_id, note.path);
    let new_content = inject_spacetime_id(&raw_content, &new_id);
    if let Err(e) = std::fs::write(abs, &new_content) {
        tracing::error!("Failed to inject UUID into {}: {}", note.path, e);
        return None;
    }
    Some(new_id)
}

fn record_in_journal(ctx: &EventContext, abs: &Path, uuid: &str) {
    let Some(journal) = ctx.journal else {
        return;
    };
    match journal::record_from_disk(ctx.vault_path, abs, uuid.to_string()) {
        Ok(record) => {
            if let Err(e) = journal.observe(&record, "create") {
                tracing::error!("Journal record failed for {}: {}", record.path, e);
            }
        }
        Err(e) => tracing::error!("Journal record failed for {:?}: {}", abs, e),
    }
}

fn delete_md(ctx: &EventContext, rel: String) -> Vec<Action> {
    if ctx.vault_path.join(&rel).exists() {
        tracing::debug!("Watcher ignoring self-write / present-file delete: {}", rel);
        return Vec::new();
    }
    let journal_uuid = ctx
        .journal
        .and_then(|j| j.by_path(&rel).ok().flatten())
        .map(|row| row.uuid);
    let from_journal = journal_uuid.is_some();
    let id = journal_uuid.or_else(|| (ctx.known_note_id)(&rel));
    let Some(id) = id else {
        tracing::warn!("Note deleted but not found in DB: {}", rel);
        return Vec::new();
    };
    if !from_journal {
        if let Some(journal) = ctx.journal {
            if let Ok(Some(row)) = journal.by_uuid(&id) {
                if row.path != rel {
                    tracing::debug!(
                        "Watcher ignoring stale delete: {} moved to {}",
                        rel,
                        row.path
                    );
                    return Vec::new();
                }
            }
        }
    }
    if let Some(journal) = ctx.journal {
        if let Err(e) = journal.tombstone(&id, journal::now_ms()) {
            tracing::error!("Journal tombstone failed for {}: {}", rel, e);
        }
    }
    vec![Action::DeleteNote { id, path: rel }]
}

fn apply_actions(
    vault_path: &Path,
    client: &SpacetimeClient,
    tracker: &ContentTracker,
    journal: Option<&Journal>,
    actions: Vec<Action>,
) {
    for action in actions {
        match action {
            Action::UpsertNote(note) => {
                backfill_ancestor_folders(client, &note.path);
                client.upsert_note(&note);
                tracker.update(&note.id, &note.content);
                tracing::debug!("Synced: {} (ID: {})", note.name, note.id);
            }
            Action::DeleteNote { id, path } => {
                client.delete_note(&id);
                tracker.remove(&id);
                tracing::info!("Deleted note: {} (ID: {})", path, id);
            }
            Action::UpsertFolder(path) => {
                client.upsert_folder(&Folder::new(path.clone()));
                tracing::debug!("Synced folder: {}", path);
            }
            Action::FolderVanished(path) => {
                handle_folder_vanished(vault_path, client, tracker, &path);
            }
            Action::Reconcile => run_full_reconcile(vault_path, client, tracker, journal),
        }
    }
}

fn handle_folder_vanished(
    vault_path: &Path,
    client: &SpacetimeClient,
    tracker: &ContentTracker,
    old_folder_path: &str,
) {
    let notes_in_folder = client.get_notes_in_folder(&format!("{}/", old_folder_path));

    for note in &notes_in_folder {
        let old_path = vault_path.join(&note.path);
        if old_path.exists() {
            continue;
        }
        match scan_for_note_by_id(vault_path, &note.id) {
            Ok(Some(mut new_note)) => {
                if new_note.id.is_empty() {
                    new_note.id = note.id.clone();
                }
                client.upsert_note(&new_note);
                tracker.update(&new_note.id, &new_note.content);
                tracing::info!("Updated note path: {} -> {}", note.path, new_note.path);
            }
            Ok(None) => {
                client.delete_note(&note.id);
                tracker.remove(&note.id);
                tracing::info!("Deleted note: {} (ID: {})", note.path, note.id);
            }
            Err(e) => {
                tracing::error!("Error scanning for note {}: {}", note.id, e);
            }
        }
    }

    client.delete_folder(old_folder_path);
    tracing::info!("Deleted folder: {}", old_folder_path);
}

fn run_full_reconcile(
    vault_path: &Path,
    client: &SpacetimeClient,
    tracker: &ContentTracker,
    journal: Option<&Journal>,
) {
    tracing::warn!("Watcher requested rescan; running full reconcile");
    if let Some(journal) = journal {
        if let Err(e) = journal::seed_from_vault(journal, vault_path) {
            tracing::error!("Journal rescan failed: {:#}", e);
        }
    }
    if let Err(e) = crate::reconcile::reconcile_on_startup(vault_path, client, tracker) {
        tracing::error!("Reconcile after rescan failed: {:#}", e);
    }
}

fn ancestor_folder_paths(note_path: &str) -> Vec<String> {
    let Some(dir) = note_path.rsplit_once('/').map(|(dir, _)| dir) else {
        return Vec::new();
    };

    let mut prefixes = Vec::new();
    let mut acc = String::new();
    for segment in dir.split('/') {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(segment);
        prefixes.push(acc.clone());
    }
    prefixes
}

fn backfill_ancestor_folders(client: &SpacetimeClient, note_path: &str) {
    for ancestor in ancestor_folder_paths(note_path) {
        let folder = Folder::new(ancestor);
        client.upsert_folder(&folder);
    }
}

pub async fn start_watcher(
    vault_path: PathBuf,
    client: Arc<SpacetimeClient>,
    tracker: Arc<ContentTracker>,
    journal: Option<Arc<Journal>>,
) -> Result<()> {
    let vault = vault_path.clone();

    let mut debouncer = new_debouncer(
        Duration::from_secs(2),
        None,
        move |res: DebounceEventResult| match res {
            Ok(events) => {
                for event in events {
                    let context = format!("watcher event ({:?} {:?})", event.kind, event.paths);
                    run_isolated(context, || {
                        let known_note_id =
                            |path: &str| client.get_note_by_path(path).map(|n| n.id);
                        let ctx = EventContext {
                            vault_path: &vault,
                            journal: journal.as_deref(),
                            tracker: &tracker,
                            known_note_id: &known_note_id,
                        };
                        let actions =
                            dispatch_event(&ctx, &event.kind, &event.paths, event.need_rescan());
                        apply_actions(&vault, &client, &tracker, journal.as_deref(), actions);
                    });
                }
            }
            Err(errors) => {
                for e in errors {
                    tracing::error!("Watch error: {:?}", e);
                }
            }
        },
    )?;

    debouncer.watch(&vault_path, RecursiveMode::Recursive)?;

    tracing::info!("Watcher started on {:?}", vault_path);

    std::future::pending::<()>().await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontmatter::extract_spacetime_id;
    use crate::journal::{hash_bytes, seed_from_vault};
    use notify_debouncer_full::notify::event::{CreateKind, DataChange, RemoveKind};

    const ID_A: &str = "11111111-1111-1111-1111-111111111111";

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "spacenotes-watcher-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_note(vault: &Path, file: &str, id: &str) {
        let content = format!("---\nspacetime_id: {}\n---\nbody of {}\n", id, file);
        std::fs::write(vault.join(file), content).unwrap();
    }

    fn seeded_vault(name: &str) -> (PathBuf, PathBuf, Journal) {
        let dir = temp_dir(name);
        let vault = dir.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        write_note(&vault, "a.md", ID_A);
        let journal = Journal::open(&dir.join("journal.db")).unwrap();
        seed_from_vault(&journal, &vault).unwrap();
        (dir, vault, journal)
    }

    fn no_known_id(_path: &str) -> Option<String> {
        None
    }

    fn upserted_note_ids_and_paths(actions: &[Action]) -> Vec<(String, String)> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::UpsertNote(note) => Some((note.id.clone(), note.path.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn rename_both_keeps_uuid() {
        let (dir, vault, journal) = seeded_vault("rename-both");
        let tracker = ContentTracker::new();
        std::fs::rename(vault.join("a.md"), vault.join("moved.md")).unwrap();

        let ctx = EventContext {
            vault_path: &vault,
            journal: Some(&journal),
            tracker: &tracker,
            known_note_id: &no_known_id,
        };
        let actions = dispatch_event(
            &ctx,
            &EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &[vault.join("a.md"), vault.join("moved.md")],
            false,
        );

        assert!(journal.by_path("a.md").unwrap().is_none());
        assert_eq!(journal.by_path("moved.md").unwrap().unwrap().uuid, ID_A);
        assert_eq!(journal.event_count("rename"), 1);
        assert_eq!(
            upserted_note_ids_and_paths(&actions),
            vec![(ID_A.to_string(), "moved.md".to_string())]
        );

        let on_disk = std::fs::read_to_string(vault.join("moved.md")).unwrap();
        assert_eq!(extract_spacetime_id(&on_disk).as_deref(), Some(ID_A));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_and_edit_in_one_tick_keeps_uuid() {
        let (dir, vault, journal) = seeded_vault("move-edit");
        let tracker = ContentTracker::new();
        std::fs::rename(vault.join("a.md"), vault.join("moved.md")).unwrap();
        let edited = format!("---\nspacetime_id: {}\n---\nedited body\n", ID_A);
        std::fs::write(vault.join("moved.md"), &edited).unwrap();

        let ctx = EventContext {
            vault_path: &vault,
            journal: Some(&journal),
            tracker: &tracker,
            known_note_id: &no_known_id,
        };
        let rename_actions = dispatch_event(
            &ctx,
            &EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &[vault.join("a.md"), vault.join("moved.md")],
            false,
        );
        let modify_actions = dispatch_event(
            &ctx,
            &EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            &[vault.join("moved.md")],
            false,
        );

        let live = journal.live_files().unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].uuid, ID_A);
        assert_eq!(live[0].path, "moved.md");
        assert_eq!(live[0].content_hash, hash_bytes(edited.as_bytes()));

        assert_eq!(
            upserted_note_ids_and_paths(&rename_actions),
            vec![(ID_A.to_string(), "moved.md".to_string())]
        );
        assert!(upserted_note_ids_and_paths(&modify_actions).is_empty());

        let on_disk = std::fs::read_to_string(vault.join("moved.md")).unwrap();
        assert_eq!(extract_spacetime_id(&on_disk).as_deref(), Some(ID_A));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn delete_note_paths(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::DeleteNote { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn remove_event_with_file_still_on_disk_is_not_a_delete() {
        let (dir, vault, journal) = seeded_vault("remove-present");
        let tracker = ContentTracker::new();

        let ctx = EventContext {
            vault_path: &vault,
            journal: Some(&journal),
            tracker: &tracker,
            known_note_id: &no_known_id,
        };
        let remove_actions = dispatch_event(
            &ctx,
            &EventKind::Remove(RemoveKind::File),
            &[vault.join("a.md")],
            false,
        );
        let rename_from_actions = dispatch_event(
            &ctx,
            &EventKind::Modify(ModifyKind::Name(RenameMode::From)),
            &[vault.join("a.md")],
            false,
        );

        assert!(vault.join("a.md").exists());
        assert!(
            delete_note_paths(&remove_actions).is_empty(),
            "Remove event for a file still on disk must not emit DeleteNote (self-write echo)"
        );
        assert!(
            delete_note_paths(&rename_from_actions).is_empty(),
            "Rename(From) event for a file still on disk must not emit DeleteNote (self-write echo)"
        );
        assert!(journal.by_path("a.md").unwrap().is_some());
        assert_eq!(journal.event_count("delete"), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_path_remove_after_move_does_not_delete_moved_row() {
        let (dir, vault, journal) = seeded_vault("move-then-old-remove");
        let tracker = ContentTracker::new();
        std::fs::rename(vault.join("a.md"), vault.join("moved.md")).unwrap();

        let ctx = EventContext {
            vault_path: &vault,
            journal: Some(&journal),
            tracker: &tracker,
            known_note_id: &no_known_id,
        };
        dispatch_event(
            &ctx,
            &EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &[vault.join("a.md"), vault.join("moved.md")],
            false,
        );

        let old_path_remove = dispatch_event(
            &ctx,
            &EventKind::Remove(RemoveKind::File),
            &[vault.join("a.md")],
            false,
        );

        assert!(delete_note_paths(&old_path_remove).is_empty());
        let live = journal.live_files().unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].uuid, ID_A);
        assert_eq!(live[0].path, "moved.md");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn known_id_a(path: &str) -> Option<String> {
        if path == "a.md" {
            Some(ID_A.to_string())
        } else {
            None
        }
    }

    #[test]
    fn skeptic_old_path_remove_with_live_db_fallback_deletes_moved_row() {
        let (dir, vault, journal) = seeded_vault("skeptic-move-db-fallback");
        let tracker = ContentTracker::new();
        std::fs::rename(vault.join("a.md"), vault.join("moved.md")).unwrap();

        let ctx = EventContext {
            vault_path: &vault,
            journal: Some(&journal),
            tracker: &tracker,
            known_note_id: &known_id_a,
        };
        dispatch_event(
            &ctx,
            &EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &[vault.join("a.md"), vault.join("moved.md")],
            false,
        );

        let old_path_remove = dispatch_event(
            &ctx,
            &EventKind::Remove(RemoveKind::File),
            &[vault.join("a.md")],
            false,
        );

        assert!(
            delete_note_paths(&old_path_remove).is_empty(),
            "old-path Remove after move must not delete the moved row via DB fallback"
        );
        let live = journal.live_files().unwrap();
        assert_eq!(live.len(), 1, "moved row must survive");
        assert_eq!(live[0].uuid, ID_A);
        assert_eq!(live[0].path, "moved.md");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_tombstones_via_journal_lookup() {
        let (dir, vault, journal) = seeded_vault("remove");
        let tracker = ContentTracker::new();
        std::fs::remove_file(vault.join("a.md")).unwrap();

        let ctx = EventContext {
            vault_path: &vault,
            journal: Some(&journal),
            tracker: &tracker,
            known_note_id: &no_known_id,
        };
        let actions = dispatch_event(
            &ctx,
            &EventKind::Remove(RemoveKind::File),
            &[vault.join("a.md")],
            false,
        );

        assert!(journal.by_path("a.md").unwrap().is_none());
        assert_eq!(journal.event_count("delete"), 1);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            Action::DeleteNote { id, path } if id == ID_A && path == "a.md"
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_injects_uuid_and_records_journal_row() {
        let (dir, vault, journal) = seeded_vault("create");
        let tracker = ContentTracker::new();
        std::fs::write(vault.join("new.md"), "fresh body\n").unwrap();

        let ctx = EventContext {
            vault_path: &vault,
            journal: Some(&journal),
            tracker: &tracker,
            known_note_id: &no_known_id,
        };
        let actions = dispatch_event(
            &ctx,
            &EventKind::Create(CreateKind::File),
            &[vault.join("new.md")],
            false,
        );

        let on_disk = std::fs::read_to_string(vault.join("new.md")).unwrap();
        let injected = extract_spacetime_id(&on_disk).expect("uuid injected into frontmatter");
        let row = journal.by_path("new.md").unwrap().unwrap();
        assert_eq!(row.uuid, injected);
        assert_eq!(journal.event_count("create"), 1);
        assert_eq!(
            upserted_note_ids_and_paths(&actions),
            vec![(injected, "new.md".to_string())]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_rename_rekeys_contained_rows() {
        let dir = temp_dir("folder-rename");
        let vault = dir.join("vault");
        std::fs::create_dir_all(vault.join("A")).unwrap();
        write_note(&vault, "A/a.md", ID_A);
        let journal = Journal::open(&dir.join("journal.db")).unwrap();
        seed_from_vault(&journal, &vault).unwrap();
        let tracker = ContentTracker::new();
        std::fs::rename(vault.join("A"), vault.join("B")).unwrap();

        let ctx = EventContext {
            vault_path: &vault,
            journal: Some(&journal),
            tracker: &tracker,
            known_note_id: &no_known_id,
        };
        let actions = dispatch_event(
            &ctx,
            &EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &[vault.join("A"), vault.join("B")],
            false,
        );

        assert_eq!(journal.by_path("B/a.md").unwrap().unwrap().uuid, ID_A);
        assert!(journal.by_path("A/a.md").unwrap().is_none());
        assert_eq!(
            upserted_note_ids_and_paths(&actions),
            vec![(ID_A.to_string(), "B/a.md".to_string())]
        );
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::UpsertFolder(p) if p == "B")));
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::FolderVanished(p) if p == "A")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn root_note_has_no_ancestors() {
        assert_eq!(ancestor_folder_paths("note.md"), Vec::<String>::new());
    }

    #[test]
    fn single_level_yields_one_ancestor() {
        assert_eq!(ancestor_folder_paths("Homelab/note.md"), vec!["Homelab"]);
    }

    #[test]
    fn two_levels_yield_two_bare_ancestors() {
        assert_eq!(
            ancestor_folder_paths("Homelab/Solar/x.md"),
            vec!["Homelab", "Homelab/Solar"]
        );
    }

    #[test]
    fn three_levels_yield_all_ancestors_no_trailing_slash() {
        assert_eq!(
            ancestor_folder_paths("A/B/C/x.md"),
            vec!["A", "A/B", "A/B/C"]
        );
    }
}
