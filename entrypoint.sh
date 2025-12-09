#!/bin/bash
set -e

echo "Starting SpaceNotes..."

# Start SpacetimeDB in background (bind to 0.0.0.0 for external access)
echo "Starting SpacetimeDB..."
spacetime start --listen-addr 0.0.0.0:3000 --data-dir /var/lib/spacetimedb &
STDB_PID=$!

# Wait for SpacetimeDB to be ready
echo "Waiting for SpacetimeDB to be ready..."
for i in {1..30}; do
    if curl -s http://127.0.0.1:3000/health > /dev/null 2>&1; then
        echo "SpacetimeDB is ready"
        break
    fi
    if [ $i -eq 30 ]; then
        echo "Timeout waiting for SpacetimeDB"
        exit 1
    fi
    sleep 1
done

# Publish the pre-built WASM module
echo "Publishing SpacetimeDB module..."
spacetime publish "$SPACETIME_DB" --server http://127.0.0.1:3000 -y --bin-path /opt/spacetime-module.wasm --anonymous || {
    echo "Module may already be published, continuing..."
}

# Start the MCP server in background
echo "Starting MCP server..."
spacenotes-mcp &
MCP_PID=$!

# Start the sync daemon (foreground - keeps container running)
echo "Starting sync daemon..."
spacenotes \
    --vault-path "$VAULT_PATH" \
    --spacetime-host "$SPACETIME_HOST" \
    --database "$SPACETIME_DB" &
DAEMON_PID=$!

# Wait for any process to exit
wait -n $STDB_PID $MCP_PID $DAEMON_PID

# If any process exits, kill the others and exit
echo "A process exited, shutting down..."
kill $STDB_PID $MCP_PID $DAEMON_PID 2>/dev/null
exit 1
