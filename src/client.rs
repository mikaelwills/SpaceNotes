use anyhow::Result;
use spacetimedb_sdk::{DbContext, Table, TableWithPrimaryKey};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::folder::Folder as LocalFolder;
use crate::space_file::SpaceFile as LocalSpaceFile;
use crate::spacetime_bindings::{
    delete_folder_reducer::delete_folder,
    delete_file_reducer::delete_file,
    folder_table::FolderTableAccess,
    folder_type::Folder as DbFolder,
    space_file_table::SpaceFileTableAccess,
    space_file_type::SpaceFile as DbSpaceFile,
    upsert_folder_reducer::upsert_folder,
    upsert_file_reducer::upsert_file,
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
            .with_database_name(db_name)
            .build()?;

        // Start the background thread first
        conn.run_threaded();

        // Subscribe to all files and folders (separate queries)
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
                "SELECT * FROM space_file",
                "SELECT * FROM folder"
            ]);

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

    /// Get all files from the local cache
    pub fn get_all_files(&self) -> Vec<LocalSpaceFile> {
        self.conn
            .db
            .space_file()
            .iter()
            .map(|db_file| LocalSpaceFile {
                id: db_file.id,
                path: db_file.path,
                name: db_file.name,
                content: db_file.content,
                folder_path: db_file.folder_path,
                depth: db_file.depth,
                extension: db_file.extension,
                size: db_file.size,
                created_time: db_file.created_time,
                modified_time: db_file.modified_time,
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

    /// Get a file by its relative path from the local cache
    pub fn get_file_by_path(&self, path: &str) -> Option<LocalSpaceFile> {
        self.conn
            .db
            .space_file()
            .iter()
            .find(|n| n.path == path)
            .map(|db_file| LocalSpaceFile {
                id: db_file.id,
                path: db_file.path,
                name: db_file.name,
                content: db_file.content,
                folder_path: db_file.folder_path,
                depth: db_file.depth,
                extension: db_file.extension,
                size: db_file.size,
                created_time: db_file.created_time,
                modified_time: db_file.modified_time,
            })
    }

    pub fn get_file_by_id(&self, id: &str) -> Option<LocalSpaceFile> {
        self.conn
            .db
            .space_file()
            .id()
            .find(&id.to_string())
            .map(|db_file| LocalSpaceFile {
                id: db_file.id,
                path: db_file.path,
                name: db_file.name,
                content: db_file.content,
                folder_path: db_file.folder_path,
                depth: db_file.depth,
                extension: db_file.extension,
                size: db_file.size,
                created_time: db_file.created_time,
                modified_time: db_file.modified_time,
            })
    }

    pub fn get_files_in_folder(&self, folder_path_prefix: &str) -> Vec<LocalSpaceFile> {
        self.conn
            .db
            .space_file()
            .iter()
            .filter(|n| n.path.starts_with(folder_path_prefix))
            .map(|db_file| LocalSpaceFile {
                id: db_file.id,
                path: db_file.path,
                name: db_file.name,
                content: db_file.content,
                folder_path: db_file.folder_path,
                depth: db_file.depth,
                extension: db_file.extension,
                size: db_file.size,
                created_time: db_file.created_time,
                modified_time: db_file.modified_time,
            })
            .collect()
    }

    /// Register callback for file updates
    pub fn on_file_updated<F>(&self, mut callback: F)
    where
        F: FnMut(&DbSpaceFile, &DbSpaceFile) + Send + 'static,
    {
        self.conn.db.space_file().on_update(move |_ctx, old, new| {
            callback(old, new);
        });
    }

    /// Register callback for file inserts
    pub fn on_file_inserted<F>(&self, mut callback: F)
    where
        F: FnMut(&DbSpaceFile) + Send + 'static,
    {
        self.conn.db.space_file().on_insert(move |_ctx, new| {
            callback(new);
        });
    }

    /// Register callback for file deletions
    pub fn on_file_deleted<F>(&self, mut callback: F)
    where
        F: FnMut(&DbSpaceFile) + Send + 'static,
    {
        self.conn.db.space_file().on_delete(move |_ctx, old| {
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

    pub fn upsert_file(&self, file: &LocalSpaceFile) {
        let _ = self.conn.reducers().upsert_file(
            file.id.clone(),
            file.path.clone(),
            file.name.clone(),
            file.content.clone(),
            file.folder_path.clone(),
            file.depth,
            file.extension.clone(),
            file.size,
            file.created_time,
            file.modified_time,
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

    pub fn delete_file(&self, id: &str) {
        let _ = self.conn.reducers().delete_file(id.to_string());
        tracing::debug!("Deleted file with ID: {}", id);
    }

    pub fn delete_folder(&self, path: &str) {
        let _ = self.conn.reducers().delete_folder(path.to_string());
        tracing::debug!("Deleted folder: {}", path);
    }
}
