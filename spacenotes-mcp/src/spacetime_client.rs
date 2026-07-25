use anyhow::Result;
use regex::Regex;
use serde::Serialize;
use spacetimedb_sdk::{DbContext, Table, TableWithPrimaryKey};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio::time::timeout;

fn spacenote_link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\[([^\]]+)\]\(spacenote:([0-9a-f-]{36})\)")
            .expect("valid spacenote link regex")
    })
}

use crate::bindings::{
    append_to_file_reducer::append_to_file,
    clear_all_sessions_reducer::clear_all_sessions,
    create_folder_reducer::create_folder,
    create_file_reducer::create_file,
    delete_folder_reducer::delete_folder,
    delete_file_reducer::delete_file,
    delete_session_reducer::delete_session as delete_session_reducer_fn,
    find_replace_in_file_reducer::find_replace_in_file,
    folder_table::FolderTableAccess,
    move_folder_reducer::move_folder,
    move_file_reducer::move_file,
    space_file_table::SpaceFileTableAccess,
    prepend_to_file_reducer::prepend_to_file,
    rename_file_reducer::rename_file,
    session_activity_table::SessionActivityTableAccess,
    session_table::SessionTableAccess,
    update_file_content_reducer::update_file_content,
    DbConnection,
};

// A reducer panic arrives as the OUTER error; a returned Err as the inner one.
fn flatten_outcome(
    outcome: Result<Result<(), String>, spacetimedb_sdk::__codegen::InternalError>,
) -> Result<(), String> {
    match outcome {
        Ok(inner) => inner,
        Err(panicked) => Err(panicked.to_string()),
    }
}

fn full_file(file: crate::bindings::space_file_type::SpaceFile) -> FullSpaceFile {
    FullSpaceFile {
        id: file.id.clone(),
        path: file.path.clone(),
        name: file.name.clone(),
        content: file.content.clone(),
        folder_path: file.folder_path.clone(),
    }
}

const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const REDUCER_TIMEOUT: Duration = Duration::from_secs(10);

pub struct SpacetimeClient {
    conn: DbConnection,
    ready: watch::Sender<bool>,
    // watch::Sender::send is a no-op once every Receiver is dropped, which would leave the
    // gate permanently closed. Holding one keeps the channel live.
    _ready_keepalive: watch::Receiver<bool>,
}

impl SpacetimeClient {
    pub fn connect(host: &str, db_name: &str) -> Result<Self> {
        tracing::info!("Connecting to SpacetimeDB at {} (db: {})", host, db_name);

        let (ready, ready_keepalive) = watch::channel(false);

        let conn = DbConnection::builder()
            .with_uri(host)
            .with_database_name(db_name)
            .build()?;

        conn.run_threaded();

        let applied = ready.clone();
        let dropped = ready.clone();
        conn.subscription_builder()
            .on_applied(move |_ctx| {
                let _ = applied.send(true);
                tracing::info!("SpacetimeDB subscription sync complete");
            })
            .on_error(move |_ctx, err| {
                let _ = dropped.send(false);
                tracing::error!("SpacetimeDB subscription error: {:?}", err);
            })
            .subscribe(vec![
                "SELECT * FROM space_file",
                "SELECT * FROM folder",
                "SELECT * FROM session",
                "SELECT * FROM session_activity",
            ]);

        tracing::info!("SpacetimeDB connection established");

        Ok(Self {
            conn,
            ready,
            _ready_keepalive: ready_keepalive,
        })
    }

    // Reducers are submitted asynchronously: the socket accepting a call says nothing about
    // whether it committed. Callers that need the truth await the reducer's own Result here.
    //
    // A timeout means UNKNOWN, not failure — the write may have committed after we stopped
    // waiting, so retrying blindly can duplicate content.
    async fn await_reducer(
        &self,
        what: &str,
        outcome: oneshot::Receiver<Result<(), String>>,
    ) -> Result<()> {
        match timeout(REDUCER_TIMEOUT, outcome).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(reducer_error))) => anyhow::bail!("{} failed: {}", what, reducer_error),
            Ok(Err(_)) => anyhow::bail!(
                "{}: outcome unknown (connection closed before it was confirmed). \
                 Re-read the note before retrying.",
                what
            ),
            Err(_) => anyhow::bail!(
                "{}: outcome unknown (no confirmation within {}s). \
                 Re-read the note before retrying.",
                what,
                REDUCER_TIMEOUT.as_secs()
            ),
        }
    }

    // Reads come from the subscription cache, so an unsynced cache answers "not found" for
    // notes that exist. Callers must await this before touching it.
    pub async fn await_ready(&self) -> Result<()> {
        if *self.ready.borrow() {
            return Ok(());
        }
        let mut rx = self.ready.subscribe();
        match timeout(READINESS_TIMEOUT, async {
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    return false;
                }
            }
            true
        })
        .await
        {
            Ok(true) => Ok(()),
            Ok(false) => anyhow::bail!("SpacetimeDB connection closed while syncing; retry"),
            Err(_) => anyhow::bail!(
                "SpacetimeDB still syncing after {}s; retry shortly",
                READINESS_TIMEOUT.as_secs()
            ),
        }
    }

    pub async fn rename_file(&self, id: String, new_path: String) -> Result<()> {
        tracing::info!("Renaming note {} to {}", id, new_path);

        // Call the rename_file reducer
        let (tx, rx) = oneshot::channel();
        self.conn.reducers().rename_file_then(id, new_path, move |_ctx, outcome| {
            let _ = tx.send(flatten_outcome(outcome));
        })?;
        self.await_reducer("rename_file", rx).await
    }

    pub async fn delete_file(&self, id: String) -> Result<()> {
        tracing::info!("Deleting note {}", id);

        // Call the delete_file reducer
        let (tx, rx) = oneshot::channel();
        self.conn.reducers().delete_file_then(id, move |_ctx, outcome| {
            let _ = tx.send(flatten_outcome(outcome));
        })?;
        self.await_reducer("delete_file", rx).await
    }

    pub async fn create_folder(&self, path: String, name: String, depth: u32) -> Result<()> {
        tracing::info!("Creating folder {} at depth {}", path, depth);

        // Call the create_folder reducer
        let (tx, rx) = oneshot::channel();
        self.conn.reducers().create_folder_then(path, name, depth, move |_ctx, outcome| {
            let _ = tx.send(flatten_outcome(outcome));
        })?;
        self.await_reducer("create_folder", rx).await
    }

    pub fn list_folder(&self, folder_path: &str) -> Result<Vec<FolderEntry>> {
        tracing::info!("Listing folder: {}", folder_path);

        // Folder paths are stored WITHOUT a trailing slash; file.folder_path is
        // stored WITH one. Normalize the input to match each.
        let parent = folder_path.trim_end_matches('/');
        let file_folder = if parent.is_empty() {
            String::new()
        } else {
            format!("{}/", parent)
        };

        let mut entries: Vec<FolderEntry> = self
            .conn
            .db()
            .folder()
            .iter()
            .filter(|f| f.path.rsplit_once('/').map(|(p, _)| p).unwrap_or("") == parent)
            .map(|f| FolderEntry {
                entry_type: "folder".to_string(),
                name: f.name.clone(),
                path: f.path.clone(),
                id: None,
            })
            .collect();

        entries.extend(
            self.conn
                .db()
                .space_file()
                .iter()
                .filter(|file| file.folder_path == file_folder)
                .map(|file| FolderEntry {
                    entry_type: "note".to_string(),
                    name: file.name.clone(),
                    path: file.path.clone(),
                    id: Some(file.id.clone()),
                }),
        );

        entries.sort_by(|a, b| {
            a.entry_type
                .cmp(&b.entry_type)
                .then_with(|| a.name.cmp(&b.name))
        });

        tracing::info!("Found {} entries in folder {}", entries.len(), parent);

        Ok(entries)
    }

    pub fn get_file_by_id(&self, id: &str) -> Result<Option<FullSpaceFile>> {
        tracing::info!("Getting note by id: {}", id);

        let file = self
            .conn
            .db()
            .space_file()
            .id()
            .find(&id.to_string())
            .map(|file| FullSpaceFile {
                id: file.id.clone(),
                path: file.path.clone(),
                name: file.name.clone(),
                content: file.content.clone(),
                folder_path: file.folder_path.clone(),
            });

        Ok(file)
    }

    pub fn get_file_by_path(&self, path: &str) -> Result<Option<FullSpaceFile>> {
        tracing::info!("Getting note by path: {}", path);

        let file = self
            .conn
            .db()
            .space_file()
            .path()
            .find(&path.to_string())
            .map(|file| FullSpaceFile {
                id: file.id.clone(),
                path: file.path.clone(),
                name: file.name.clone(),
                content: file.content.clone(),
                folder_path: file.folder_path.clone(),
            });

        Ok(file)
    }

    pub fn get_files_by_paths(&self, paths: &[String]) -> Result<Vec<FullSpaceFile>> {
        tracing::info!("Getting {} notes by paths", paths.len());

        let files: Vec<FullSpaceFile> = paths
            .iter()
            .filter_map(|path| {
                self.conn
                    .db()
                    .space_file()
                    .path()
                    .find(path)
                    .map(|file| FullSpaceFile {
                        id: file.id.clone(),
                        path: file.path.clone(),
                        name: file.name.clone(),
                        content: file.content.clone(),
                        folder_path: file.folder_path.clone(),
                    })
            })
            .collect();

        tracing::info!("Found {} of {} requested notes", files.len(), paths.len());
        Ok(files)
    }

    pub fn get_files_by_ids(&self, ids: &[String]) -> Result<Vec<FullSpaceFile>> {
        tracing::info!("Getting {} notes by ids", ids.len());

        let files: Vec<FullSpaceFile> = ids
            .iter()
            .filter_map(|id| {
                self.conn
                    .db()
                    .space_file()
                    .id()
                    .find(id)
                    .map(|file| FullSpaceFile {
                        id: file.id.clone(),
                        path: file.path.clone(),
                        name: file.name.clone(),
                        content: file.content.clone(),
                        folder_path: file.folder_path.clone(),
                    })
            })
            .collect();

        tracing::info!("Found {} of {} requested notes", files.len(), ids.len());
        Ok(files)
    }

    pub async fn create_file(
        &self,
        id: String,
        path: String,
        name: String,
        content: String,
        folder_path: String,
    ) -> Result<()> {
        tracing::info!("Creating note: {} at {}", name, path);

        let depth = path.matches('/').count() as u32;
        let extension = extension_of(&path);
        let size = content.len() as u64;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let (tx, rx) = oneshot::channel();
        self.conn.reducers().create_file_then(id,
            path,
            name,
            content,
            folder_path,
            depth,
            extension,
            size,
            now,
            now, move |_ctx, outcome| {
            let _ = tx.send(flatten_outcome(outcome));
        })?;
        self.await_reducer("create_file", rx).await
    }

    pub async fn update_file_content(&self, id: String, content: String) -> Result<()> {
        tracing::info!("Updating note content: {}", id);

        let size = content.len() as u64;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let (tx, rx) = oneshot::channel();
        self.conn.reducers().update_file_content_then(id,
            content,
            size,
            now, move |_ctx, outcome| {
            let _ = tx.send(flatten_outcome(outcome));
        })?;
        self.await_reducer("update_file_content", rx).await
    }

    pub async fn move_file(&self, old_path: String, new_path: String) -> Result<()> {
        tracing::info!("Moving note from {} to {}", old_path, new_path);
        let (tx, rx) = oneshot::channel();
        self.conn.reducers().move_file_then(old_path, new_path, move |_ctx, outcome| {
            let _ = tx.send(flatten_outcome(outcome));
        })?;
        self.await_reducer("move_file", rx).await
    }

    pub async fn move_folder(&self, old_path: String, new_path: String) -> Result<()> {
        tracing::info!("Moving folder from {} to {}", old_path, new_path);
        let (tx, rx) = oneshot::channel();
        self.conn.reducers().move_folder_then(old_path, new_path, move |_ctx, outcome| {
            let _ = tx.send(flatten_outcome(outcome));
        })?;
        self.await_reducer("move_folder", rx).await
    }

    pub async fn delete_folder(&self, path: String) -> Result<()> {
        tracing::info!("Deleting folder: {}", path);
        let (tx, rx) = oneshot::channel();
        self.conn.reducers().delete_folder_then(path, move |_ctx, outcome| {
            let _ = tx.send(flatten_outcome(outcome));
        })?;
        self.await_reducer("delete_folder", rx).await
    }

    pub async fn append_to_file(&self, path: String, content: String) -> Result<()> {
        tracing::info!("Appending to note: {}", path);
        let (tx, rx) = oneshot::channel();
        self.conn.reducers().append_to_file_then(path, content, move |_ctx, outcome| {
            let _ = tx.send(flatten_outcome(outcome));
        })?;
        self.await_reducer("append_to_file", rx).await
    }

    pub async fn prepend_to_file(&self, path: String, content: String) -> Result<()> {
        tracing::info!("Prepending to note: {}", path);
        let (tx, rx) = oneshot::channel();
        self.conn.reducers().prepend_to_file_then(path, content, move |_ctx, outcome| {
            let _ = tx.send(flatten_outcome(outcome));
        })?;
        self.await_reducer("prepend_to_file", rx).await
    }

    pub async fn find_replace_in_file(
        &self,
        path: String,
        old_text: String,
        new_text: String,
        replace_all: bool,
    ) -> Result<()> {
        tracing::info!("Find/replace in note: {}", path);
        let (tx, rx) = oneshot::channel();
        self.conn.reducers().find_replace_in_file_then(
            path.clone(),
            old_text,
            new_text,
            replace_all,
            move |_ctx, outcome| {
                let _ = tx.send(flatten_outcome(outcome));
            },
        )?;
        self.await_reducer(&format!("edit of '{}'", path), rx).await
    }

    // The filewatcher creates folder rows as it ingests; MCP-created notes need the same, or
    // their folders never appear in list_folder on the parent.
    pub async fn ensure_folder_ancestry(&self, folder_path: &str) -> Result<()> {
        let trimmed = folder_path.trim_end_matches('/');
        if trimmed.is_empty() {
            return Ok(());
        }
        let existing: HashSet<String> =
            self.conn.db().folder().iter().map(|f| f.path.clone()).collect();

        let mut prefix = String::new();
        for segment in trimmed.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(segment);
            if existing.contains(&prefix) {
                continue;
            }
            let depth = prefix.matches('/').count() as u32;
            // Racing the filewatcher here is fine: a duplicate is reported and ignored.
            if let Err(e) = self
                .create_folder(prefix.clone(), segment.to_string(), depth)
                .await
            {
                tracing::debug!("ancestry backfill skipped {}: {}", prefix, e);
            }
        }
        Ok(())
    }

    pub fn files_in_scope(
        &self,
        folder: Option<&str>,
        paths: Option<&[String]>,
    ) -> Result<Vec<FullSpaceFile>> {
        if folder.is_some() && paths.is_some() {
            anyhow::bail!("provide folder or paths, not both");
        }
        let mut out: Vec<FullSpaceFile> = match (folder, paths) {
            (Some(folder), _) => {
                // An empty prefix would silently scope to the entire vault.
                if folder.trim_matches('/').is_empty() {
                    anyhow::bail!("folder must not be empty; pass a real folder or use paths");
                }
                let prefix = if folder.ends_with('/') {
                    folder.to_string()
                } else {
                    format!("{}/", folder)
                };
                self.conn
                    .db()
                    .space_file()
                    .iter()
                    .filter(|f| f.path.starts_with(&prefix))
                    .map(full_file)
                    .collect()
            }
            (None, Some(paths)) => {
                let wanted: HashSet<&str> = paths.iter().map(|p| p.as_str()).collect();
                self.conn
                    .db()
                    .space_file()
                    .iter()
                    .filter(|f| wanted.contains(f.path.as_str()))
                    .map(full_file)
                    .collect()
            }
            (None, None) => Vec::new(),
        };
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    pub fn search_files(&self, query: &str, context_lines: Option<u32>) -> Result<Vec<SearchResult>> {
        tracing::info!("Searching notes for: {} (context_lines: {:?})", query, context_lines);

        let tokens: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();

        let mut files: Vec<(usize, SearchResult)> = self
            .conn
            .db()
            .space_file()
            .iter()
            .filter_map(|file| {
                let name_lower = file.name.to_lowercase();
                let path_lower = file.path.to_lowercase();
                let content_lower = file.content.to_lowercase();

                let match_count = tokens
                    .iter()
                    .filter(|token| {
                        name_lower.contains(*token)
                            || path_lower.contains(*token)
                            || content_lower.contains(*token)
                    })
                    .count();

                if match_count == 0 {
                    return None;
                }

                let excerpts = context_lines.map(|ctx| {
                    let lines: Vec<&str> = file.content.lines().collect();
                    let total = lines.len();
                    let ctx = ctx as usize;
                    let mut matched_ranges: Vec<(usize, usize)> = Vec::new();

                    for (i, line) in lines.iter().enumerate() {
                        let line_lower = line.to_lowercase();
                        if tokens.iter().any(|t| line_lower.contains(t)) {
                            let start = i.saturating_sub(ctx);
                            let end = (i + ctx + 1).min(total);
                            matched_ranges.push((start, end));
                        }
                    }

                    let merged = merge_ranges(&matched_ranges);

                    merged
                        .into_iter()
                        .map(|(start, end)| {
                            let snippet: String = lines[start..end]
                                .iter()
                                .enumerate()
                                .map(|(j, line)| format!("{:>4}| {}", start + j + 1, line))
                                .collect::<Vec<_>>()
                                .join("\n");
                            format!("[lines {}-{}]\n{}", start + 1, end, snippet)
                        })
                        .collect()
                });

                Some((
                    match_count,
                    SearchResult {
                        id: file.id.clone(),
                        path: file.path.clone(),
                        name: file.name.clone(),
                        excerpts,
                    },
                ))
            })
            .collect();

        files.sort_by(|a, b| b.0.cmp(&a.0));

        let files: Vec<SearchResult> = files.into_iter().map(|(_, file)| file).collect();

        tracing::info!("Found {} notes matching '{}'", files.len(), query);

        Ok(files)
    }

    pub fn get_outbound_links(&self, id: &str) -> Result<Vec<Link>> {
        tracing::info!("Getting outbound links for note {}", id);

        let Some(file) = self
            .conn
            .db()
            .space_file()
            .id()
            .find(&id.to_string())
        else {
            return Ok(Vec::new());
        };

        let id_to_meta: HashMap<String, (String, String)> = self
            .conn
            .db()
            .space_file()
            .iter()
            .map(|n| (n.id.clone(), (n.name.clone(), n.path.clone())))
            .collect();

        let mut seen = HashSet::new();
        let mut links = Vec::new();
        for cap in spacenote_link_re().captures_iter(&file.content) {
            let target_id = cap.get(2).unwrap().as_str().to_string();
            if !seen.insert(target_id.clone()) {
                continue;
            }
            let (name, path, broken) = match id_to_meta.get(&target_id) {
                Some((n, p)) => (n.clone(), p.clone(), false),
                None => (String::new(), String::new(), true),
            };
            links.push(Link {
                id: target_id,
                name,
                path,
                broken,
            });
        }

        tracing::info!("Found {} outbound links for {}", links.len(), id);
        Ok(links)
    }

    pub fn get_backlinks(&self, id: &str) -> Result<Vec<Link>> {
        tracing::info!("Getting backlinks for note {}", id);

        let target_exists = self
            .conn
            .db()
            .space_file()
            .id()
            .find(&id.to_string())
            .is_some();

        let mut seen = HashSet::new();
        let mut links = Vec::new();
        for file in self.conn.db().space_file().iter() {
            if file.id == id {
                continue;
            }
            let mut references_target = false;
            for cap in spacenote_link_re().captures_iter(&file.content) {
                if cap.get(2).unwrap().as_str() == id {
                    references_target = true;
                    break;
                }
            }
            if references_target && seen.insert(file.id.clone()) {
                links.push(Link {
                    id: file.id.clone(),
                    name: file.name.clone(),
                    path: file.path.clone(),
                    broken: !target_exists,
                });
            }
        }

        tracing::info!("Found {} backlinks for {}", links.len(), id);
        Ok(links)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        tracing::info!("Listing all SpaceChannel sessions");

        let activity: HashMap<String, (String, i64)> = self
            .conn
            .db()
            .session_activity()
            .iter()
            .map(|a| {
                (
                    a.session_id.clone(),
                    (a.state.clone(), a.updated_at.to_micros_since_unix_epoch()),
                )
            })
            .collect();

        let mut sessions: Vec<SessionInfo> = self
            .conn
            .db()
            .session()
            .iter()
            .map(|s| {
                let act = activity.get(&s.id);
                SessionInfo {
                    id: s.id.clone(),
                    base_name: s.base_name.clone(),
                    host: s.host.clone(),
                    last_seen_us: s.last_seen.to_micros_since_unix_epoch(),
                    state: act.map(|(state, _)| state.clone()),
                    activity_updated_at_us: act.map(|(_, ts)| *ts),
                }
            })
            .collect();

        sessions.sort_by(|a, b| b.last_seen_us.cmp(&a.last_seen_us));
        Ok(sessions)
    }

    pub fn delete_session(&self, session_id: String) -> Result<()> {
        tracing::info!("Deleting session {}", session_id);
        self.conn.reducers().delete_session(session_id)?;
        Ok(())
    }

    pub fn clear_all_sessions(&self) -> Result<()> {
        tracing::info!("Clearing all SpaceChannel sessions");
        self.conn.reducers().clear_all_sessions()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub base_name: String,
    pub host: String,
    pub last_seen_us: i64,
    pub state: Option<String>,
    pub activity_updated_at_us: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Link {
    pub id: String,
    pub name: String,
    pub path: String,
    pub broken: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderEntry {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub id: String,
    pub path: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpts: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FullSpaceFile {
    pub id: String,
    pub path: String,
    pub name: String,
    pub content: String,
    pub folder_path: String,
}

fn extension_of(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    match base.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_lowercase(),
        _ => String::new(),
    }
}

fn merge_ranges(ranges: &[(usize, usize)]) -> Vec<(usize, usize)> {
    if ranges.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<(usize, usize)> = ranges.to_vec();
    sorted.sort_by_key(|r| r.0);
    let mut merged = vec![sorted[0]];
    for &(start, end) in &sorted[1..] {
        let last = merged.last_mut().unwrap();
        if start <= last.1 {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}
