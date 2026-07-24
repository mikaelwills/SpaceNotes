use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

fn normalize_for_matching(s: &str) -> String {
    s.replace("\r\n", "\n")
        .lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

fn numbered(content: &str) -> String {
    content
        .lines()
        .enumerate()
        .map(|(i, line)| format!("{:>4}| {}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_note(n: &crate::spacetime_client::FullNote) -> String {
    format!(
        "id: {}\npath: {}\nname: {}\nfolder_path: {}\nfrontmatter: {}\n\n{}",
        n.id, n.path, n.name, n.folder_path, n.frontmatter, numbered(&n.content)
    )
}

// The latest session note for a workflow, formatted, or a "no sessions" line.
// Newest = max date, then lowest N within that date (create_session makes the
// new file `-1-` and bumps older ones up).
fn latest_session_block(
    client: &crate::spacetime_client::SpacetimeClient,
    workflow: &str,
) -> Result<String, String> {
    let folder = format!("Workflows/{}/status/sessions/", workflow);
    let entries = client.list_folder(&folder).map_err(|e| e.to_string())?;

    let latest = entries
        .iter()
        .filter(|e| e.entry_type == "note")
        .filter_map(|n| {
            let date = n.name.get(0..10)?;
            let after = n.name.get(11..)?;
            let (num_str, _) = after.split_once('-')?;
            let num: u32 = num_str.parse().ok()?;
            let id = n.id.as_ref()?;
            Some((date.to_string(), num, id))
        })
        .min_by(|a, b| a.0.cmp(&b.0).reverse().then(a.1.cmp(&b.1)));

    let Some((_, _, id)) = latest else {
        return Ok(format!("No sessions found in {}", folder));
    };
    match client.get_note_by_id(id).map_err(|e| e.to_string())? {
        Some(note) => Ok(format_note(&note)),
        None => Ok("Latest session note vanished between list and fetch".to_string()),
    }
}

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
            name: "search_notes_content".to_string(),
            description: "Search notes and return content excerpts around matches. Use after search_notes to find specific text within notes.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query (case-insensitive, matches content)"
                    },
                    "context_lines": {
                        "type": "integer",
                        "description": "Number of lines to include above and below each match (default: 5)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max number of notes to return (default: 10)"
                    }
                },
                "required": ["query"]
            }),
        },
        Tool {
            name: "list_folder".to_string(),
            description: "List the immediate contents of a folder — both subfolders and notes, each tagged with a 'type' field ('folder' or 'note'). Only direct children, not recursive.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "folder_path": {
                        "type": "string",
                        "description": "Folder path (e.g., 'Development/'). Empty string lists the root."
                    }
                },
                "required": ["folder_path"]
            }),
        },
        Tool {
            name: "get_note".to_string(),
            description: "Get a note's full content by ID or path. Pass raw:true to skip line numbers when you only need to read (not edit).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Note UUID (optional if path provided)"},
                    "path": {"type": "string", "description": "Note path (optional if id provided)"},
                    "raw": {"type": "boolean", "description": "Omit line-number prefixes (default false)"}
                }
            }),
        },
        Tool {
            name: "get_notes".to_string(),
            description: "Get multiple notes' full content by IDs or paths in a single request".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Array of note UUIDs (optional if paths provided)"
                    },
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Array of note paths (optional if ids provided)"
                    }
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
            name: "create_session".to_string(),
            description: "Write a workflow session log. Auto-rotates: new file is `<date>-1-<slug>.md`, existing same-day `<date>-N-*` bump +1 (lower N = newer). Owns the numbering — don't hand-pick. Returns the path.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workflow": {"type": "string", "description": "Bare workflow name, e.g. 'workflow-agent' — the folder is Workflows/<workflow>/status/sessions/"},
                    "slug": {"type": "string", "description": "Kebab-case session topic, e.g. 'session-start-cutover' (no date, no number, no .md)"},
                    "content": {"type": "string", "description": "Full markdown body of the session log"},
                    "date": {"type": "string", "description": "Session date YYYY-MM-DD"}
                },
                "required": ["workflow", "slug", "content", "date"]
            }),
        },
        Tool {
            name: "get_latest_session".to_string(),
            description: "Get the latest workflow session log, full content. Use this to find where you left off.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workflow": {"type": "string", "description": "Bare workflow name, e.g. 'workflow-agent' — the folder is Workflows/<workflow>/status/sessions/"}
                },
                "required": ["workflow"]
            }),
        },
        Tool {
            name: "get_workflow_onload".to_string(),
            description: "Workflow on-load in one call: README + latest session + knowledge index. Call this first when a workflow session starts.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workflow": {"type": "string", "description": "Bare workflow name, e.g. 'workflow-agent'"}
                },
                "required": ["workflow"]
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
            name: "delete_notes".to_string(),
            description: "Delete multiple notes by ID in a single operation".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Array of note UUIDs to delete"
                    }
                },
                "required": ["ids"]
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
                    "old_string": {"type": "string", "description": "Text to find (whitespace differences are handled automatically)"},
                    "new_string": {"type": "string", "description": "Text to replace with (empty to delete)"},
                    "replace_all": {"type": "boolean", "description": "Replace all occurrences (default: false, replaces first only)"}
                },
                "required": ["path", "old_string"]
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
        Tool {
            name: "get_outbound_links".to_string(),
            description: "Get notes this note links to (via `[text](spacenote:UUID)`). Returns id/name/path + `broken` flag per target.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Note UUID (optional if path provided)"},
                    "path": {"type": "string", "description": "Note path (optional if id provided)"}
                }
            }),
        },
        Tool {
            name: "get_backlinks".to_string(),
            description: "Get notes that link TO this note (via `[text](spacenote:UUID)`). Returns id/name/path + `broken` per source. The link graph, backwards.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Note UUID (optional if path provided)"},
                    "path": {"type": "string", "description": "Note path (optional if id provided)"}
                }
            }),
        },
        Tool {
            name: "list_sessions".to_string(),
            description: "List registered SpaceChannel Claude sessions (id, base_name, host, state, last_seen_us). Heartbeats every ~20s → last_seen stale >60s means dead.".to_string(),
            input_schema: json!({"type": "object", "properties": {}}),
        },
        Tool {
            name: "delete_session".to_string(),
            description: "Delete one SpaceChannel session row by id (+cascaded state). For cleaning up stale ghost rows.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": {"type": "string", "description": "Full session id like 'note-assistant@robert'"}
                },
                "required": ["session_id"]
            }),
        },
        Tool {
            name: "clear_all_sessions".to_string(),
            description: "Wipe ALL SpaceChannel session state. Live binaries re-register within seconds, so only ghost rows are destroyed.".to_string(),
            input_schema: json!({"type": "object", "properties": {}}),
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

            let notes = client.search_notes(&query, None).map_err(|e| e.to_string())?;

            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&notes).unwrap_or_else(|_| "[]".to_string())
                }]
            }))
        }
        "search_notes_content" => {
            let query: String = serde_json::from_value(params.arguments["query"].clone())
                .map_err(|e| e.to_string())?;
            let context_lines = params.arguments.get("context_lines")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32)
                .unwrap_or(5);
            let limit = params.arguments.get("limit")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or(10);

            let mut notes = client.search_notes(&query, Some(context_lines)).map_err(|e| e.to_string())?;
            notes.truncate(limit);

            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&notes).unwrap_or_else(|_| "[]".to_string())
                }]
            }))
        }
        "list_folder" => {
            let folder_path: String =
                serde_json::from_value(params.arguments["folder_path"].clone())
                    .map_err(|e| e.to_string())?;

            let entries = client
                .list_folder(&folder_path)
                .map_err(|e| e.to_string())?;

            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_string())
                }]
            }))
        }
        "get_note" => {
            let note = if let Some(id) = params.arguments.get("id").and_then(|v| v.as_str()) {
                client.get_note_by_id(id).map_err(|e| e.to_string())?
            } else if let Some(path) = params.arguments.get("path").and_then(|v| v.as_str()) {
                client.get_note_by_path(path).map_err(|e| e.to_string())?
            } else {
                return Err("Must provide either 'id' or 'path'".to_string());
            };

            let raw = params.arguments.get("raw").and_then(|v| v.as_bool()).unwrap_or(false);
            match note {
                Some(n) => {
                    let body = if raw { n.content.clone() } else { numbered(&n.content) };
                    let text = format!(
                        "id: {}\npath: {}\nname: {}\nfolder_path: {}\nfrontmatter: {}\n\n{}",
                        n.id, n.path, n.name, n.folder_path, n.frontmatter, body
                    );
                    Ok(json!({"content": [{"type": "text", "text": text}]}))
                },
                None => Ok(json!({"content": [{"type": "text", "text": "Note not found"}]})),
            }
        }
        "get_notes" => {
            let notes = if let Some(ids) = params.arguments.get("ids").and_then(|v| v.as_array()) {
                let ids: Vec<String> = ids
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                client.get_notes_by_ids(&ids).map_err(|e| e.to_string())?
            } else if let Some(paths) = params.arguments.get("paths").and_then(|v| v.as_array()) {
                let paths: Vec<String> = paths
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                client.get_notes_by_paths(&paths).map_err(|e| e.to_string())?
            } else {
                return Err("Must provide either 'ids' or 'paths' array".to_string());
            };

            if notes.is_empty() {
                return Ok(json!({"content": [{"type": "text", "text": "No notes found"}]}));
            }

            let mut result = String::new();
            for note in &notes {
                let numbered_content: String = note.content.lines()
                    .enumerate()
                    .map(|(i, line)| format!("{:>4}| {}", i + 1, line))
                    .collect::<Vec<_>>()
                    .join("\n");

                result.push_str(&format!(
                    "---\nid: {}\npath: {}\nname: {}\nfolder_path: {}\nfrontmatter: {}\n\n{}\n\n",
                    note.id, note.path, note.name, note.folder_path, note.frontmatter, numbered_content
                ));
            }

            Ok(json!({"content": [{"type": "text", "text": format!("Found {} notes:\n\n{}", notes.len(), result)}]}))
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
        "create_session" => {
            let workflow: String = serde_json::from_value(params.arguments["workflow"].clone())
                .map_err(|e| e.to_string())?;
            let slug: String = serde_json::from_value(params.arguments["slug"].clone())
                .map_err(|e| e.to_string())?;
            let content: String = serde_json::from_value(params.arguments["content"].clone())
                .map_err(|e| e.to_string())?;
            let date: String = serde_json::from_value(params.arguments["date"].clone())
                .map_err(|e| e.to_string())?;

            let folder = format!("Workflows/{}/status/sessions/", workflow);
            let entries = client
                .list_folder(&folder)
                .map_err(|e| e.to_string())?;

            // Existing files for this day: name is `<date>-<N>-<rest>` (no .md in name).
            // Parse N, sort DESCENDING so we bump the highest first and never clobber.
            let prefix = format!("{}-", date);
            let mut todays: Vec<(u32, String)> = entries
                .iter()
                .filter(|e| e.entry_type == "note")
                .filter_map(|n| {
                    let after = n.name.strip_prefix(&prefix)?;
                    let (num_str, _) = after.split_once('-')?;
                    let num: u32 = num_str.parse().ok()?;
                    Some((num, n.name.clone()))
                })
                .collect();
            todays.sort_by(|a, b| b.0.cmp(&a.0));

            let mut bumped = Vec::new();
            for (num, name) in &todays {
                let rest = name
                    .strip_prefix(&format!("{}-{}-", date, num))
                    .unwrap_or("");
                let old_path = format!("{}{}.md", folder, name);
                let new_path = format!("{}{}-{}-{}.md", folder, date, num + 1, rest);
                client
                    .move_note(old_path.clone(), new_path.clone())
                    .map_err(|e| e.to_string())?;
                bumped.push(format!("{} -> {}-{}-{}", name, date, num + 1, rest));
            }

            let new_name = format!("{}-1-{}", date, slug);
            let new_path = format!("{}{}.md", folder, new_name);
            let new_id = uuid::Uuid::new_v4().to_string();
            client
                .create_note(new_id.clone(), new_path.clone(), new_name, content, folder)
                .map_err(|e| e.to_string())?;

            let mut text = format!("Created session: {} (id: {})", new_path, new_id);
            if !bumped.is_empty() {
                text.push_str(&format!("\nRotated {} existing: {:?}", bumped.len(), bumped));
            }
            Ok(json!({"content": [{"type": "text", "text": text}]}))
        }
        "get_latest_session" => {
            let workflow: String = serde_json::from_value(params.arguments["workflow"].clone())
                .map_err(|e| e.to_string())?;
            let text = latest_session_block(client, &workflow)?;
            Ok(json!({"content": [{"type": "text", "text": text}]}))
        }
        "get_workflow_onload" => {
            let workflow: String = serde_json::from_value(params.arguments["workflow"].clone())
                .map_err(|e| e.to_string())?;

            let section = |title: &str, path: &str| -> String {
                match client.get_note_by_path(path) {
                    Ok(Some(n)) => format!("## {}\n\n{}", title, format_note(&n)),
                    Ok(None) => format!("## {}\n\n(not found: {})", title, path),
                    Err(e) => format!("## {}\n\n(error: {})", title, e),
                }
            };

            let readme = section("README", &format!("Workflows/{}/README.md", workflow));
            let latest = format!("## Latest session\n\n{}", latest_session_block(client, &workflow)?);
            let knowledge = section(
                "Knowledge index",
                &format!("Workflows/{}/knowledge/README.md", workflow),
            );
            let text = format!("{}\n\n{}\n\n{}", readme, latest, knowledge);
            Ok(json!({"content": [{"type": "text", "text": text}]}))
        }
        "delete_note" => {
            let id: String = serde_json::from_value(params.arguments["id"].clone())
                .map_err(|e| e.to_string())?;

            client.delete_note(id.clone()).map_err(|e| e.to_string())?;

            Ok(json!({"content": [{"type": "text", "text": format!("Deleted note: {}", id)}]}))
        }
        "delete_notes" => {
            let ids: Vec<String> = serde_json::from_value(params.arguments["ids"].clone())
                .map_err(|e| e.to_string())?;

            let mut deleted = Vec::new();
            let mut errors = Vec::new();

            for id in ids {
                match client.delete_note(id.clone()) {
                    Ok(_) => deleted.push(id),
                    Err(e) => errors.push(format!("{}: {}", id, e)),
                }
            }

            let mut result = format!("Deleted {} notes", deleted.len());
            if !errors.is_empty() {
                result.push_str(&format!("\nErrors: {:?}", errors));
            }

            Ok(json!({"content": [{"type": "text", "text": result}]}))
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
            let old_string: String = params.arguments.get("old_string")
                .or_else(|| params.arguments.get("oldString"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| "old_string is required".to_string())?;
            let new_string: String = params.arguments.get("new_string")
                .or_else(|| params.arguments.get("newString"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let replace_all: bool = params.arguments.get("replace_all")
                .or_else(|| params.arguments.get("replaceAll"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let note = client.get_note_by_path(&path)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("Note not found: {}", path))?;

            if note.content.contains(&old_string) {
                client.find_replace_in_note(path.clone(), old_string, new_string, replace_all)
                    .map_err(|e| e.to_string())?;
                Ok(json!({"content": [{"type": "text", "text": format!("Edited note: {}", path)}]}))
            } else {
                let norm_content = normalize_for_matching(&note.content);
                let norm_old = normalize_for_matching(&old_string);

                if norm_content.contains(&norm_old) {
                    let new_content = if replace_all {
                        norm_content.replace(&norm_old, &new_string)
                    } else {
                        norm_content.replacen(&norm_old, &new_string, 1)
                    };

                    client
                        .update_note_content(note.id, new_content)
                        .map_err(|e| e.to_string())?;

                    tracing::info!("edit_note used normalized fallback for path={}", path);

                    Ok(json!({"content": [{"type": "text", "text": format!("Edited note: {}", path)}]}))
                } else {
                    let old_preview: String = old_string.chars().take(80).collect();
                    let content_preview: String = note.content.chars().take(200).collect();
                    tracing::warn!(
                        "EDIT_NOTE FAILED: old_string not found even after normalization. old_preview={:?}, content_preview={:?}",
                        old_preview, content_preview
                    );
                    Err(format!(
                        "Edit failed: The text to replace was not found in '{}'. The old_string does not match any content even after whitespace normalization. Try reading the note again and using the exact content.",
                        path
                    ))
                }
            }
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
        "get_outbound_links" => {
            let id = resolve_note_id(client, &params.arguments)?;
            let links = client.get_outbound_links(&id).map_err(|e| e.to_string())?;
            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&json!({"links": links}))
                        .unwrap_or_else(|_| "{\"links\":[]}".to_string())
                }]
            }))
        }
        "get_backlinks" => {
            let id = resolve_note_id(client, &params.arguments)?;
            let links = client.get_backlinks(&id).map_err(|e| e.to_string())?;
            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&json!({"links": links}))
                        .unwrap_or_else(|_| "{\"links\":[]}".to_string())
                }]
            }))
        }
        "list_sessions" => {
            let sessions = client.list_sessions().map_err(|e| e.to_string())?;
            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&json!({"sessions": sessions}))
                        .unwrap_or_else(|_| "{\"sessions\":[]}".to_string())
                }]
            }))
        }
        "delete_session" => {
            let session_id: String = serde_json::from_value(params.arguments["session_id"].clone())
                .map_err(|e| e.to_string())?;
            client.delete_session(session_id.clone()).map_err(|e| e.to_string())?;
            Ok(json!({"content": [{"type": "text", "text": format!("Deleted session: {}", session_id)}]}))
        }
        "clear_all_sessions" => {
            client.clear_all_sessions().map_err(|e| e.to_string())?;
            Ok(json!({"content": [{"type": "text", "text": "Cleared all sessions. Live binaries will re-register within seconds."}]}))
        }
        _ => Err(format!("Unknown tool: {}", params.name)),
    }
}

fn resolve_note_id(
    client: &crate::spacetime_client::SpacetimeClient,
    args: &Value,
) -> Result<String, String> {
    if let Some(id) = args.get("id").and_then(|v| v.as_str()) {
        return Ok(id.to_string());
    }
    if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
        let note = client
            .get_note_by_path(path)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Note not found: {}", path))?;
        return Ok(note.id);
    }
    Err("Must provide either 'id' or 'path'".to_string())
}
