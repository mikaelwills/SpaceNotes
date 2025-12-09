#!/bin/bash
set -e

FLUTTER_REPO="/Users/mikaelwills/Productivity/Development/Flutter/spacenotes_client"
CLIENT_DIR="./client-web"

echo "Building Flutter web client..."
cd "$FLUTTER_REPO"
flutter build web --release

echo "Copying build to $CLIENT_DIR..."
cd - > /dev/null
rm -rf "$CLIENT_DIR"
mkdir -p "$CLIENT_DIR"
cp -r "$FLUTTER_REPO/build/web/"* "$CLIENT_DIR/"

echo "Client updated. Run 'git add client-web && git commit' to commit."
