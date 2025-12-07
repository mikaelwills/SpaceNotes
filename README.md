# SpaceNotes

**Real-time note sync without the cloud.**

No subscriptions. No cloud storage. No vendor lock-in. Just your notes, synced instantly across all your devices.

SpaceNotes is the notes app that doesn't exist yet: real-time cross-platform sync with **zero recurring costs** and **complete data ownership**. Your notes stay on your hardware as plain markdown files - not trapped in someone else's database.

- **No cloud** - Runs entirely on your own server. Your data never touches third-party infrastructure.
- **No costs** - Zero monthly fees. No per-device charges, no storage limits.
- **True ownership** - Plain markdown files in a folder. Use any editor. Switch apps anytime. Your notes are yours.
- **Real-time sync** - Edit on your phone, see it on your desktop instantly. Thanks to SpacetimeDb.
- **LLM-ready** - Built-in MCP server lets AI assistants read and write your notes directly.

## How It Works

```
Your Server (NAS/VPS)                    Your Devices
┌─────────────────────────────────┐      ┌─────────────────┐
│  Notes Folder ←→ Rust Daemon ←→ │ ←──→ │  Flutter App    │
│        ↓                        │      │  (iOS/Android/  │
│  SpacetimeDB (real-time DB)     │      │   Desktop)      │
│        ↓                        │      └─────────────────┘
│  MCP Server ←───────────────────│←───→ Claude / LLMs
└─────────────────────────────────┘
```

The Rust daemon watches your notes folder and syncs bidirectionally with SpacetimeDB. Clients connect via WebSocket and receive instant updates. Edit a note on your phone - it appears on your desktop in milliseconds.

## Requirements

- Docker and Docker Compose
- A server accessible from your devices (home server, NAS, VPS)
- Network access via Tailscale, VPN, or port forwarding

## Quick Start

1. **Clone and configure:**
   ```bash
   git clone https://github.com/mikaelwills/SpaceNotes.git
   cd SpaceNotes
   cp docker-compose.example.yml docker-compose.yml
   ```

2. **Edit `docker-compose.yml`:**
   ```yaml
   volumes:
     - /absolute/path/to/your/notes:/vault  # Your notes folder (must be absolute path)
     - spacetimedb-data:/var/lib/spacetimedb
   ports:
     - "3000:3000"  # Change 3000 if port is already in use
   ```

3. **Build and start:**
   ```bash
   docker-compose up -d
   ```
   First build takes a few minutes (compiling Rust). Subsequent starts are instant.

4. **Verify it's running:**
   ```bash
   docker logs spacenotes
   ```
   You should see "Watcher started on /vault" when ready.

5. **Connect the Flutter app** to `http://<your-server-ip>:3000`
   - Same machine: `http://localhost:3000`
   - Over Tailscale: `http://<tailscale-ip>:3000`
   - Find your Tailscale IP: `tailscale ip -4`

## Components

| Component | Description |
|-----------|-------------|
| **spacenotes** (this repo) | Rust daemon that syncs filesystem ↔ SpacetimeDB |
| **spacenotes-flutter** | Cross-platform client app (iOS, Android, macOS, Windows, Linux) |
| **spacenotes-mcp** | MCP server for AI assistant integration |

## Configuration

| Environment Variable | Default | Description |
|---------------------|---------|-------------|
| `VAULT_PATH` | (required) | Path to notes folder inside container |
| `SPACETIME_HOST` | `http://localhost:3000` | SpacetimeDB server URL |
| `SPACETIME_DB` | `spacenotes` | Database name |

## Network Setup

SpaceNotes requires your devices to reach your server. Options:

- **Tailscale (recommended)** - Zero-config mesh VPN. Install on server and devices, connect via Tailscale IP. No port forwarding needed.
- **Local network** - If server and devices are on the same WiFi, use the server's local IP (e.g., `192.168.1.x`).
- **WireGuard/OpenVPN** - Traditional VPN to your home network.
- **Port forwarding** - Expose the port on your router (less secure, not recommended).
- **Cloudflare Tunnel** - Free, secure tunneling without opening ports.

## Architecture

The system uses [SpacetimeDB](https://spacetimedb.com), a real-time database that combines:
- Relational data storage
- WebSocket subscriptions for instant updates
- Server-side logic (reducers) that run atomically

This means clients don't poll for changes - they subscribe once and receive updates pushed to them instantly.

## Current Limitations

- **Single user** - No multi-user authentication yet
- **Last-write-wins** - No conflict resolution for simultaneous edits
- **Markdown only** - Designed for `.md` files

## License

MIT
