#!/bin/bash
set -e

NAS_HOST="mikael@100.84.184.121"
NAS_PATH="/volume1/docker/spacenotes"
IMAGE_NAME="spacenotes-claude-code:latest"
TARBALL="/tmp/claude-code-image.tar.gz"

cd "$(dirname "$0")"

echo "Building claude-code image for linux/amd64..."
docker buildx build --platform linux/amd64 --load -t "$IMAGE_NAME" claude-code/

echo "Exporting image..."
docker save "$IMAGE_NAME" | gzip > "$TARBALL"

echo "Transferring to NAS..."
rsync -avz --progress "$TARBALL" "$NAS_HOST:$NAS_PATH/"

echo "Loading image and restarting on NAS..."
ssh "$NAS_HOST" "cd $NAS_PATH && \
  docker load < claude-code-image.tar.gz && \
  docker-compose stop claude-code && \
  docker-compose rm -f claude-code && \
  docker-compose up -d claude-code && \
  rm claude-code-image.tar.gz"

echo "Pruning old images..."
ssh "$NAS_HOST" "docker image prune -f"

rm "$TARBALL"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Claude Code deployed!"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "  Fakechat WS  ws://100.84.184.121:5054/ws"
echo ""
echo "  First time only:"
echo "  ssh $NAS_HOST 'docker exec -it claude-code claude'"
echo "  Login with Max plan, then: docker-compose restart claude-code"
echo ""
echo "  Logs: ssh $NAS_HOST 'docker logs claude-code -f'"
echo ""
