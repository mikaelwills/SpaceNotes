use crate::matcher;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

fn relaxed(s: &str) -> String {
    s.replace("\r\n", "\n")
        .lines()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join("\n")
}

fn escape_invisibles(s: &str) -> String {
    s.replace('\t', "→")
        .replace('\r', "\\r")
        .replace('\u{00A0}', "<NBSP>")
        .replace('\u{202F}', "<NNBSP>")
        .lines()
        .map(|line| {
            let trimmed = line.trim_end();
            if trimmed.len() == line.len() {
                line.to_string()
            } else {
                let dots = line[trimmed.len()..].chars().count();
                format!("{}{}", trimmed, "·".repeat(dots))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn edit_no_match_error(path: &str, old_string: &str, content: &str) -> String {
    let mut msg = format!("old_string not found in '{}'.\n", path);

    let needle_first = old_string.lines().next().unwrap_or("").trim();
    let candidate = if needle_first.is_empty() {
        None
    } else {
        content.lines().enumerate().find(|(_, l)| l.trim() == needle_first).or_else(|| {
            content
                .lines()
                .enumerate()
                .find(|(_, l)| l.trim().eq_ignore_ascii_case(needle_first))
        })
    };

    match candidate {
        Some((idx, _)) => {
            let needle_lines = old_string.lines().count();
            let actual: Vec<&str> = content.lines().skip(idx).take(needle_lines).collect();
            let actual_block = actual.join("\n");
            msg.push_str(&format!(
                "\nClosest match at lines {}-{}:\n",
                idx + 1,
                idx + needle_lines
            ));
            msg.push_str(&format!("  actual:    {}\n", escape_invisibles(&actual_block)));
            msg.push_str(&format!("  you sent:  {}\n", escape_invisibles(old_string)));

            // lines() strips \r, so a CRLF note renders `actual` identical to what the caller
            // sent — telling them to copy it would loop forever.
            if content.contains("\r\n") && !old_string.contains('\r') {
                msg.push_str(
                    "\nThis note uses CRLF line endings. old_string must use \\r\\n between lines.\n",
                );
            } else if relaxed(&actual_block) == relaxed(old_string) {
                msg.push_str("\nWhitespace differs (· space, → tab). Copy `actual`.\n");
            } else if actual_block.to_lowercase() == old_string.to_lowercase() {
                msg.push_str("\nCase differs.\n");
            } else {
                msg.push_str("\nCopy `actual`, or re-read with get_note raw:true.\n");
            }
        }
        None => {
            msg.push_str("\nNo similar line found. Re-read with get_note raw:true.\n");
        }
    }

    truncate_on_char_boundary(&mut msg, 2048);
    msg
}

// String::truncate panics unless the index is a char boundary.
fn truncate_on_char_boundary(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

// Must stay identical to the module's name_from_path (file_reducers.rs), or a note gets one
// name when created here and a different one after any move.
fn name_from_path(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    match base.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => base.to_string(),
    }
}

fn folder_path_of(path: &str) -> String {
    match path.rfind('/') {
        Some(idx) => format!("{}/", &path[..idx]),
        None => String::new(),
    }
}

fn validate_note_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("path must not be empty".to_string());
    }
    if path.ends_with('/') {
        return Err(format!("path must be a file, not a folder: '{}'", path));
    }
    if name_from_path(path).is_empty() {
        return Err(format!("path has no filename: '{}'", path));
    }
    Ok(())
}

fn numbered(content: &str) -> String {
    content
        .lines()
        .enumerate()
        .map(|(i, line)| format!("{:>4}| {}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_file(n: &crate::spacetime_client::FullSpaceFile) -> String {
    format!(
        "id: {}\npath: {}\nname: {}\nfolder_path: {}\n\n{}",
        n.id, n.path, n.name, n.folder_path, numbered(&n.content)
    )
}

// The latest session file for a workflow, formatted, or a "no sessions" line.
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
    match client.get_file_by_id(id).map_err(|e| e.to_string())? {
        Some(file) => Ok(format_file(&file)),
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
    #[serde(default)]
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
            description: "Delete a folder and everything inside it, recursively.".to_string(),
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
            description: "Edit a note by finding and replacing text.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Note path (e.g., 'Development/My Note.md')"},
                    "old_string": {"type": "string", "description": "Text to find. Whitespace and indentation differences are tolerated; case is not. Must be unique unless replace_all or occurrence is set."},
                    "new_string": {"type": "string", "description": "Replacement text. Pass \"\" to delete."},
                    "replace_all": {"type": "boolean", "description": "Replace every occurrence (default false)"},
                    "occurrence": {"type": "integer", "description": "Replace only the Nth match (1-based)"},
                    "dry_run": {"type": "boolean", "description": "Report what would change without writing"},
                    "allow_fuzzy": {"type": "boolean", "description": "Permit an ambiguous whitespace-folded match"}
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
    client.await_ready().await.map_err(|e| e.to_string())?;

    match params.name.as_str() {
        "search_notes" => {
            let query: String = serde_json::from_value(params.arguments["query"].clone())
                .map_err(|e| e.to_string())?;

            let files = client.search_files(&query, None).map_err(|e| e.to_string())?;

            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&files).unwrap_or_else(|_| "[]".to_string())
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

            let mut files = client.search_files(&query, Some(context_lines)).map_err(|e| e.to_string())?;
            files.truncate(limit);

            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&files).unwrap_or_else(|_| "[]".to_string())
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
            let file = if let Some(id) = params.arguments.get("id").and_then(|v| v.as_str()) {
                client.get_file_by_id(id).map_err(|e| e.to_string())?
            } else if let Some(path) = params.arguments.get("path").and_then(|v| v.as_str()) {
                client.get_file_by_path(path).map_err(|e| e.to_string())?
            } else {
                return Err("Must provide either 'id' or 'path'".to_string());
            };

            let raw = params.arguments.get("raw").and_then(|v| v.as_bool()).unwrap_or(false);
            match file {
                Some(n) => {
                    let body = if raw { n.content.clone() } else { numbered(&n.content) };
                    let text = format!(
                        "id: {}\npath: {}\nname: {}\nfolder_path: {}\n\n{}",
                        n.id, n.path, n.name, n.folder_path, body
                    );
                    Ok(json!({"content": [{"type": "text", "text": text}]}))
                },
                None => Ok(json!({"content": [{"type": "text", "text": "Note not found"}]})),
            }
        }
        "get_notes" => {
            let files = if let Some(ids) = params.arguments.get("ids").and_then(|v| v.as_array()) {
                let ids: Vec<String> = ids
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                client.get_files_by_ids(&ids).map_err(|e| e.to_string())?
            } else if let Some(paths) = params.arguments.get("paths").and_then(|v| v.as_array()) {
                let paths: Vec<String> = paths
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                client.get_files_by_paths(&paths).map_err(|e| e.to_string())?
            } else {
                return Err("Must provide either 'ids' or 'paths' array".to_string());
            };

            if files.is_empty() {
                return Ok(json!({"content": [{"type": "text", "text": "No notes found"}]}));
            }

            let mut result = String::new();
            for file in &files {
                let numbered_content: String = file.content.lines()
                    .enumerate()
                    .map(|(i, line)| format!("{:>4}| {}", i + 1, line))
                    .collect::<Vec<_>>()
                    .join("\n");

                result.push_str(&format!(
                    "---\nid: {}\npath: {}\nname: {}\nfolder_path: {}\n\n{}\n\n",
                    file.id, file.path, file.name, file.folder_path, numbered_content
                ));
            }

            Ok(json!({"content": [{"type": "text", "text": format!("Found {} notes:\n\n{}", files.len(), result)}]}))
        }
        "create_note" => {
            let path: String = serde_json::from_value(params.arguments["path"].clone())
                .map_err(|e| e.to_string())?;
            let content: String = serde_json::from_value(params.arguments["content"].clone())
                .map_err(|e| e.to_string())?;

            validate_note_path(&path)?;

            if let Some(existing) = client.get_file_by_path(&path).map_err(|e| e.to_string())? {
                return Err(format!(
                    "Note already exists at '{}' (id: {}). Use edit_note or append_to_note.",
                    path, existing.id
                ));
            }

            let name = name_from_path(&path);
            let folder_path = folder_path_of(&path);
            let id = uuid::Uuid::new_v4().to_string();

            client
                .create_file(id.clone(), path.clone(), name, content, folder_path)
                .await
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
                    .move_file(old_path.clone(), new_path.clone())
                    .await
                    .map_err(|e| e.to_string())?;
                bumped.push(format!("{} -> {}-{}-{}", name, date, num + 1, rest));
            }

            let new_name = format!("{}-1-{}", date, slug);
            let new_path = format!("{}{}.md", folder, new_name);
            let new_id = uuid::Uuid::new_v4().to_string();
            client
                .create_file(new_id.clone(), new_path.clone(), new_name, content, folder)
                .await
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
                match client.get_file_by_path(path) {
                    Ok(Some(n)) => format!("## {}\n\n{}", title, format_file(&n)),
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

            client.delete_file(id.clone()).await.map_err(|e| e.to_string())?;

            Ok(json!({"content": [{"type": "text", "text": format!("Deleted note: {}", id)}]}))
        }
        "delete_notes" => {
            let ids: Vec<String> = serde_json::from_value(params.arguments["ids"].clone())
                .map_err(|e| e.to_string())?;

            let mut deleted = Vec::new();
            let mut errors = Vec::new();

            for id in ids {
                match client.delete_file(id.clone()).await {
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
                .move_file(old_path.clone(), new_path.clone())
                .await
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
                .await
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
                .await
                .map_err(|e| e.to_string())?;

            Ok(json!({"content": [{"type": "text", "text": format!("Created folder: {}", path)}]}))
        }
        "delete_folder" => {
            let path: String = serde_json::from_value(params.arguments["path"].clone())
                .map_err(|e| e.to_string())?;

            client
                .delete_folder(path.clone())
                .await
                .map_err(|e| e.to_string())?;

            Ok(json!({"content": [{"type": "text", "text": format!("Deleted folder: {}", path)}]}))
        }
        "append_to_note" => {
            let path: String = serde_json::from_value(params.arguments["path"].clone())
                .map_err(|e| e.to_string())?;
            let content: String = serde_json::from_value(params.arguments["content"].clone())
                .map_err(|e| e.to_string())?;

            client
                .append_to_file(path.clone(), content)
                .await
                .map_err(|e| e.to_string())?;

            Ok(json!({"content": [{"type": "text", "text": format!("Appended to note: {}", path)}]}))
        }
        "prepend_to_note" => {
            let path: String = serde_json::from_value(params.arguments["path"].clone())
                .map_err(|e| e.to_string())?;
            let content: String = serde_json::from_value(params.arguments["content"].clone())
                .map_err(|e| e.to_string())?;

            client
                .prepend_to_file(path.clone(), content)
                .await
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
                .ok_or_else(|| {
                    "new_string is required (pass \"\" to delete the matched text)".to_string()
                })?;
            let replace_all: bool = params.arguments.get("replace_all")
                .or_else(|| params.arguments.get("replaceAll"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let occurrence: Option<usize> = params
                .arguments
                .get("occurrence")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let dry_run: bool = params
                .arguments
                .get("dry_run")
                .or_else(|| params.arguments.get("dryRun"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let allow_fuzzy: bool = params
                .arguments
                .get("allow_fuzzy")
                .or_else(|| params.arguments.get("allowFuzzy"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if old_string.is_empty() {
                return Err("old_string must not be empty".to_string());
            }

            let file = client
                .get_file_by_path(&path)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("Note not found: {}", path))?;

            let found = matcher::find(&file.content, &old_string)
                .ok_or_else(|| edit_no_match_error(&path, &old_string, &file.content))?;
            let count = found.matches.len();

            // A fuzzy tier can match text the caller did not mean, so it needs either an
            // unambiguous match or an explicit opt-in.
            if found.tier.needs_opt_in() && count > 1 && !allow_fuzzy {
                return Err(format!(
                    "old_string only matched '{}' at the {} tier, in {} places. \
                     Add exact context, or pass allow_fuzzy: true.",
                    path,
                    found.tier.label(),
                    count
                ));
            }

            let targets: Vec<usize> = match (occurrence, replace_all) {
                (Some(n), _) => {
                    if n == 0 || n > count {
                        return Err(format!(
                            "occurrence {} is out of range: {} match(es) in '{}'.",
                            n, count, path
                        ));
                    }
                    vec![n - 1]
                }
                (None, true) => (0..count).collect(),
                (None, false) => {
                    if count > 1 {
                        let places: Vec<String> = found
                            .matches
                            .iter()
                            .map(|m| format!("lines {}-{}", m.first_line, m.last_line))
                            .collect();
                        return Err(format!(
                            "old_string appears {} times in '{}' ({}). Add context to make it \
                             unique, pass replace_all: true, or target one with occurrence: N.",
                            count,
                            path,
                            places.join("; ")
                        ));
                    }
                    vec![0]
                }
            };

            let updated = matcher::apply(&file.content, &old_string, &new_string, &found, &targets);

            if dry_run {
                let places: Vec<String> = targets
                    .iter()
                    .map(|&i| {
                        let m = &found.matches[i];
                        format!("lines {}-{}", m.first_line, m.last_line)
                    })
                    .collect();
                return Ok(json!({"content": [{"type": "text", "text": format!(
                    "dry run: would edit {} at {} ({} tier, {} of {} match(es)). No changes written.",
                    path, places.join("; "), found.tier.label(), targets.len(), count
                )}]}));
            }

            client
                .update_file_content(file.id, updated)
                .await
                .map_err(|e| e.to_string())?;

            let summary = format!(
                "Edited note: {} ({} of {} match(es), {} tier)",
                path,
                targets.len(),
                count,
                found.tier.label()
            );
            Ok(json!({"content": [{"type": "text", "text": summary}]}))
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

                match client.move_file(old_path.clone(), new_path.clone()).await {
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

            let current_file = client.get_file_by_path(&path)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("Note not found: {}", path))?;

            let re = RegexBuilder::new(&pattern)
                .case_insensitive(case_insensitive)
                .multi_line(multiline)
                .build()
                .map_err(|e| format!("Invalid regex pattern: {}", e))?;

            let new_content = re.replace_all(&current_file.content, replacement.as_str()).to_string();

            if new_content == current_file.content {
                return Ok(json!({"content": [{"type": "text", "text": "No matches found - note unchanged"}]}));
            }

            let match_count = re.find_iter(&current_file.content).count();

            client
                .update_file_content(current_file.id, new_content.clone())
                .await
                .map_err(|e| e.to_string())?;

            Ok(json!({"content": [{"type": "text", "text": format!("Replaced {} matches in {}\n\n---\n\n{}", match_count, path, new_content)}]}))
        }
        "get_outbound_links" => {
            let id = resolve_file_id(client, &params.arguments)?;
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
            let id = resolve_file_id(client, &params.arguments)?;
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

fn resolve_file_id(
    client: &crate::spacetime_client::SpacetimeClient,
    args: &Value,
) -> Result<String, String> {
    if let Some(id) = args.get("id").and_then(|v| v.as_str()) {
        return Ok(id.to_string());
    }
    if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
        let file = client
            .get_file_by_path(path)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Note not found: {}", path))?;
        return Ok(file.id);
    }
    Err("Must provide either 'id' or 'path'".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_strips_exactly_one_extension() {
        assert_eq!(name_from_path("A/notes.md"), "notes");
        assert_eq!(name_from_path("notes.md"), "notes");
        assert_eq!(name_from_path("A/config.yaml"), "config");
    }

    #[test]
    fn name_strips_one_extension_not_repeats() {
        assert_eq!(name_from_path("A/notes.md.md"), "notes.md");
    }

    #[test]
    fn name_keeps_dotless_and_leading_dot_names() {
        assert_eq!(name_from_path("A/README"), "README");
        assert_eq!(name_from_path("A/.gitignore"), ".gitignore");
    }

    #[test]
    fn folder_path_keeps_trailing_slash_and_is_empty_at_root() {
        assert_eq!(folder_path_of("A/B/note.md"), "A/B/");
        assert_eq!(folder_path_of("note.md"), "");
    }

    #[test]
    fn validate_rejects_empty_folder_and_nameless_paths() {
        assert!(validate_note_path("A/note.md").is_ok());
        assert!(validate_note_path("").is_err());
        assert!(validate_note_path("   ").is_err());
        assert!(validate_note_path("A/B/").is_err());
    }

    #[test]
    fn relaxed_ignores_indentation_and_line_endings() {
        assert_eq!(relaxed("  a\r\n\tb  "), relaxed("a\nb"));
    }

    #[test]
    fn escape_invisibles_marks_trailing_space_tab_and_nbsp() {
        assert_eq!(escape_invisibles("a  "), "a··");
        assert_eq!(escape_invisibles("\ta"), "→a");
        assert_eq!(escape_invisibles("a\u{00A0}b"), "a<NBSP>b");
    }

    #[test]
    fn escape_invisibles_leaves_clean_text_alone() {
        assert_eq!(escape_invisibles("plain text"), "plain text");
    }

    #[test]
    fn no_match_error_flags_whitespace_only_difference() {
        let err = edit_no_match_error("N.md", "hello", "  hello  \nworld\n");
        assert!(err.contains("Whitespace differs"), "got: {}", err);
        assert!(err.contains("Closest match at lines 1-1"), "got: {}", err);
    }

    #[test]
    fn no_match_error_names_crlf_instead_of_showing_identical_blocks() {
        // lines() strips \r, so `actual` and `you sent` render identically here. Advising
        // "copy actual" would loop the caller forever.
        let err = edit_no_match_error("N.md", "line one\nline two", "line one\r\nline two\r\n");
        assert!(err.contains("CRLF"), "got: {}", err);
        assert!(!err.contains("Copy `actual`"), "sent caller on a loop: {}", err);
    }

    #[test]
    fn escape_invisibles_counts_multibyte_trailing_space_once() {
        assert_eq!(escape_invisibles("abc\u{2003}"), "abc·");
    }

    #[test]
    fn no_match_error_flags_case_only_difference() {
        let err = edit_no_match_error("N.md", "Hello World", "hello world\n");
        assert!(err.contains("Case differs"), "got: {}", err);
    }

    #[test]
    fn no_match_error_reports_absence_when_nothing_is_similar() {
        let err = edit_no_match_error("N.md", "zebra", "alpha\nbeta\n");
        assert!(err.contains("No similar line found"), "got: {}", err);
    }

    #[test]
    fn no_match_error_is_capped_when_it_quotes_a_long_candidate() {
        // Must take the candidate-found branch, or truncation is never exercised.
        let long = "x".repeat(50_000);
        let content = format!("  {}  \n", long);
        let err = edit_no_match_error("N.md", &long, &content);
        assert!(err.contains("Closest match"), "wrong branch: {}", &err[..80.min(err.len())]);
        assert!(err.len() <= 2048);
    }

    #[test]
    fn empty_old_string_is_not_treated_as_a_match() {
        // "abc".matches("") counts 4, and replace_all would splice between every char.
        assert_eq!("abc".matches("").count(), 4);
    }

    #[test]
    fn no_match_error_does_not_split_multibyte_chars() {
        // A naive String::truncate panics when the cap lands inside an em dash.
        let needle = "—".repeat(4_000);
        let content = format!("{}\nother\n", needle);
        let err = edit_no_match_error("N.md", &needle, &content);
        assert!(err.len() <= 2048);
        assert!(err.is_char_boundary(err.len()));
    }

    #[test]
    fn truncate_on_char_boundary_never_splits_a_char() {
        for cap in 0..12 {
            let mut s = "a—b—c".to_string();
            truncate_on_char_boundary(&mut s, cap);
            assert!(s.len() <= cap);
        }
    }

    #[test]
    fn truncate_on_char_boundary_leaves_short_strings_alone() {
        let mut s = "short".to_string();
        truncate_on_char_boundary(&mut s, 2048);
        assert_eq!(s, "short");
    }

    #[test]
    fn tool_call_params_tolerate_missing_arguments() {
        let params: ToolCallParams = serde_json::from_value(json!({"name": "get_note"})).unwrap();
        assert_eq!(params.name, "get_note");
        assert!(params.arguments.is_null());
    }

    #[test]
    fn tool_call_params_reject_missing_name() {
        assert!(serde_json::from_value::<ToolCallParams>(json!({"arguments": {}})).is_err());
    }

    #[test]
    fn edit_note_schema_requires_new_string() {
        let edit = get_tools().into_iter().find(|t| t.name == "edit_note").unwrap();
        let required = edit.input_schema["required"].as_array().unwrap().clone();
        assert!(required.contains(&json!("new_string")));
    }

    #[test]
    fn every_tool_name_is_still_the_external_contract() {
        // Tool names are bound by string in skills, hooks, allowlists and python callers.
        // Renaming one silently breaks callers, so pin the *_note surface.
        let names: Vec<String> = get_tools().into_iter().map(|t| t.name).collect();
        for expected in [
            "get_note", "get_notes", "create_note", "edit_note", "delete_note", "delete_notes",
            "move_note", "move_notes_to_folder", "append_to_note", "prepend_to_note",
            "search_notes", "search_notes_content", "list_folder",
        ] {
            assert!(names.contains(&expected.to_string()), "missing tool: {}", expected);
        }
    }
}
