use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub path: String,
    pub name: String,
    pub content: String,
    pub folder_path: String,
    pub depth: u32,
    pub extension: String,
    pub kind: String,
    pub size: u64,
    pub created_time: u64,
    pub modified_time: u64,
}

pub fn extension_of(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    match base.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_lowercase(),
        _ => String::new(),
    }
}

pub fn kind_of(extension: &str) -> String {
    if extension == "md" {
        "md".to_string()
    } else {
        "file".to_string()
    }
}

impl Note {
    pub fn new(
        id: String,
        path: String,
        content: String,
        size: u64,
        created_time: u64,
        modified_time: u64,
    ) -> Self {
        let extension = extension_of(&path);
        let kind = kind_of(&extension);

        let base = path.rsplit('/').next().unwrap_or("");
        let name = match base.rsplit_once('.') {
            Some((stem, _)) if !stem.is_empty() => stem.to_string(),
            _ => base.to_string(),
        };

        let folder_path = match path.rfind('/') {
            Some(idx) => format!("{}/", &path[..idx]),
            None => String::new(),
        };

        let depth = path.matches('/').count() as u32;

        Self {
            id,
            path,
            name,
            content,
            folder_path,
            depth,
            extension,
            kind,
            size,
            created_time,
            modified_time,
        }
    }
}
