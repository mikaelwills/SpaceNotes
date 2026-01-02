#!/bin/bash
set -e

NAS_HOST="mikael@100.84.184.121"
CONTAINER="spacenotes"
REMOTE_PATH="/var/www/html"

echo "Building Flutter web client..."
./build-client.sh dev

echo "Copying to NAS..."
rsync -az --delete client-web/ "$NAS_HOST:~/client-web-dev/"

echo "Deploying to container..."
ssh "$NAS_HOST" "docker cp ~/client-web-dev/. $CONTAINER:$REMOTE_PATH && rm -rf ~/client-web-dev"

echo ""
echo "Done! Refresh http://100.84.184.121:5051"
