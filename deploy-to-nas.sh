#!/bin/bash
set -e

NAS_HOST="mikael@192.168.1.161"
NAS_PATH="/volume1/docker/obsidian-spacetime-sync"

echo "Building for Linux..."
cargo build --release --target x86_64-unknown-linux-musl

echo "Syncing to NAS..."
rsync -avz \
    --exclude '.git' \
    --exclude 'target' \
    --exclude 'spacetime-module/target' \
    -e ssh . "$NAS_HOST:$NAS_PATH/"

# Copy just the binary
echo "Copying binary..."
scp -O target/x86_64-unknown-linux-musl/release/obsidian-spacetime-sync "$NAS_HOST:$NAS_PATH/target/x86_64-unknown-linux-musl/release/"

echo "Building Docker image on NAS..."
ssh "$NAS_HOST" "cd $NAS_PATH && docker-compose build obsidian-spacetime-sync"

echo "Restarting container..."
ssh "$NAS_HOST" "cd $NAS_PATH && docker-compose up -d obsidian-spacetime-sync"

echo "Deployment complete!"
echo "Check logs with: ssh $NAS_HOST 'docker logs obsidian-spacetime-sync -f'"
