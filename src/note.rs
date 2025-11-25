use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub path: String,
    pub name: String,
    pub content: String,
    pub folder_path: String,
    pub depth: u32,
    pub frontmatter: String,
    pub size: u64,
    pub created_time: u64,
    pub modified_time: u64,
}

impl Note {
    pub fn new(
        id: String,
        path: String,
        content: String,
        frontmatter: String,
        size: u64,
        created_time: u64,
        modified_time: u64,
    ) -> Self {
        let name = path
            .trim_end_matches(".md")
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string();

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
            frontmatter,
            size,
            created_time,
            modified_time,
        }
    }
}
