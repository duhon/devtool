#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
cargo build --locked --release --lib --target wasm32-unknown-unknown
mkdir -p "$root/../../../target/test-tls"
tls=$(mktemp -d "$root/../../../target/test-tls/run.XXXXXX")
container=""
cleanup() {
    if [ -n "$container" ]; then docker rm -f "$container" >/dev/null; fi
    rm -f "$tls/ca.key" "$tls/ca.pem" "$tls/server.key" "$tls/server.csr" \
        "$tls/server.pem" "$tls/ca.srl" "$tls/extensions.cnf"
    rmdir "$tls"
}
trap cleanup EXIT HUP INT TERM
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj '/CN=Valkey integration CA' \
    -keyout "$tls/ca.key" -out "$tls/ca.pem" >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -subj '/CN=localhost' \
    -keyout "$tls/server.key" -out "$tls/server.csr" >/dev/null 2>&1
printf 'subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\n' > "$tls/extensions.cnf"
openssl x509 -req -in "$tls/server.csr" -CA "$tls/ca.pem" -CAkey "$tls/ca.key" \
    -CAcreateserial -days 1 -extfile "$tls/extensions.cnf" -out "$tls/server.pem" >/dev/null 2>&1
chmod 755 "$tls"
chmod 644 "$tls/server.key"
container=$(docker run -d -p 127.0.0.1::6379 -p 127.0.0.1::6380 \
    -v "$tls:/tls:ro" valkey/valkey:8-alpine valkey-server \
    --save '' --appendonly no --port 6379 --tls-port 6380 \
    --tls-cert-file /tls/server.pem --tls-key-file /tls/server.key \
    --tls-ca-cert-file /tls/ca.pem --tls-auth-clients no \
    --user default off --user tester on '>test-password' '~*' '&*' '+@all')
attempt=0
until docker exec "$container" valkey-cli --user tester -a test-password PING 2>/dev/null | grep -q PONG; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 60 ]; then docker logs "$container"; exit 1; fi
    sleep 0.25
done
port=$(docker port "$container" 6379/tcp | sed 's/.*://')
tls_port=$(docker port "$container" 6380/tcp | sed 's/.*://')
export VALKEY_TEST_URL="redis://tester:test-password@127.0.0.1:$port/0"
export VALKEY_TEST_TLS_URL="rediss://tester:test-password@localhost:$tls_port/0"
VALKEY_TEST_CA_PEM=$(cat "$tls/ca.pem")
export VALKEY_TEST_CA_PEM
cargo test --locked --test integration -- --ignored --test-threads=1
