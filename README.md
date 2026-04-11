<p align="center">
  <img src="assets/android/mipmap-xxxhdpi/spacenotes2.png" width="128" alt="SpaceNotes Logo" />
</p>

<h1 align="center">SpaceNotes</h1>

**Self-hosted notes with real-time AI agent integration.**

SpaceNotes is a self-hosted note-taking system with real-time sync and a built-in bridge between your notes and AI coding agents. Your notes live as plain markdown files on your own server. AI assistants like Claude Code can read, write, and discuss them through MCP — and SpaceChannel lets you monitor and interact with multiple Claude Code sessions in real-time from the Flutter app.

No vendor lock-in, no subscription fees, no compromises on privacy. It's opinionated, it requires technical setup, and it's not for everyone.

Contributions welcome.

![Desktop Notes View](assets/screenshots/desktop-notes.png)
![Desktop AI Chat](assets/screenshots/desktop-chat.png)

<p align="center">
  <img src="assets/screenshots/mobile-notes.png" width="45%" alt="Mobile Notes View" />
  <img src="assets/screenshots/mobile-chat.png" width="45%" alt="Mobile AI Chat" />
</p>

## How it compares

| Feature | SpaceNotes | Obsidian Sync | Notion | Notesnook | Basic Memory |
|---------|------------|---------------|--------|-----------|--------------|
| **Self-hosted** | Yes | No | No | Yes | No |
| **Real-time sync** | Yes | Yes | Yes | Yes | Yes |
| **Mobile + Web** | Yes | Mobile only | Yes | Yes | Web only |
| **AI integration** | MCP + Agent bridge | None | Built-in | None | MCP |
| **Plain markdown** | Yes | Yes | No | Partial | Yes |
| **Data ownership** | Full | Partial | None | Full | Partial |
| **Cost** | Free | $8/mo | Free/$10/mo | Free/$5/mo | Paid |

**Requirements:**
- A server or laptop.
- Comfort with Docker and basic command line
- A private network setup (Tailscale, WireGuard, or similar)

**Current limitations:**
- No hosted option - you must run your own server
- No E2E encryption - security comes from self-hosting on a private network
- No multi-user collaboration yet
- Early-stage software - expect rough edges

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│  Your Server (Docker)                                           │
│                                                                 │
│  ┌──────────────┐  ┌────────────┐  ┌─────────────────────────┐  │
│  │ SpacetimeDB  │  │ Sync Daemon│  │ SpaceChannel Server     │  │
│  │ (notes DB)   │◄─┤ (fs↔db)   │  │ (WebSocket relay)       │  │
│  └──────┬───────┘  └────────────┘  │                         │  │
│         │                          │  :5054 ← Flutter app    │  │
│  ┌──────┴───────┐                  │  :5055 ← Claude Code    │  │
│  │  MCP Server  │                  │  :5056 ← Webhooks       │  │
│  │  (note CRUD) │                  └──────┬──────────────────┘  │
│  └──────────────┘                         │                     │
│                                    ┌──────┴──────────────────┐  │
│  ┌──────────────┐                  │ Note-Assistant           │  │
│  │  Web Client  │                  │ (Claude Code in Docker)  │  │
│  │  (nginx)     │                  └─────────────────────────┘  │
│  └──────────────┘                                               │
│  ┌──────────────┐                                               │
│  │  OpenCode    │                                               │
│  │  (chat API)  │                                               │
│  └──────────────┘                                               │
└─────────────────────────────────────────────────────────────────┘
        ▲                                    ▲
        │ :5050 (notes sync)                 │ :5054 (sessions)
        │ :5051 (web UI)                     │ :5055 (agents)
        │ :5052 (MCP)                        │
        │ :5053 (chat)                       │
        ▼                                    ▼
┌──────────────────┐              ┌─────────────────────┐
│  Flutter Client  │              │  Claude Code (local) │
│  iOS/Android/    │              │  space-channel.ts    │
│  macOS/Web       │              │  MCP server per      │
│                  │              │  session              │
└──────────────────┘              └─────────────────────┘
```

## Components

- **SpacetimeDB** - Real-time database. Clients connect once and receive instant updates.
- **Filesystem Sync Daemon** - Watches your notes folder and syncs bidirectionally with SpacetimeDB.
- **MCP Server** - Lets AI assistants (Claude Code, Cursor, etc.) read/write your notes.
- **SpaceChannel Server** - Rust WebSocket relay that bridges Claude Code sessions and the Flutter app. Handles session registration, message routing, tool event streaming, status updates, heartbeat monitoring, and message history replay.
- **SpaceChannel MCP Client** (`space-channel.ts`) - Runs as an MCP server inside each Claude Code session. Connects to the SpaceChannel Server via WebSocket and exposes `reply` and `edit_message` tools so Claude can send messages to the Flutter app. Also forwards hook events (thinking, idle, tool use) as status updates.
- **Note-Assistant** - A headless Claude Code instance running in Docker on your server. Always available from the Flutter app for note-related tasks. Connects to SpaceChannel like any other session.
- **OpenCode** (optional) - Headless AI chat server. Provides the chat interface in the Flutter client using free or bring-your-own API keys.
- **[Flutter Client](https://github.com/mikaelwills/spacenotes-client)** - Native apps for iOS, Android, macOS, Windows, Linux, and web. Includes a session dashboard for monitoring all connected Claude Code agents in real-time.

## Standard Ports

- **5050** - SpacetimeDB (WebSocket/HTTP) - Flutter clients connect here for note sync
- **5051** - Web Client (HTTP) - Flutter web app served via nginx
- **5052** - MCP Server (HTTP) - AI assistant integration endpoint
- **5053** - OpenCode (HTTP) - AI chat server for Flutter client
- **5054** - SpaceChannel (WebSocket) - Flutter app connects here for agent sessions
- **5055** - SpaceChannel (WebSocket) - Claude Code agents connect here
- **5056** - SpaceChannel (HTTP) - Webhook endpoint for external integrations

All ports are configurable via `docker-compose.yml`.

## Flutter Client Features

**Notes:**
- Real-time sync across all devices via SpacetimeDB
- Fuzzy search, folder organization, markdown editing
- Inline AI chat within any note
- Offline editing with automatic conflict resolution

**Agent Dashboard:**
- Live session cards for all connected Claude Code agents
- Thinking/idle/tool-use status indicators in real-time
- Send messages to any session and receive replies
- Tool event streaming — see what each agent is doing as it happens
- Message history replay on reconnect

**Mobile (iOS/Android):**
- Recent notes, pull-up chat within notes
- Session dashboard and per-session chat

**Desktop (macOS/Windows/Linux/Web):**
- Split-pane view: notes list + editor + AI chat
- Full markdown editor
- Drag and drop file organization

## SpaceChannel

SpaceChannel is a real-time communication bridge between Claude Code sessions and the Flutter app. Each Claude Code session runs a `space-channel.ts` MCP server that connects to the SpaceChannel Server via WebSocket. The Flutter app connects on a separate port and receives all session activity.

**What it enables:**
- See all active Claude Code sessions in the Flutter app with live status (thinking, idle, tool use)
- Send messages to any session from your phone and receive replies
- View tool events as they happen (file edits, bash commands, etc.)
- Session message history — reconnecting clients get caught up automatically
- Heartbeat monitoring — dead sessions are detected within 60 seconds and removed
- Webhook ingestion — external services (Trello, CI, etc.) can push messages into sessions

**Session types:**
- **Local sessions** — Claude Code running on your machine, connected via `spacechannel-session.sh` launcher
- **Note-Assistant** — A headless Claude Code container on the server, always available for note tasks
- **Webhook sessions** — Auto-registered when external webhooks arrive targeting a session name

## Claude Code Integration

SpaceNotes integrates with Claude Code at two levels: MCP for note access, and SpaceChannel for real-time session communication.

### 1. MCP — Note Access

Add the SpaceNotes MCP server so Claude Code can read and write your notes (see [MCP Integration](#mcp-integration-claude-code) below).

### 2. SpaceChannel — Session Bridge

The `space-channel.ts` MCP server runs inside each Claude Code session and connects it to the SpaceChannel Server. It uses Claude Code's hook system to stream status updates and tool events to the Flutter app.

**Setup with the launcher script:**

The `spacechannel-session.sh` script handles everything — it finds a free port for the hook server, writes HTTP hooks into `.claude/settings.local.json`, and launches Claude Code with the space-channel MCP server configured:

```bash
# Usage: spacechannel-session.sh <session-name> <project-name> <skill>
~/.dotfiles/scripts/spacechannel-session.sh myproject myproject my-workflow-skill
```

Create shell aliases for your projects:
```bash
alias myproject='$HOME/.dotfiles/scripts/spacechannel-session.sh myproject myproject my-skill'
```

**What Claude Code gets:**
- `reply` tool — send a message to the Flutter app
- `edit_message` tool — update a previously sent message
- Automatic status streaming via hooks (thinking indicators, tool events)
- Session registration and heartbeat with the SpaceChannel Server

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
   - **SpaceChannel**: `ws://<your-server-ip>:5054/ws` (Flutter agent dashboard), `ws://<your-server-ip>:5055/ws` (Claude Code sessions)

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
- `delete_note` / `delete_notes` - Delete one or multiple notes by ID
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

```

## License

GPL-3.0 - This project is free software. Any derivative work must also be open source under the same license.
