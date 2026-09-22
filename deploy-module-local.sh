#!/bin/bash
set -e

NAS_HOST="mikael@100.84.184.121"
NAS_PATH="/volume1/docker/spacenotes"
CONTAINER="spacenotes"
WASM_PATH="spacetime-module/target/wasm32-unknown-unknown/release/spacenotes_module.wasm"

DISCARD_HOTPATCHES=false
for arg in "$@"; do
    case "$arg" in
        --discard-hotpatches) DISCARD_HOTPATCHES=true ;;
    esac
done

cd "$(dirname "$0")"

source "$HOME/.dotfiles/scripts/spacenotes-deploy/lib-hotpatch.sh"

# This deploy recreates the container, so anything deploy-mcp.sh or
# deploy-filewatcher.sh copied into the running one is about to be replaced by
# the image's copy. Checked before the build so it fails in seconds.
HOTPATCHES="$(read_hotpatches "$NAS_HOST" "$CONTAINER")"
if [ -n "$HOTPATCHES" ] && [ "$HOTPATCHES" != "{}" ] && [ "$DISCARD_HOTPATCHES" != "true" ]; then
    echo "ERROR: the running container has hot-patched binaries that are not in the image:" >&2
    echo "$HOTPATCHES" | python3 -c '
import json, sys
for name, info in json.load(sys.stdin).items():
    dirty = " (built from a dirty tree)" if info.get("dirty") else ""
    print("  %-8s  %s  at %s%s" % (name, info.get("revision","?"), info.get("at","?"), dirty))
' >&2
    echo "" >&2
    echo "Recreating the container would revert them to the image's versions." >&2
    echo "" >&2
    echo "  To keep them:    run build-and-push.sh first, then this script." >&2
    echo "  To re-apply:     re-run each deploy script after this one finishes." >&2
    echo "  To discard them: re-run with --discard-hotpatches." >&2
    exit 1
fi

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
    echo "REVERTED by the recreate — re-run these to get them back:"
    echo "$HOTPATCHES" | python3 -c '
import json, sys
script = {"mcp": "deploy-mcp.sh", "daemon": "deploy-filewatcher.sh"}
for name in json.load(sys.stdin):
    print("  ~/.dotfiles/scripts/spacenotes-deploy/%s" % script.get(name, name))
'
fi

echo "Watch: ssh $NAS_HOST 'docker logs -f $CONTAINER'"
