# Obsidian SpacetimeDB Sync

A Rust file watcher that monitors an Obsidian vault and syncs all changes to SpacetimeDB in real-time. This enables multi-client sync through the SpacetimeDB Dart SDK.

## Architecture

```
NAS Obsidian Vault → Rust Watcher → SpacetimeDB (NAS)
                                         ↓
                    Flutter/Dart apps ← Dart SDK
```

**Current implementation:** One-way sync (Vault → SpacetimeDB)

## Features

- **Initial scan** - Syncs all notes and folders on startup
- **Real-time watching** - Monitors vault for file changes
- **Debouncing** - 2-second debounce to handle editor save spam
- **Frontmatter parsing** - Extracts YAML frontmatter as JSON
- **Hidden file filtering** - Skips `.obsidian`, `.git`, etc.
- **Path canonicalization** - Handles relative/absolute path mismatches

## Data Model

### Note
- `path` - Relative path (e.g., "Projects/my-note.md")
- `name` - Note name without extension
- `content` - Markdown body (without frontmatter)
- `folder_path` - Parent folder path
- `depth` - Nesting level
- `frontmatter` - JSON-serialized YAML frontmatter
- `size` - File size in bytes
- `created_time` - Unix timestamp (ms)
- `modified_time` - Unix timestamp (ms)

### Folder
- `path` - Relative path
- `name` - Folder name
- `depth` - Nesting level

## Project Structure

```
obsidian-spacetime-sync/
├── src/
│   ├── main.rs              # CLI args, startup, orchestration
│   ├── client.rs            # SpacetimeDB client wrapper
│   ├── scanner.rs           # Vault scanning (notes & folders)
│   ├── watcher.rs           # File system watcher
│   ├── note.rs              # Note struct
│   ├── folder.rs            # Folder struct
│   ├── frontmatter.rs       # YAML frontmatter parser
│   ├── tracker.rs           # Content hash tracker (for two-way sync)
│   ├── writer.rs            # Atomic file writer (for two-way sync)
│   └── spacetime_bindings/  # Auto-generated SpacetimeDB client code
├── spacetime-module/
│   └── src/lib.rs           # SpacetimeDB tables & reducers
├── Cargo.toml
├── Dockerfile
├── docker-compose.yml
├── PLAN.md                  # Original implementation plan
├── TWO_WAY_PLAN.md          # Future two-way sync plan
└── CLAUDE.md                # Teaching progress tracker
```

## SpacetimeDB Reducers

- `upsert_note` - Create or update a note
- `upsert_folder` - Create or update a folder
- `delete_note` - Delete a note by path
- `delete_folder` - Delete a folder by path
- `move_note` - Rename/move a note
- `move_folder` - Rename/move a folder
- `clear_all` - Delete all notes and folders

## Usage

### Local Development

```bash
# Build
cargo build

# Run with arguments
cargo run -- --vault-path /path/to/vault \
             --spacetime-host http://localhost:3003 \
             --database obsidian-sync

# Or use environment variables
VAULT_PATH=/path/to/vault cargo run
```

### CLI Arguments

| Argument | Env Var | Default | Description |
|----------|---------|---------|-------------|
| `-v, --vault-path` | `VAULT_PATH` | (required) | Path to Obsidian vault |
| `-s, --spacetime-host` | `SPACETIME_HOST` | `http://localhost:3003` | SpacetimeDB URL |
| `-d, --database` | `SPACETIME_DB` | `obsidian-sync` | Database name |

## Deployment

### Prerequisites

1. SpacetimeDB running on NAS (port 3003)
2. Docker installed on NAS

### Steps

1. **Copy project to NAS:**
   ```bash
   scp -r . mikael@192.168.1.161:/volume1/docker/obsidian-spacetime-sync/
   ```

2. **Start SpacetimeDB:**
   ```bash
   ssh mikael@192.168.1.161
   cd /volume1/docker/obsidian-spacetime-sync
   docker-compose up -d spacetimedb
   ```

3. **Publish module (from development machine):**
   ```bash
   cd spacetime-module
   spacetime build
   spacetime publish -s http://192.168.1.161:3003 obsidian-sync
   ```

4. **Start watcher:**
   ```bash
   docker-compose up -d obsidian-spacetime-sync
   ```

### Docker Compose

```yaml
services:
  spacetimedb:
    image: clockworklabs/spacetime
    ports:
      - "3003:3000"
    volumes:
      - spacetimedb-data:/var/lib/spacetimedb

  obsidian-spacetime-sync:
    build: .
    depends_on:
      - spacetimedb
    volumes:
      - /volume1/homes/mikael/ObsidianNAS:/vault:ro
    environment:
      - VAULT_PATH=/vault
      - SPACETIME_HOST=http://spacetimedb:3000
      - SPACETIME_DB=obsidian-sync
```

## Configuration

### Vault Location
- **NAS Path:** `/volume1/homes/mikael/ObsidianNAS/`
- **Docker Mount:** `/vault` (read-only)

> **Note for Two-Way Sync:** The `:ro` flag must be removed (change to `:rw` or default) when implementing two-way sync, or `writer.rs` will fail with "Permission Denied" errors.

### SpacetimeDB
- **Host:** `http://192.168.1.161:3003`
- **Database:** `obsidian-sync`

## Dependencies

```toml
spacetimedb-sdk = "1.0"
notify-debouncer-mini = "0.4"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
walkdir = "2.5"
anyhow = "1.0"
clap = { version = "4", features = ["derive", "env"] }
tracing = "0.1"
tracing-subscriber = "0.3"
filetime = "0.2"
sha2 = "0.10"
hex = "0.4"
```

## Current Limitations

- **One-way sync only** - Changes in Flutter apps don't sync back to vault
- **Folder deletion** - Deleting a folder doesn't remove child notes from DB
- **No conflict resolution** - Last write wins

## Future Enhancements

See [TWO_WAY_PLAN.md](TWO_WAY_PLAN.md) for planned bidirectional sync:

- Startup reconciliation (compare timestamps)
- Subscribe to SpacetimeDB changes
- Write server changes back to vault
- Content hashing to prevent infinite loops

## Related Projects

- **SpacetimeDB Dart SDK:** For Flutter/Dart client apps
- **Obsidian MCP Server:** Reference implementation for vault operations
- **Remotely Save Plugin:** Syncs Obsidian to NAS via WebDAV

## License

MIT
