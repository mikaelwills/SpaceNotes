use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Deserialize)]
pub struct ToolCallParams {
    pub name: String,
    pub arguments: Value,
}

pub fn get_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "search_notes".to_string(),
            description: "Search notes by title, path, or content. Use this first to find notes.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query (case-insensitive, matches title/path/content)"
                    }
                },
                "required": ["query"]
            }),
        },
        Tool {
            name: "list_notes_in_folder".to_string(),
            description: "List all notes in a specific folder".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "folder_path": {
                        "type": "string",
                        "description": "Folder path (e.g., 'Development/')"
                    }
                },
                "required": ["folder_path"]
            }),
        },
        Tool {
            name: "get_note".to_string(),
            description: "Get a note's full content by ID or path".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Note UUID (optional if path provided)"},
                    "path": {"type": "string", "description": "Note path (optional if id provided)"}
                }
            }),
        },
        Tool {
            name: "create_note".to_string(),
            description: "Create a new note with content".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Full path including filename (e.g., 'Development/My Note.md')"},
                    "content": {"type": "string", "description": "Markdown content of the note"}
                },
                "required": ["path", "content"]
            }),
        },
        Tool {
            name: "delete_note".to_string(),
            description: "Delete a note by ID".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Note UUID"}
                },
                "required": ["id"]
            }),
        },
        Tool {
            name: "move_note".to_string(),
            description: "Move a note to a new path".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "old_path": {"type": "string", "description": "Current path"},
                    "new_path": {"type": "string", "description": "New path"}
                },
                "required": ["old_path", "new_path"]
            }),
        },
        Tool {
            name: "move_folder".to_string(),
            description: "Move/rename a folder and all its contents".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "old_path": {"type": "string", "description": "Current folder path"},
                    "new_path": {"type": "string", "description": "New folder path"}
                },
                "required": ["old_path", "new_path"]
            }),
        },
        Tool {
            name: "create_folder".to_string(),
            description: "Create a new folder".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Folder path with trailing slash"}
                },
                "required": ["path"]
            }),
        },
        Tool {
            name: "delete_folder".to_string(),
            description: "Delete an empty folder".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Folder path to delete"}
                },
                "required": ["path"]
            }),
        },
        Tool {
            name: "append_to_note".to_string(),
            description: "Append content to the end of an existing note".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Note path (e.g., 'Development/My Note.md')"},
                    "content": {"type": "string", "description": "Content to append"}
                },
                "required": ["path", "content"]
            }),
        },
        Tool {
            name: "prepend_to_note".to_string(),
            description: "Prepend content to the beginning of an existing note".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Note path (e.g., 'Development/My Note.md')"},
                    "content": {"type": "string", "description": "Content to prepend"}
                },
                "required": ["path", "content"]
            }),
        },
        Tool {
            name: "edit_note".to_string(),
            description: "Edit a note by finding and replacing text. More efficient than update_note_content for small changes.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Note path (e.g., 'Development/My Note.md')"},
                    "old_string": {"type": "string", "description": "Text to find (must match exactly)"},
                    "new_string": {"type": "string", "description": "Text to replace with"},
                    "replace_all": {"type": "boolean", "description": "Replace all occurrences (default: false, replaces first only)"}
                },
                "required": ["path", "old_string", "new_string"]
            }),
        },
        Tool {
            name: "move_notes_to_folder".to_string(),
            description: "Move multiple notes to a destination folder in a single operation".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Array of note paths to move"
                    },
                    "destination_folder": {
                        "type": "string",
                        "description": "Destination folder (e.g., 'Development/SpaceNotes/')"
                    }
                },
                "required": ["paths", "destination_folder"]
            }),
        },
        Tool {
            name: "regex_replace".to_string(),
            description: "Replace text using regex patterns. Powerful for bulk formatting (e.g., '\\n\\n+' -> '\\n\\n' to clean up whitespace).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Note path (e.g., 'Development/My Note.md')"},
                    "pattern": {"type": "string", "description": "Regex pattern (e.g., '\\n\\n+' for multiple newlines)"},
                    "replacement": {"type": "string", "description": "Replacement string (supports $1, $2 for capture groups)"},
                    "case_insensitive": {"type": "boolean", "description": "Case-insensitive matching (default: false)"},
                    "multiline": {"type": "boolean", "description": "Multiline mode: ^ and $ match line boundaries (default: false)"}
                },
                "required": ["path", "pattern", "replacement"]
            }),
        },
    ]
}

pub async fn execute_tool(
    client: &crate::spacetime_client::SpacetimeClient,
    params: ToolCallParams,
) -> Result<Value, String> {
    match params.name.as_str() {
        "search_notes" => {
            let query: String = serde_json::from_value(params.arguments["query"].clone())
                .map_err(|e| e.to_string())?;

            let notes = client.search_notes(&query).map_err(|e| e.to_string())?;

            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&notes).unwrap_or_else(|_| "[]".to_string())
                }]
            }))
        }
        "list_notes_in_folder" => {
            let folder_path: String =
                serde_json::from_value(params.arguments["folder_path"].clone())
                    .map_err(|e| e.to_string())?;

            let notes = client
                .list_notes_in_folder(&folder_path)
                .map_err(|e| e.to_string())?;

            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&notes).unwrap_or_else(|_| "[]".to_string())
                }]
            }))
        }
        "get_note" => {
            // Try by ID first, then by path
            let note = if let Some(id) = params.arguments.get("id").and_then(|v| v.as_str()) {
                client.get_note_by_id(id).map_err(|e| e.to_string())?
            } else if let Some(path) = params.arguments.get("path").and_then(|v| v.as_str()) {
                client.get_note_by_path(path).map_err(|e| e.to_string())?
            } else {
                return Err("Must provide either 'id' or 'path'".to_string());
            };

            match note {
                Some(n) => Ok(json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&n).unwrap_or_else(|_| "{}".to_string())
                    }]
                })),
                None => Ok(json!({"content": [{"type": "text", "text": "Note not found"}]})),
            }
        }
        "create_note" => {
            let path: String = serde_json::from_value(params.arguments["path"].clone())
                .map_err(|e| e.to_string())?;
            let content: String = serde_json::from_value(params.arguments["content"].clone())
                .map_err(|e| e.to_string())?;

            // Extract name from path
            let name = path
                .trim_end_matches(".md")
                .split('/')
                .next_back()
                .unwrap_or(&path)
                .to_string();

            // Extract folder path
            let folder_path = if path.contains('/') {
                let parts: Vec<&str> = path.rsplitn(2, '/').collect();
                format!("{}/", parts.get(1).unwrap_or(&""))
            } else {
                String::new()
            };

            // Generate UUID
            let id = uuid::Uuid::new_v4().to_string();

            client
                .create_note(id.clone(), path.clone(), name, content, folder_path)
                .map_err(|e| e.to_string())?;

            Ok(
                json!({"content": [{"type": "text", "text": format!("Created note: {} (id: {})", path, id)}]}),
            )
        }
        "delete_note" => {
            let id: String = serde_json::from_value(params.arguments["id"].clone())
                .map_err(|e| e.to_string())?;

            client.delete_note(id.clone()).map_err(|e| e.to_string())?;

            Ok(json!({"content": [{"type": "text", "text": format!("Deleted note: {}", id)}]}))
        }
        "move_note" => {
            let old_path: String = serde_json::from_value(params.arguments["old_path"].clone())
                .map_err(|e| e.to_string())?;
            let new_path: String = serde_json::from_value(params.arguments["new_path"].clone())
                .map_err(|e| e.to_string())?;

            client
                .move_note(old_path.clone(), new_path.clone())
                .map_err(|e| e.to_string())?;

            Ok(
                json!({"content": [{"type": "text", "text": format!("Moved note from {} to {}", old_path, new_path)}]}),
            )
        }
        "move_folder" => {
            let old_path: String = serde_json::from_value(params.arguments["old_path"].clone())
                .map_err(|e| e.to_string())?;
            let new_path: String = serde_json::from_value(params.arguments["new_path"].clone())
                .map_err(|e| e.to_string())?;

            client
                .move_folder(old_path.clone(), new_path.clone())
                .map_err(|e| e.to_string())?;

            Ok(
                json!({"content": [{"type": "text", "text": format!("Moved folder from {} to {}", old_path, new_path)}]}),
            )
        }
        "create_folder" => {
            let path: String = serde_json::from_value(params.arguments["path"].clone())
                .map_err(|e| e.to_string())?;
            let name = path
                .trim_end_matches('/')
                .split('/')
                .next_back()
                .unwrap_or(&path)
                .to_string();
            let depth = path.matches('/').count() as u32;

            client
                .create_folder(path.clone(), name, depth)
                .map_err(|e| e.to_string())?;

            Ok(json!({"content": [{"type": "text", "text": format!("Created folder: {}", path)}]}))
        }
        "delete_folder" => {
            let path: String = serde_json::from_value(params.arguments["path"].clone())
                .map_err(|e| e.to_string())?;

            client
                .delete_folder(path.clone())
                .map_err(|e| e.to_string())?;

            Ok(json!({"content": [{"type": "text", "text": format!("Deleted folder: {}", path)}]}))
        }
        "append_to_note" => {
            let path: String = serde_json::from_value(params.arguments["path"].clone())
                .map_err(|e| e.to_string())?;
            let content: String = serde_json::from_value(params.arguments["content"].clone())
                .map_err(|e| e.to_string())?;

            client
                .append_to_note(path.clone(), content)
                .map_err(|e| e.to_string())?;

            Ok(json!({"content": [{"type": "text", "text": format!("Appended to note: {}", path)}]}))
        }
        "prepend_to_note" => {
            let path: String = serde_json::from_value(params.arguments["path"].clone())
                .map_err(|e| e.to_string())?;
            let content: String = serde_json::from_value(params.arguments["content"].clone())
                .map_err(|e| e.to_string())?;

            client
                .prepend_to_note(path.clone(), content)
                .map_err(|e| e.to_string())?;

            Ok(json!({"content": [{"type": "text", "text": format!("Prepended to note: {}", path)}]}))
        }
        "edit_note" => {
            let path: String = serde_json::from_value(params.arguments["path"].clone())
                .map_err(|e| e.to_string())?;
            let old_string: String = serde_json::from_value(params.arguments["old_string"].clone())
                .map_err(|e| e.to_string())?;
            let new_string: String = serde_json::from_value(params.arguments["new_string"].clone())
                .map_err(|e| e.to_string())?;
            let replace_all: bool = params.arguments.get("replace_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let current_note = client.get_note_by_path(&path)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("Note not found: {}", path))?;

            if !current_note.content.contains(&old_string) {
                return Err(format!("Text not found in note: '{}'",
                    if old_string.len() > 50 { format!("{}...", &old_string[..50]) } else { old_string }));
            }

            let new_content = if replace_all {
                current_note.content.replace(&old_string, &new_string)
            } else {
                current_note.content.replacen(&old_string, &new_string, 1)
            };

            client
                .find_replace_in_note(path.clone(), old_string, new_string, replace_all)
                .map_err(|e| e.to_string())?;

            Ok(json!({"content": [{"type": "text", "text": format!("Edited note: {}\n\n---\n\n{}", path, new_content)}]}))
        }
        "move_notes_to_folder" => {
            let paths: Vec<String> = serde_json::from_value(params.arguments["paths"].clone())
                .map_err(|e| e.to_string())?;
            let destination_folder: String = serde_json::from_value(params.arguments["destination_folder"].clone())
                .map_err(|e| e.to_string())?;

            // Ensure destination folder ends with /
            let dest = if destination_folder.ends_with('/') {
                destination_folder
            } else {
                format!("{}/", destination_folder)
            };

            let mut moved = Vec::new();
            let mut errors = Vec::new();

            for old_path in paths {
                // Extract filename from old path
                let filename = old_path.split('/').last().unwrap_or(&old_path);
                let new_path = format!("{}{}", dest, filename);

                match client.move_note(old_path.clone(), new_path.clone()) {
                    Ok(_) => moved.push(format!("{} -> {}", old_path, new_path)),
                    Err(e) => errors.push(format!("{}: {}", old_path, e)),
                }
            }

            let mut result = format!("Moved {} notes to {}", moved.len(), dest);
            if !errors.is_empty() {
                result.push_str(&format!("\nErrors: {:?}", errors));
            }

            Ok(json!({"content": [{"type": "text", "text": result}]}))
        }
        "regex_replace" => {
            let path: String = serde_json::from_value(params.arguments["path"].clone())
                .map_err(|e| e.to_string())?;
            let pattern: String = serde_json::from_value(params.arguments["pattern"].clone())
                .map_err(|e| e.to_string())?;
            let replacement: String = serde_json::from_value(params.arguments["replacement"].clone())
                .map_err(|e| e.to_string())?;
            let case_insensitive: bool = params.arguments.get("case_insensitive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let multiline: bool = params.arguments.get("multiline")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let current_note = client.get_note_by_path(&path)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("Note not found: {}", path))?;

            let re = RegexBuilder::new(&pattern)
                .case_insensitive(case_insensitive)
                .multi_line(multiline)
                .build()
                .map_err(|e| format!("Invalid regex pattern: {}", e))?;

            let new_content = re.replace_all(&current_note.content, replacement.as_str()).to_string();

            if new_content == current_note.content {
                return Ok(json!({"content": [{"type": "text", "text": "No matches found - note unchanged"}]}));
            }

            let match_count = re.find_iter(&current_note.content).count();

            client
                .update_note_content(current_note.id, new_content.clone())
                .map_err(|e| e.to_string())?;

            Ok(json!({"content": [{"type": "text", "text": format!("Replaced {} matches in {}\n\n---\n\n{}", match_count, path, new_content)}]}))
        }
        _ => Err(format!("Unknown tool: {}", params.name)),
    }
}
