#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
cargo build --locked --release --lib --target wasm32-unknown-unknown
work=$(CDPATH= cd -- "$root/../../.." && pwd)/target/x509-test
rm -rf "$work"
mkdir -p "$work"
container=""
trap '[ -z "$container" ] || docker rm -f "$container" >/dev/null; rm -rf "$work"' EXIT HUP INT TERM
cd "$work"
openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.pem -subj "/CN=Test CA" -days 1 2>/dev/null
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\n' > server.ext
printf 'extendedKeyUsage=clientAuth\n' > client.ext
for name in server client; do
    openssl req -newkey rsa:2048 -nodes -keyout "$name.key" -out "$name.csr" -subj "/CN=$([ "$name" = server ] && echo localhost || echo client)" 2>/dev/null
    openssl x509 -req -in "$name.csr" -CA ca.pem -CAkey ca.key -CAcreateserial -out "$name.crt" -days 1 -extfile "$name.ext" 2>/dev/null
    cat "$name.crt" "$name.key" > "$name.pem"
done
chmod 644 ./*.pem
container=$(docker run -d -p 127.0.0.1::27017 -v "$work:/certs:ro" mongo:7 \
    --tlsMode requireTLS --tlsCertificateKeyFile /certs/server.pem --tlsCAFile /certs/ca.pem \
    --tlsAllowConnectionsWithoutCertificates --auth)
shell="mongosh --quiet --tls --tlsCAFile /certs/ca.pem --host localhost"
attempt=0
until docker exec "$container" $shell --eval 'db.runCommand({ping: 1}).ok' 2>/dev/null | grep -q 1; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 120 ]; then docker logs "$container" | tail -20; exit 1; fi
    sleep 0.5
done
docker exec "$container" $shell --eval '
db.getSiblingDB("admin").createUser({user: "root", pwd: "root-pw", roles: ["root"]});
db.getSiblingDB("admin").auth("root", "root-pw");
db.getSiblingDB("$external").runCommand({createUser: "CN=client", roles: [{role: "readWrite", db: "x509_it"}]});' >/dev/null
port=$(docker port "$container" 27017/tcp | head -1 | sed 's/.*://')
export MONGODB_X509_URL="mongodb://127.0.0.1:$port/x509_it?tls=true&tlsCAFile=$work/ca.pem"
export MONGODB_X509_CERT="$work/client.pem"
cd "$root"
cargo test --locked --test integration x509 -- --ignored --test-threads=1
