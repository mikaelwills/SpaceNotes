#!/bin/bash
set -e

NAS_HOST="mikael@192.168.1.161"
CONTAINER="spacenotes"
REMOTE_PATH="/var/www/html"

echo "Building Flutter web client..."
./build-client.sh dev

echo "Copying to NAS..."
rsync -az --delete client-web/ "$NAS_HOST:~/client-web-dev/"

echo "Deploying to container..."
ssh "$NAS_HOST" "docker cp ~/client-web-dev/. $CONTAINER:$REMOTE_PATH && rm -rf ~/client-web-dev"

echo ""
echo "Done! Refresh http://192.168.1.161:5051"
