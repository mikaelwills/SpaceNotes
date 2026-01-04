<p align="center">
  <img src="assets/android/mipmap-xxxhdpi/spacenotes2.png" width="128" alt="SpaceNotes Logo" />
</p>

<h1 align="center">SpaceNotes</h1>

**An open-source attempt at the ideal notes solution.**

This project explores what note-taking could look like if you had full control: your own server, plain markdown files, real-time sync across all devices, and AI that can actually help you organize your thoughts. No vendor lock-in, no subscription fees, no compromises on privacy.

It's opinionated, it requires technical setup, and it's not for everyone. But if you've ever been frustrated by cloud services holding your notes hostage, sync conflicts, or AI features locked behind paywalls - this is an attempt to build something better.

Contributions welcome.

![Desktop Notes View](assets/screenshots/desktop-notes.png)
![Desktop AI Chat](assets/screenshots/desktop-chat.png)

<p align="center">
  <img src="assets/screenshots/mobile-notes.png" width="45%" alt="Mobile Notes View" />
  <img src="assets/screenshots/mobile-chat.png" width="45%" alt="Mobile AI Chat" />
</p>

## How it compares

| Feature | SpaceNotes | Obsidian Sync | Notion | Evernote | Notesnook | Syncthing | iCloud/Google | Basic Memory | zk |
|---------|------------|---------------|--------|----------|-----------|-----------|---------------|--------------|-----|
| **Self-hosted** | Yes | No | No | No | Yes | Yes | No | No | N/A |
| **Real-time sync** | Yes | Yes | Yes | Yes | Yes | Delayed | Delayed | Yes | None |
| **Mobile app** | Yes | Yes | Yes | Yes | Yes | Partial | Yes | Web only | No |
| **Web access** | Yes | No | Yes | Yes | Yes | No | Yes | Yes | No |
| **AI integration** | MCP + Chat UI | None | Built-in | Built-in | None | None | None | MCP | None |
| **Plain markdown** | Yes | Yes | No | No | Partial | Yes | Varies | Yes | Yes |
| **Conflict handling** | Auto-resolve | Manual | Auto | Auto | Auto | Manual | Overwrites | Auto | N/A |
| **Cost** | Free | $8/mo | Free/$10/mo | Free/$15/mo | Free/$5/mo | Free | Free tier limits | Paid | Free |
| **Offline editing** | Yes | Yes | Limited | Paid only | Yes | Yes | Yes | Yes | Yes |
| **Data ownership** | Full | Partial | None | None | Full | Full | None | Partial | Full |
| **Export freedom** | Native files | Native files | Lossy export | Lossy export | Markdown | Native files | Varies | Markdown | Native files |
| **End-to-end encrypted** | No | No | No | No | Yes | N/A | No | No | N/A |

**Requirements:**
- A server or laptop.
- Comfort with Docker and basic command line
- A private network setup (Tailscale, WireGuard, or similar)

**Current limitations:**
- No hosted option - you must run your own server
- No E2E encryption - security comes from self-hosting on a private network
- No multi-user collaboration yet
- Early-stage software - expect rough edges

## Components

- **SpacetimeDB** - Real-time database. Clients connect once and receive instant updates.
- **Filesystem Sync Daemon** - Watches your notes folder and syncs bidirectionally with SpacetimeDB.
- **MCP Server** - Lets AI assistants (Claude Code, Cursor, etc.) read/write your notes.
- **OpenCode** (optional) - Headless AI chat server. Provides the chat interface in the Flutter client using free or bring-your-own API keys.
- **[Flutter Client](https://github.com/mikaelwills/spacenotes-client)** - Native apps for iOS, Android, macOS, Windows, Linux, and web.

## Standard Ports

- **5050** - SpacetimeDB (WebSocket/HTTP) - Flutter clients connect here
- **5051** - Web Client (HTTP) - Flutter web app served via nginx
- **5052** - MCP Server (HTTP) - AI assistant integration endpoint
- **5053** - OpenCode (HTTP) - AI chat server for Flutter client

All ports are configurable via `docker-compose.yml`.

## Flutter Client Features

**Mobile (iOS/Android):**
- Recent notes for quick access
- Fuzzy real-time search
- Create and edit notes with markdown
- OpenCode AI chat with access to your notes
- Manage OpenCode chat sessions
- Swipe actions for quick operations

**Desktop (macOS/Windows/Linux/Web):**
- Split-pane view: notes list + editor + AI chat
- Full markdown editor
- Keyboard shortcuts
- Drag and drop file organization

## Quick Start

1. **Download docker-compose.yml:**
   ```bash
   curl -O https://raw.githubusercontent.com/mikaelwills/SpaceNotes/master/docker-compose.yml
   ```

2. **Edit it** - set your notes folder path:
   ```yaml
   volumes:
     - /path/to/your/notes:/vault
   ```
   Replace `/path/to/your/notes` with the absolute path to your markdown folder (e.g., `/home/user/notes` or `/volume1/notes`).

3. **Start:**
   ```bash
   docker-compose up -d
   ```
   Docker pulls the pre-built image. First run takes a minute to download.

4. **Verify it's running:**
   ```bash
   docker logs spacenotes
   ```
   You should see "Watcher started on /vault" when ready.

5. **Access SpaceNotes:**
   - **Web Client**: `http://<your-server-ip>:5051` (includes AI chat)
   - **Mobile App**: Connect to `http://<your-server-ip>:5050` in settings
   - **MCP Server**: `http://<your-server-ip>:5052/mcp` (for Claude Code, Cursor, etc.)
   - **OpenCode API**: `http://<your-server-ip>:5053` (powers the chat UI)

## Updating

To update to the latest version:

```bash
docker-compose pull && docker-compose up -d
```

Your notes are safe - they live on your filesystem, not in the database.

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
- `regex_replace` - Replace text using regex patterns
- `append_to_note` / `prepend_to_note` - Add content to a note
- `delete_note` - Delete a note by ID
- `move_note` - Move/rename a note
- `move_notes_to_folder` - Bulk move multiple notes
- `list_notes_in_folder` - List all notes in a folder
- `create_folder` / `delete_folder` / `move_folder` - Folder operations

## Configuration

Environment variables (set in `docker-compose.yml`):

- `VAULT_PATH` - Path to notes folder inside container (default: `/vault`)
- `SPACETIME_HOST` - SpacetimeDB URL, internal (default: `http://127.0.0.1:3000`)
- `SPACETIME_DB` - Database name (default: `spacenotes`)
- `ANTHROPIC_API_KEY` - Optional, for OpenCode with your own Anthropic key
- `OPENAI_API_KEY` - Optional, for OpenCode with your own OpenAI key

OpenCode configuration is in `opencode.json`. By default it uses the free `opencode/big-pickle` model. Edit this file to change models or add custom agents.

## Project Structure

```
spacenotes/
├── src/                    # Rust sync daemon
├── spacetime-module/       # SpacetimeDB schema and reducers
├── spacenotes-mcp/         # MCP server for AI assistants
├── client-web/             # Flutter web client (pre-built)
├── assets/                 # Screenshots and icons
├── Dockerfile              # All-in-one container build
├── docker-compose.yml      # Container orchestration
├── entrypoint.sh           # Container startup script
├── opencode.json           # OpenCode AI chat configuration
└── nginx.conf              # Web client server config
```

## License

GPL-3.0 - This project is free software. Any derivative work must also be open source under the same license.
