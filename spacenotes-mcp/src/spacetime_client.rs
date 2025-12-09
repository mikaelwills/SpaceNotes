use anyhow::Result;
use serde::Serialize;
use spacetimedb_sdk::{DbContext, Table, TableWithPrimaryKey};
use std::sync::{Arc, Mutex};

use crate::bindings::{
    append_to_note_reducer::append_to_note,
    create_folder_reducer::create_folder,
    create_note_reducer::create_note,
    delete_folder_reducer::delete_folder,
    delete_note_reducer::delete_note,
    find_replace_in_note_reducer::find_replace_in_note,
    move_folder_reducer::move_folder,
    move_note_reducer::move_note,
    note_table::NoteTableAccess,
    prepend_to_note_reducer::prepend_to_note,
    rename_note_reducer::rename_note,
    update_note_content_reducer::update_note_content,
    DbConnection,
};

pub struct SpacetimeClient {
    conn: DbConnection,
    synced: Arc<Mutex<bool>>,
}

impl SpacetimeClient {
    pub fn connect(host: &str, db_name: &str) -> Result<Self> {
        tracing::info!("Connecting to SpacetimeDB at {} (db: {})", host, db_name);

        let synced = Arc::new(Mutex::new(false));

        let conn = DbConnection::builder()
            .with_uri(host)
            .with_module_name(db_name)
            .build()?;

        // Start the background thread
        conn.run_threaded();

        // Subscribe to all notes and folders
        let synced_clone = synced.clone();
        conn.subscription_builder()
            .on_applied(move |_ctx| {
                let mut s = synced_clone.lock().unwrap();
                *s = true;
                tracing::info!("SpacetimeDB subscription sync complete");
            })
            .on_error(|_ctx, err| {
                tracing::error!("SpacetimeDB subscription error: {:?}", err);
            })
            .subscribe(vec!["SELECT * FROM note", "SELECT * FROM folder"]);

        tracing::info!("SpacetimeDB connection established");

        Ok(Self { conn, synced })
    }

    pub fn rename_note(&self, id: String, new_path: String) -> Result<()> {
        tracing::info!("Renaming note {} to {}", id, new_path);

        // Call the rename_note reducer
        self.conn.reducers().rename_note(id, new_path)?;

        Ok(())
    }

    pub fn delete_note(&self, id: String) -> Result<()> {
        tracing::info!("Deleting note {}", id);

        // Call the delete_note reducer
        self.conn.reducers().delete_note(id)?;

        Ok(())
    }

    pub fn create_folder(&self, path: String, name: String, depth: u32) -> Result<()> {
        tracing::info!("Creating folder {} at depth {}", path, depth);

        // Call the create_folder reducer
        self.conn.reducers().create_folder(path, name, depth)?;

        Ok(())
    }

    pub fn list_notes_in_folder(&self, folder_path: &str) -> Result<Vec<NoteInfo>> {
        tracing::info!("Listing notes in folder: {}", folder_path);

        // Query from the local note table cache
        let notes: Vec<NoteInfo> = self
            .conn
            .db()
            .note()
            .iter()
            .filter(|note| note.folder_path == folder_path)
            .map(|note| NoteInfo {
                id: note.id.clone(),
                path: note.path.clone(),
                name: note.name.clone(),
            })
            .collect();

        tracing::info!("Found {} notes in folder {}", notes.len(), folder_path);

        Ok(notes)
    }

    pub fn get_note_by_id(&self, id: &str) -> Result<Option<FullNote>> {
        tracing::info!("Getting note by id: {}", id);

        let note = self
            .conn
            .db()
            .note()
            .id()
            .find(&id.to_string())
            .map(|note| FullNote {
                id: note.id.clone(),
                path: note.path.clone(),
                name: note.name.clone(),
                content: note.content.clone(),
                folder_path: note.folder_path.clone(),
                frontmatter: note.frontmatter.clone(),
            });

        Ok(note)
    }

    pub fn get_note_by_path(&self, path: &str) -> Result<Option<FullNote>> {
        tracing::info!("Getting note by path: {}", path);

        let note = self
            .conn
            .db()
            .note()
            .path()
            .find(&path.to_string())
            .map(|note| FullNote {
                id: note.id.clone(),
                path: note.path.clone(),
                name: note.name.clone(),
                content: note.content.clone(),
                folder_path: note.folder_path.clone(),
                frontmatter: note.frontmatter.clone(),
            });

        Ok(note)
    }

    pub fn create_note(
        &self,
        id: String,
        path: String,
        name: String,
        content: String,
        folder_path: String,
    ) -> Result<()> {
        tracing::info!("Creating note: {} at {}", name, path);

        let depth = path.matches('/').count() as u32;
        let size = content.len() as u64;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.conn.reducers().create_note(
            id,
            path,
            name,
            content,
            folder_path,
            depth,
            String::new(), // frontmatter
            size,
            now,
            now,
        )?;

        Ok(())
    }

    pub fn update_note_content(&self, id: String, content: String) -> Result<()> {
        tracing::info!("Updating note content: {}", id);

        let size = content.len() as u64;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.conn.reducers().update_note_content(
            id,
            content,
            String::new(), // frontmatter - keep existing or empty
            size,
            now,
        )?;

        Ok(())
    }

    pub fn move_note(&self, old_path: String, new_path: String) -> Result<()> {
        tracing::info!("Moving note from {} to {}", old_path, new_path);
        self.conn.reducers().move_note(old_path, new_path)?;
        Ok(())
    }

    pub fn move_folder(&self, old_path: String, new_path: String) -> Result<()> {
        tracing::info!("Moving folder from {} to {}", old_path, new_path);
        self.conn.reducers().move_folder(old_path, new_path)?;
        Ok(())
    }

    pub fn delete_folder(&self, path: String) -> Result<()> {
        tracing::info!("Deleting folder: {}", path);
        self.conn.reducers().delete_folder(path)?;
        Ok(())
    }

    pub fn append_to_note(&self, path: String, content: String) -> Result<()> {
        tracing::info!("Appending to note: {}", path);
        self.conn.reducers().append_to_note(path, content)?;
        Ok(())
    }

    pub fn prepend_to_note(&self, path: String, content: String) -> Result<()> {
        tracing::info!("Prepending to note: {}", path);
        self.conn.reducers().prepend_to_note(path, content)?;
        Ok(())
    }

    pub fn find_replace_in_note(
        &self,
        path: String,
        old_text: String,
        new_text: String,
        replace_all: bool,
    ) -> Result<()> {
        tracing::info!("Find/replace in note: {}", path);
        self.conn
            .reducers()
            .find_replace_in_note(path, old_text, new_text, replace_all)?;
        Ok(())
    }

    pub fn search_notes(&self, query: &str) -> Result<Vec<NoteInfo>> {
        tracing::info!("Searching notes for: {}", query);

        let query_lower = query.to_lowercase();

        let notes: Vec<NoteInfo> = self
            .conn
            .db()
            .note()
            .iter()
            .filter(|note| {
                note.name.to_lowercase().contains(&query_lower)
                    || note.path.to_lowercase().contains(&query_lower)
                    || note.content.to_lowercase().contains(&query_lower)
            })
            .map(|note| NoteInfo {
                id: note.id.clone(),
                path: note.path.clone(),
                name: note.name.clone(),
            })
            .collect();

        tracing::info!("Found {} notes matching '{}'", notes.len(), query);

        Ok(notes)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NoteInfo {
    pub id: String,
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FullNote {
    pub id: String,
    pub path: String,
    pub name: String,
    pub content: String,
    pub folder_path: String,
    pub frontmatter: String,
}
