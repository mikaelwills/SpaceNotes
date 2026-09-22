#!/bin/bash
set -e

NAS_HOST="mikael@100.84.184.121"
NAS_PATH="/volume1/docker/spacenotes"
CONTAINER="spacenotes"
WASM_PATH="spacetime-module/target/wasm32-unknown-unknown/release/spacenotes_module.wasm"

cd "$(dirname "$0")"

source "$HOME/.dotfiles/scripts/spacenotes-deploy/lib-hotpatch.sh"

# The recreate below discards the container's writable layer, so anything
# deploy-mcp.sh or deploy-filewatcher.sh copied into the running container
# reverts to the image's copy. Read the record now, report it at the end:
# re-running those scripts is quick, so this is a note to act on, not a gate.
HOTPATCHES="$(read_hotpatches "$NAS_HOST" "$CONTAINER")"

echo "Building spacetime-module for wasm32-unknown-unknown..."
(cd spacetime-module && cargo build --release --target wasm32-unknown-unknown)

if [ ! -f "$WASM_PATH" ]; then
    echo "Build artifact missing: $WASM_PATH"
    exit 1
fi

echo "Copying wasm to NAS..."
rsync -avz --progress "$WASM_PATH" "$NAS_HOST:~/spacenotes-module.wasm"

echo "Create (not start) fresh container, cp wasm in, wipe volume, start..."
ssh "$NAS_HOST" "
    set -e
    cd $NAS_PATH
    docker-compose stop $CONTAINER || true
    docker-compose rm -f $CONTAINER || true
    docker volume rm -f spacenotes_spacetimedb-data || true
    docker-compose create $CONTAINER
    docker cp ~/spacenotes-module.wasm $CONTAINER:/opt/spacetime-module.wasm
    docker-compose start $CONTAINER
    rm ~/spacenotes-module.wasm
"

# The new container's binaries all came from the image, so any previous record
# is stale by definition.
clear_hotpatches "$NAS_HOST" "$CONTAINER"

echo ""
echo "Done. STDB volume wiped, new module published on fresh container boot."

if [ -n "$HOTPATCHES" ] && [ "$HOTPATCHES" != "{}" ]; then
    echo ""
    echo "REVERTED by the recreate — these were hot-patched and are now the image's copies."
    echo "Re-run to restore them:"
    echo "$HOTPATCHES" | python3 -c '
import json, sys
script = {"mcp": "deploy-mcp.sh", "daemon": "deploy-filewatcher.sh"}
for name, info in json.load(sys.stdin).items():
    print("  ~/.dotfiles/scripts/spacenotes-deploy/%-22s  (was %s)"
          % (script.get(name, name), info.get("revision", "?")))
'
fi

echo "Watch: ssh $NAS_HOST 'docker logs -f $CONTAINER'"
