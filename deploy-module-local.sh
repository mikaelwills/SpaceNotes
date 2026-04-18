#!/bin/bash
set -e

NAS_HOST="mikael@100.84.184.121"
NAS_PATH="/volume1/docker/spacenotes"
CONTAINER="spacenotes"
WASM_PATH="spacetime-module/target/wasm32-unknown-unknown/release/spacenotes_module.wasm"

cd "$(dirname "$0")"

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

echo ""
echo "Done. STDB volume wiped, new module published on fresh container boot."
echo "Watch: ssh $NAS_HOST 'docker logs -f $CONTAINER'"
