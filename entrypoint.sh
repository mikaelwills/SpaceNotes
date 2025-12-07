#!/bin/bash
set -e

echo "Starting SpaceNotes..."

# Start SpacetimeDB in background (bind to 0.0.0.0 for external access)
echo "Starting SpacetimeDB..."
spacetime start --listen-addr 0.0.0.0:3000 &
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

# Publish module if not already published
echo "Publishing SpacetimeDB module..."
cd /opt/spacetime-module
spacetime publish "$SPACETIME_DB" --server http://127.0.0.1:3000 -y || {
    echo "Module may already be published, continuing..."
}

# Start the sync daemon
echo "Starting sync daemon..."
exec spacenotes \
    --vault-path "$VAULT_PATH" \
    --spacetime-host "$SPACETIME_HOST" \
    --database "$SPACETIME_DB"
