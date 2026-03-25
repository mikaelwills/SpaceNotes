#!/bin/bash
set -e

NAS_HOST="mikael@100.84.184.121"
NAS_PATH="/volume1/docker/spacenotes"

echo "Pulling latest opencode image on NAS..."
ssh "$NAS_HOST" "docker pull ghcr.io/anomalyco/opencode:latest"

echo "Restarting opencode container..."
ssh "$NAS_HOST" "cd $NAS_PATH && docker-compose stop opencode && docker-compose rm -f opencode && docker-compose up -d opencode"

echo "Pruning old images..."
ssh "$NAS_HOST" "docker image prune -f"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  ✓ OpenCode updated!"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "  OpenCode  http://100.84.184.121:5053"
echo ""
echo "  NOTE: You will need to re-authenticate:"
echo "  ssh $NAS_HOST 'docker exec -it opencode opencode auth login'"
echo ""
