#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
cargo build --locked --release --lib --target wasm32-unknown-unknown
container=$(docker run -d -p 127.0.0.1::27017 mongo:7)
trap 'docker rm -f "$container" >/dev/null' EXIT HUP INT TERM
attempt=0
until docker exec "$container" mongosh --quiet --eval 'db.runCommand({ping: 1}).ok' 2>/dev/null | grep -q 1; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 120 ]; then docker logs "$container"; exit 1; fi
    sleep 0.5
done
port=$(docker port "$container" 27017/tcp | head -1 | sed 's/.*://')
export MONGODB_URL="mongodb://127.0.0.1:$port"
cargo test --locked --test integration -- --ignored --test-threads=1 --skip x509
