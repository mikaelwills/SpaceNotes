use anyhow::Result;
use std::path::Path;
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

use crate::folder::Folder;
use crate::frontmatter::{extract_spacetime_id, parse_frontmatter};
use crate::note::Note;
use crate::sanitize::sanitize_path;

pub fn read_note_at(vault_path: &Path, abs_path: &Path) -> Result<Option<Note>> {
    // Validation
    if !abs_path.exists() || !abs_path.is_file() {
        return Ok(None);
    }

    // Skip non-markdown
    if abs_path.extension().map_or(true, |e| e != "md") {
        return Ok(None);
    }

    // Relative path - sanitize to prevent URI encoding issues
    let rel_path = sanitize_path(&abs_path
        .strip_prefix(vault_path)?
        .to_string_lossy()
        .to_string());

    // Read content
    let content = std::fs::read_to_string(abs_path)?;

    // Extract UUID (READ-ONLY - do not inject here)
    let id = extract_spacetime_id(&content).unwrap_or_default();

    let metadata = std::fs::metadata(abs_path)?;

    let size = metadata.len();
    let modified = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)?
        .as_millis() as u64;
    let created = metadata
        .created()
        .unwrap_or_else(|_| metadata.modified().unwrap())
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(modified);

    // Parse frontmatter
    let (body, frontmatter) = parse_frontmatter(&content);

    Ok(Some(Note::new(id, rel_path, body, frontmatter, size, created, modified)))
}

/// Scan filesystem to find a note by its UUID
pub fn scan_for_note_by_id(vault_path: &Path, target_id: &str) -> Result<Option<Note>> {
    let walker = WalkDir::new(vault_path).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        !name.starts_with('.') && name != "@eaDir"
    });

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();

        if !path.is_file() || path.extension().map_or(true, |e| e != "md") {
            continue;
        }

        if let Ok(content) = std::fs::read_to_string(path) {
            if let Some(id) = extract_spacetime_id(&content) {
                if id == target_id {
                    // Found it! Read the full note
                    return read_note_at(vault_path, path);
                }
            }
        }
    }

    Ok(None)
}

pub fn scan_notes(vault_path: &Path) -> Result<Vec<Note>> {
    let mut notes = Vec::new();

    // Optimization: filter_entry prevents descending into hidden directories
    let walker = WalkDir::new(vault_path).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        !name.starts_with('.') && name != "@eaDir"
    });

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();

        // Skip non-markdown files
        if !path.is_file() || path.extension().map_or(true, |e| e != "md") {
            continue;
        }

        // Get relative path - sanitize to prevent URI encoding issues
        let rel_path = match path.strip_prefix(vault_path) {
            Ok(p) => sanitize_path(&p.to_string_lossy().to_string()),
            Err(e) => {
                tracing::warn!("Failed to get relative path for {:?}: {}", path, e);
                continue;
            }
        };

        // Read file content
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to read {:?}: {}", path, e);
                continue;
            }
        };

        // Extract UUID (READ-ONLY - do not inject here)
        // Notes without UUIDs will be skipped during initial scan
        let Some(id) = extract_spacetime_id(&content) else {
            tracing::debug!("Skipping note without UUID: {}", rel_path);
            continue;
        };

        // Get metadata
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("Failed to get metadata for {:?}: {}", path, e);
                continue;
            }
        };

        let size = metadata.len();
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let created = metadata
            .created()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(modified);

        // Parse frontmatter
        let (body, frontmatter) = parse_frontmatter(&content);

        let note = Note::new(id, rel_path, body, frontmatter, size, created, modified);
        notes.push(note);
    }

    Ok(notes)
}

pub fn scan_folders(vault_path: &Path) -> Result<Vec<Folder>> {
    let mut folders = Vec::new();

    // Optimization: filter_entry prevents descending into hidden directories
    let walker = WalkDir::new(vault_path).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        !name.starts_with('.') && name != "@eaDir"
    });

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();

        // Must be a directory, and must not be the root itself
        if !path.is_dir() || path == vault_path {
            continue;
        }

        // Get relative path - sanitize to prevent URI encoding issues
        let rel_path = sanitize_path(&path.strip_prefix(vault_path)?.to_string_lossy().to_string());

        folders.push(Folder::new(rel_path));
    }

    Ok(folders)
}
