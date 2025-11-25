use anyhow::Result;
use spacetimedb_sdk::{DbContext, Table, TableWithPrimaryKey};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::folder::Folder as LocalFolder;
use crate::note::Note as LocalNote;
use crate::spacetime_bindings::{
    delete_folder_reducer::delete_folder,
    delete_note_reducer::delete_note,
    folder_table::FolderTableAccess,
    folder_type::Folder as DbFolder,
    note_table::NoteTableAccess,
    note_type::Note as DbNote,
    upsert_folder_reducer::upsert_folder,
    upsert_note_reducer::upsert_note,
    DbConnection,
};

pub struct SpacetimeClient {
    conn: DbConnection,
    synced: Arc<Mutex<bool>>,
}

impl SpacetimeClient {
    pub fn connect(host: &str, db_name: &str) -> Result<Self> {
        let synced = Arc::new(Mutex::new(false));

        let conn = DbConnection::builder()
            .with_uri(host)
            .with_module_name(db_name)
            .build()?;

        // Start the background thread first
        conn.run_threaded();

        // Subscribe to all notes and folders (separate queries)
        let synced_clone = synced.clone();
        conn.subscription_builder()
            .on_applied(move |_ctx| {
                let mut s = synced_clone.lock().unwrap();
                *s = true;
                tracing::info!("Subscription sync complete");
            })
            .on_error(|_ctx, err| {
                tracing::error!("Subscription error: {:?}", err);
            })
            .subscribe(vec![
                "SELECT * FROM note",
                "SELECT * FROM folder"
            ]);

        tracing::debug!("Subscription registered for note and folder tables");
        tracing::info!("Connected to SpacetimeDB at {}/{}", host, db_name);
        Ok(Self { conn, synced })
    }

    /// Wait for initial subscription data to be synced
    pub fn wait_for_sync(&self) -> Result<()> {
        let timeout = Duration::from_secs(30);
        let start = std::time::Instant::now();

        loop {
            {
                let synced = self.synced.lock().unwrap();
                if *synced {
                    return Ok(());
                }
            }

            if start.elapsed() > timeout {
                anyhow::bail!("Timeout waiting for subscription sync");
            }

            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Get all notes from the local cache
    pub fn get_all_notes(&self) -> Vec<LocalNote> {
        self.conn
            .db
            .note()
            .iter()
            .map(|db_note| LocalNote {
                id: db_note.id,
                path: db_note.path,
                name: db_note.name,
                content: db_note.content,
                folder_path: db_note.folder_path,
                depth: db_note.depth,
                frontmatter: db_note.frontmatter,
                size: db_note.size,
                created_time: db_note.created_time,
                modified_time: db_note.modified_time,
            })
            .collect()
    }

    /// Get all folders from the local cache
    pub fn get_all_folders(&self) -> Vec<LocalFolder> {
        self.conn
            .db
            .folder()
            .iter()
            .map(|db_folder| LocalFolder {
                path: db_folder.path,
                name: db_folder.name,
                depth: db_folder.depth,
            })
            .collect()
    }

    /// Get a note by its relative path from the local cache
    pub fn get_note_by_path(&self, path: &str) -> Option<LocalNote> {
        self.conn
            .db
            .note()
            .iter()
            .find(|n| n.path == path)
            .map(|db_note| LocalNote {
                id: db_note.id,
                path: db_note.path,
                name: db_note.name,
                content: db_note.content,
                folder_path: db_note.folder_path,
                depth: db_note.depth,
                frontmatter: db_note.frontmatter,
                size: db_note.size,
                created_time: db_note.created_time,
                modified_time: db_note.modified_time,
            })
    }

    /// Register callback for note updates
    pub fn on_note_updated<F>(&self, mut callback: F)
    where
        F: FnMut(&DbNote, &DbNote) + Send + 'static,
    {
        self.conn.db.note().on_update(move |_ctx, old, new| {
            callback(old, new);
        });
    }

    /// Register callback for note inserts
    pub fn on_note_inserted<F>(&self, mut callback: F)
    where
        F: FnMut(&DbNote) + Send + 'static,
    {
        self.conn.db.note().on_insert(move |_ctx, new| {
            callback(new);
        });
    }

    /// Register callback for note deletions
    pub fn on_note_deleted<F>(&self, mut callback: F)
    where
        F: FnMut(&DbNote) + Send + 'static,
    {
        self.conn.db.note().on_delete(move |_ctx, old| {
            callback(old);
        });
    }

    /// Register callback for folder updates
    pub fn on_folder_updated<F>(&self, mut callback: F)
    where
        F: FnMut(&DbFolder, &DbFolder) + Send + 'static,
    {
        self.conn.db.folder().on_update(move |_ctx, old, new| {
            callback(old, new);
        });
    }

    /// Register callback for folder inserts
    pub fn on_folder_inserted<F>(&self, mut callback: F)
    where
        F: FnMut(&DbFolder) + Send + 'static,
    {
        self.conn.db.folder().on_insert(move |_ctx, new| {
            callback(new);
        });
    }

    /// Register callback for folder deletions
    pub fn on_folder_deleted<F>(&self, mut callback: F)
    where
        F: FnMut(&DbFolder) + Send + 'static,
    {
        self.conn.db.folder().on_delete(move |_ctx, old| {
            callback(old);
        });
    }

    pub fn upsert_note(&self, note: &LocalNote) {
        let _ = self.conn.reducers().upsert_note(
            note.id.clone(),
            note.path.clone(),
            note.name.clone(),
            note.content.clone(),
            note.folder_path.clone(),
            note.depth,
            note.frontmatter.clone(),
            note.size,
            note.created_time,
            note.modified_time,
        );
    }

    pub fn upsert_folder(&self, folder: &LocalFolder) {
        let _ = self.conn.reducers().upsert_folder(
            folder.path.clone(),
            folder.name.clone(),
            folder.depth,
        );
    }

    pub fn sync_folders(&self, folders: &[LocalFolder]) {
        tracing::info!("Syncing {} folders to SpacetimeDB", folders.len());
        for folder in folders {
            self.upsert_folder(folder);
        }
    }

    pub fn delete_note(&self, id: &str) {
        let _ = self.conn.reducers().delete_note(id.to_string());
        tracing::debug!("Deleted note with ID: {}", id);
    }

    pub fn delete_folder(&self, path: &str) {
        let _ = self.conn.reducers().delete_folder(path.to_string());
        tracing::debug!("Deleted folder: {}", path);
    }
}
