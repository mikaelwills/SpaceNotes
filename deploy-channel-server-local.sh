#!/bin/bash
set -e

NAS_HOST="mikael@100.84.184.121"
CONTAINER="spacenotes"

cd "$(dirname "$0")"

echo "Building space-channel-server for x86_64 Linux (musl)..."
cross build --release --package space-channel-server --target x86_64-unknown-linux-musl

echo "Copying binary to NAS..."
rsync -avz --progress target/x86_64-unknown-linux-musl/release/space-channel-server "$NAS_HOST:~/space-channel-server"

echo "Copying into container, killing old process, starting new..."
ssh "$NAS_HOST" "docker cp ~/space-channel-server $CONTAINER:/usr/local/bin/space-channel-server && docker exec $CONTAINER pkill -f space-channel-server 2>/dev/null; sleep 1; docker exec -d $CONTAINER bash -c '/usr/local/bin/space-channel-server > /tmp/channel-server.log 2>&1' && rm ~/space-channel-server"

echo ""
echo "Done! SpaceChannelServer updated."
echo "Ports: 5054 (Flutter WS), 5055 (SpaceChannel WS), 5056 (Webhook HTTP)"
