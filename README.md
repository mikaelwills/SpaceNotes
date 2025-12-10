# SpaceNotes

**Real-time note sync without the cloud.**

Your notes, synced instantly across all your devices - without paying for cloud subscriptions, without corporations mining your data, without trusting your private thoughts to someone else's servers. 
True privacy. True ownership. Just plain markdown files across your own hardware.

- **Self-hosted** - Runs entirely on your own server and your TailScale/Wireguard network, your data never touches third-party infrastructure. No monthly fees. No per-device charges. No storage limits.
- **Real-time sync** - Edit on your phone, see it on your desktop in milliseconds. Powered by SpacetimeDB.
- **True ownership** - Plain markdown files in a folder. Use any editor. Switch apps anytime. Your notes are free and yours.
- **AI-ready** - Built-in MCP server.

## Components

- **SpacetimeDB** - Real-time database. Clients connect once and receive instant updates.
- **Filesystem sync Daemon** - Watches your notes folder and syncs bidirectionally with SpacetimeDB.
- **MCP Server** - Let AI services read/write your notes directly.
- **Flutter Clients** - Native apps for iOS, Android, macOS, Windows, Linux, and web.

## Standard Ports

- **5050** - SpacetimeDB (WebSocket/HTTP) - Flutter clients connect here
- **5051** - Web Client (HTTP) - Flutter web app served via nginx
- **5052** - MCP Server (HTTP) - AI assistant integration endpoint

All ports are configurable via `docker-compose.yml`.

## Requirements

- Docker and Docker Compose
- A server accessible from your devices (home server, NAS, VPS)
- Network access via Tailscale, VPN, or port forwarding

## Quick Start

1. **Clone:**
   ```bash
   git clone https://github.com/mikaelwills/SpaceNotes.git
   cd SpaceNotes
   ```

2. **Edit `docker-compose.yml`** - set your notes folder path:
   ```yaml
   volumes:
     - /path/to/your/notes:/vault
   ```
   Replace `/path/to/your/notes` with the absolute path to your markdown folder (e.g., `/home/user/notes` or `/volume1/notes`).

3. **Build and start:**
   ```bash
   docker-compose up -d
   ```
   First build compiles Rust and takes several minutes. Subsequent starts are instant.

4. **Verify it's running:**
   ```bash
   docker logs spacenotes
   ```
   You should see "Watcher started on /vault" when ready.

5. **Access SpaceNotes:**
   - **Web Client**: `http://<your-server-ip>:5051`
   - **SpacetimeDB API**: `http://<your-server-ip>:5050` (for mobile app)
   - **MCP Server**: `http://<your-server-ip>:5052/mcp` (for AI assistants)

## MCP Integration (Claude Code)

SpaceNotes includes an MCP server that lets AI assistants read and write your notes.

### Configure Claude Code

Add to your `~/.claude.json`:

```json
{
  "mcpServers": {
    "spacenotes-mcp": {
      "type": "http",
      "url": "http://<your-server-ip>:5052/mcp"
    }
  }
}
```

Or use the CLI:
```bash
claude mcp add spacenotes-mcp --type http --url "http://<your-server-ip>:5052/mcp" --scope user
```

### Available MCP Tools

- `search_notes` - Search notes by title, path, or content
- `get_note` - Get full content of a note by ID or path
- `create_note` - Create a new note with content
- `edit_note` - Find and replace text in a note
- `append_to_note` / `prepend_to_note` - Add content to a note
- `delete_note` - Delete a note by ID
- `move_note` - Move/rename a note
- `list_notes_in_folder` - List all notes in a folder
- `create_folder` / `delete_folder` / `move_folder` - Folder operations

## Configuration

Environment variables (set in `docker-compose.yml`):

- `VAULT_PATH` - Path to notes folder inside container (default: `/vault`)
- `SPACETIME_HOST` - SpacetimeDB URL, internal (default: `http://127.0.0.1:3000`)
- `SPACETIME_DB` - Database name (default: `spacenotes`)

## Network Setup

SpaceNotes requires your devices to reach your server. Options:

- **Tailscale (recommended)** - Zero-config mesh VPN. Install on server and devices, connect via Tailscale IP. No port forwarding needed.
- **Local network** - If server and devices are on the same WiFi, use the server's local IP (e.g., `192.168.1.x`).
- **WireGuard/OpenVPN** - Traditional VPN to your home network.
- **Cloudflare Tunnel** - Free, secure tunneling without opening ports.
- **Port forwarding** - Expose ports on your router (less secure).

## Project Structure

```
spacenotes/
├── src/                    # Rust sync daemon
├── spacetime-module/       # SpacetimeDB schema and reducers
├── spacenotes-mcp/         # MCP server for AI assistants
├── client-web/             # Flutter web client (built artifact)
├── Dockerfile              # All-in-one container build
├── docker-compose.yml      # Container orchestration
├── entrypoint.sh           # Container startup script
└── deploy-to-nas.sh        # NAS deployment helper
```

## Development

### Building Locally

```bash
# Build the Docker image
docker-compose build

# Run with logs
docker-compose up

# Rebuild after code changes
docker-compose up --build
```

### Modifying the SpacetimeDB Schema

1. Edit `spacetime-module/src/lib.rs`
2. Regenerate Rust bindings:
   ```bash
   spacetime generate --lang rust --out-dir src/generated --project-path spacetime-module
   ```
3. Rebuild and deploy

## Current Limitations

- **Single user** - No multi-user authentication yet
- **Last-write-wins** - No conflict resolution for simultaneous edits
- **Markdown only** - Designed for `.md` files

## License

MIT
