# Obsidian SpacetimeDB Sync - Teaching Project

This is a RUST LEARNING EXERCISE. You are teaching the user Rust through building a file watcher that syncs an Obsidian vault to SpacetimeDB.

## Infrastructure:
- **Local SpacetimeDB (Dart SDK testing)**: `http://127.0.0.1:3000` (localhost, used for other projects)
- **Production SpacetimeDB Server (Obsidian Sync)**: `http://192.168.1.161:3003` (NAS)
- **Database Name**: `obsidian-sync`
- **NAS Deploy Target**: `mikael@192.168.1.161:/volume1/docker/obsidian-spacetime-sync`

**IMPORTANT**: This project uses the NAS SpacetimeDB at `192.168.1.161:3003`, NOT localhost. Always specify `--server http://192.168.1.161:3003` when using spacetime CLI commands for this project.

## Maintenance Operations:

### How to Completely Wipe and Reset the Database

**When to use:** If the database gets into a bad state (e.g., stuck folders with trailing slashes, corrupt data, etc.)

**Prerequisites:**
1. Backup your vault files to another location
2. Stop any Flutter clients connected to the database

**Steps:**

```bash
# 1. Wipe and restart SpacetimeDB on NAS
ssh mikael@192.168.1.161 "cd /volume1/docker/obsidian-spacetime-sync && docker-compose stop spacetimedb && docker-compose rm -f spacetimedb && docker volume rm obsidian-spacetime-sync_spacetimedb-data && docker-compose up -d spacetimedb"

# 2. Accept new server fingerprint
echo "y" | spacetime server fingerprint http://192.168.1.161:3003

# 3. Login to server (required for SQL access)
spacetime logout
spacetime login --server-issued-login http://192.168.1.161:3003

# 4. Republish the module (WITHOUT --anonymous for SQL access)
spacetime publish obsidian-sync \
  --project-path spacetime-module \
  --server http://192.168.1.161:3003 \
  -y

# 5. Deploy and restart the Rust daemon
./deploy-to-nas.sh

# 6. Check logs to verify sync is working
ssh mikael@192.168.1.161 'docker logs obsidian-spacetime-sync --tail 50'
```

**What happens:**
- All data in SpacetimeDB is deleted
- Fresh database is created with your named identity (allows SQL queries)
- Module is republished with latest schema
- Rust daemon scans local vault and uploads all notes/folders
- Everything is back in sync with clean data

### Debugging with SQL Queries

**Important:** You must publish WITHOUT `--anonymous` to use SQL queries.

```bash
# Count all notes
spacetime sql obsidian-sync "SELECT COUNT(*) FROM note" --server http://192.168.1.161:3003

# List notes in a specific folder
spacetime sql obsidian-sync "SELECT id, path FROM note WHERE path LIKE 'FolderName/%' ORDER BY path" --server http://192.168.1.161:3003

# Check for duplicate paths
spacetime sql obsidian-sync "SELECT path, COUNT(*) as count FROM note GROUP BY path HAVING count > 1" --server http://192.168.1.161:3003
```

## Teaching Structure:
- Phase 1: Project setup and basic structure
- Phase 2: Vault scanner (read all notes)
- Phase 3: SpacetimeDB client connection
- Phase 4: File watcher with notify
- Phase 5: Debouncing and event handling
- Phase 6: Docker deployment

## Teaching Approach:
- Each step ≤35 lines
- User types code themselves
- Build toward testable milestones
- Answer questions before proceeding

## Phase Roadmap:

### Phase 1: Project Setup
- [x] Step 1: CLI argument parsing with clap
- [x] Step 2: Config struct and environment variables
- [x] Step 3: Main async runtime setup (deferred to final wiring)

### Phase 2: Vault Scanner
- [x] Step 1: Note struct definition
- [x] Step 2: Folder struct definition
- [x] Step 3: Frontmatter parsing
- [x] Step 4: Walk directory and collect notes
- [x] Step 5: Walk directory and collect folders

### Phase 3: SpacetimeDB Client
- [x] Step 1: Generate client bindings
- [x] Step 2: Connect to SpacetimeDB
- [x] Step 3: Call upsert reducers for initial sync
- [x] Step 4: Individual note/folder operations (delete, move)

### Phase 4: File Watcher
- [x] Step 1: Extract read_note_at helper function
- [x] Step 2: Add notify-debouncer-mini dependency
- [x] Step 3: Create watcher with debouncing
- [x] Step 4: Wire everything in main.rs

### Phase 5: Debouncing
- [ ] Step 1: Debounce logic for rapid changes
- [ ] Step 2: Batch processing

### Phase 6: Deployment
- [x] Step 1: Dockerfile creation

## File Watcher Architecture

**Change Detection Flow:**
1. `notify-debouncer-mini` detects filesystem changes (2s debounce)
2. Watcher reads file and extracts frontmatter UUID
3. `tracker.has_changed()` - read-only check if content differs from cached hash
4. If unchanged, skip (echo prevention from daemon's own writes)
5. If changed, inject UUID if missing, then `tracker.is_modified()` - updates cache and syncs to SpacetimeDB

**Key Components:**
- `ContentTracker.has_changed()` - read-only hash comparison (no side effects)
- `ContentTracker.is_modified()` - hash comparison + cache update (side effect)
- Separation prevents double-update bug where echo check consumed the change detection

**Sources Synced:**
- Obsidian desktop writes → SpacetimeDB
- MCP server writes → SpacetimeDB
- SpacetimeDB updates → local filesystem

**Safety Features:**
- Prevents duplicate UUID injection if DB already knows the note
- Skips hidden files/folders and `@eaDir` (Synology system folders)
- Raw text verification before UUID injection


