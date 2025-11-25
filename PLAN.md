# Obsidian SpacetimeDB Sync - Master Plan

## Overview

A Rust file watcher application that monitors an Obsidian vault on the NAS and syncs all changes to SpacetimeDB in real-time. This enables multi-client sync through the SpacetimeDB Dart SDK.

## Architecture

```
NAS Obsidian Vault → Rust Watcher → SpacetimeDB (local on NAS)
                                         ↓
                    Flutter/Dart apps ← Dart SDK
```

### Sync Flow
- **Remotely Save plugin** syncs Obsidian (Desktop + iOS) to NAS via WebDAV
- **This watcher** monitors the NAS vault folder and pushes changes to SpacetimeDB
- **Dart SDK** allows Flutter apps to subscribe to real-time note updates

## Configuration

### Vault Location
- **Path:** `/volume1/homes/mikael/ObsidianNAS/`
- **Access:** WebDAV for Obsidian clients, direct filesystem for watcher

### SpacetimeDB
- **Host:** `http://192.168.1.161:3003` (running on NAS, NOT local laptop)
- **Database:** `obsidian_sync`
- **Note:** Must publish module to NAS SpacetimeDB instance, not local laptop instance

## Data Model

### Note Table

Based on the Obsidian-aware Note class from `opencode_flutter_client/lib/models/note.dart`:

```rust
#[spacetimedb(table)]
pub struct Note {
    #[primarykey]
    pub path: String,        // "Projects/my-note.md"
    pub name: String,        // "my-note"
    pub content: String,
    pub folder_path: String, // "Projects/"
    pub depth: u32,
    pub frontmatter: String, // JSON-serialized Map
    pub size: u64,
    pub created_time: u64,   // ms since epoch
    pub modified_time: u64,
}
```

### Folder Table

```rust
#[spacetimedb(table)]
pub struct Folder {
    #[primarykey]
    pub path: String,
    pub name: String,
    pub depth: u32,
}
```

## Reducers

### Note Operations

```rust
#[spacetimedb(reducer)]
pub fn create_note(ctx: &ReducerContext, note: Note) -> Result<(), String>

#[spacetimedb(reducer)]
pub fn update_note(ctx: &ReducerContext, path: String, content: String,
                   frontmatter: String, size: u64, modified_time: u64) -> Result<(), String>

#[spacetimedb(reducer)]
pub fn delete_note(ctx: &ReducerContext, path: String) -> Result<(), String>

#[spacetimedb(reducer)]
pub fn move_note(ctx: &ReducerContext, old_path: String, new_path: String) -> Result<(), String>
```

### Folder Operations

```rust
#[spacetimedb(reducer)]
pub fn create_folder(ctx: &ReducerContext, folder: Folder) -> Result<(), String>

#[spacetimedb(reducer)]
pub fn delete_folder(ctx: &ReducerContext, path: String) -> Result<(), String>

#[spacetimedb(reducer)]
pub fn move_folder(ctx: &ReducerContext, old_path: String, new_path: String) -> Result<(), String>
```

### Bulk Operations

```rust
#[spacetimedb(reducer)]
pub fn bulk_sync(ctx: &ReducerContext, notes: Vec<Note>, folders: Vec<Folder>) -> Result<(), String>
```

## Application Structure

### Main Components

1. **Vault Scanner** - Initial sync, scan all `.md` files
2. **File Watcher** - Monitor for create/modify/delete/rename events
3. **SpacetimeDB Client** - Connect and call reducers
4. **Debouncer** - Batch rapid changes (editor autosaves)

### File Event Mapping

| File Event | Reducer Call |
|------------|--------------|
| Create (file) | `create_note` |
| Create (dir) | `create_folder` |
| Modify | `update_note` |
| Remove (file) | `delete_note` |
| Remove (dir) | `delete_folder` |
| Rename (file) | `move_note` |
| Rename (dir) | `move_folder` |

### Dependencies

```toml
[dependencies]
spacetimedb-sdk = "1.0"
notify = "6.0"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
walkdir = "2.5"
anyhow = "1.0"
clap = { version = "4", features = ["derive"] }
```

## Reference Implementation

The `obsidian_mcp_server` project at `~/Productivity/Development/Rust/obsidian_mcp_server/` provides useful patterns:

- `Vault` struct with path handling
- `list_notes()` using `walkdir`
- `read_note()` / `write_note()` async operations
- Relative path handling with `strip_prefix`
- Docker deployment setup

## Deployment

### Docker Compose

Similar to obsidian-mcp-server setup:

```yaml
version: '3.8'

services:
  obsidian-spacetime-sync:
    build: .
    container_name: obsidian-spacetime-sync
    volumes:
      - /volume1/homes/mikael/ObsidianNAS:/vault:ro
    environment:
      - VAULT_PATH=/vault
      - SPACETIME_HOST=http://localhost:3000
      - SPACETIME_DB=obsidian_sync
    restart: unless-stopped
    network_mode: host  # To access local SpacetimeDB
```

### Alternative: Systemd Service

Run as a background service on the NAS without Docker.

## Deployment Workflow

1. Copy project folder to NAS: `scp -r . mikael@192.168.1.161:/volume1/docker/obsidian-spacetime-sync/`
2. Start SpacetimeDB: `docker-compose up -d spacetimedb`
3. Publish module (from laptop): `spacetime publish -s http://192.168.1.161:3003 obsidian_sync`
4. Start watcher: `docker-compose up -d obsidian-spacetime-sync`

**Note:** docker-compose does NOT auto-publish the module. Must manually publish after SpacetimeDB starts.

## Implementation Steps

1. **SpacetimeDB Module** ✅
   - Define Note and Folder tables
   - Implement all reducers
   - Build module locally: `cd spacetime-module && spacetime build`
   - Deploy to NAS SpacetimeDB instance: `spacetime publish -s http://192.168.1.161:3003 obsidian_sync`

2. **Dockerfile**
   - Create multi-stage build for Rust watcher app
   - Use musl for static binary

3. **Rust Watcher App**
   - Setup project with dependencies
   - Implement Vault scanner (reuse from obsidian-mcp)
   - Add notify file watcher
   - Connect to SpacetimeDB
   - Map file events to reducer calls
   - Add debouncing for rapid changes

3. **Initial Sync**
   - Scan vault on startup
   - Use `bulk_sync` reducer to populate DB
   - Handle incremental updates after initial sync

4. **Metadata Extraction**
   - Parse YAML frontmatter from notes
   - Extract file size and timestamps
   - Calculate folder depth from path

5. **Docker Deployment**
   - Create Dockerfile (musl static binary)
   - Create docker-compose.yml
   - Deploy to NAS

6. **Testing**
   - Test with Dart SDK from opencode_flutter_client
   - Verify real-time updates

## Future Enhancements

- **Folder deletion handling** - Currently deleting a folder doesn't remove its notes from SpacetimeDB (orphan records). Need to implement `delete_folder_recursive` that queries DB for notes with matching `folder_path` prefix and deletes them.
- **Rename/move detection** - Detect file renames vs delete+create to preserve note history
- **Content hashing** - Only sync notes when content actually changes (compare hashes)
- **Error callbacks** - Register SpacetimeDB reducer error callbacks for better error handling
- **Graceful shutdown** - Handle SIGTERM/SIGINT for clean watcher shutdown

## Related Projects

- **SpacetimeDB Dart SDK:** `~/Productivity/Development/Dart/spacetimedb_dart_sdk/`
- **Obsidian MCP Server:** `~/Productivity/Development/Rust/obsidian_mcp_server/`
- **OpenCode Flutter Client:** `~/Productivity/Development/Flutter/opencode_flutter_client/`
  - Note model: `lib/models/note.dart`
