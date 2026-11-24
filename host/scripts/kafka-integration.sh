#!/bin/sh
# Runs the kafka driver tests, then the module end-to-end tests, against a throwaway single-node
# KRaft broker in Docker, bound to 127.0.0.1 only. Opt-in: the default `cargo test` skips them
# (KAFKA_TEST_BOOTSTRAP is not set). The broker has a plaintext listener and a SASL_PLAINTEXT one
# (PLAIN, SCRAM-SHA-256, SCRAM-SHA-512) with throwaway credentials made up here.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$root"
name="sdt-kafka-$$"
cleanup() {
    docker rm -f "$name" >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM
free_port() {
    python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'
}
plain=$(free_port)
sasl=$(free_port)
password="pw-$$-$(date +%s)"
jaas_plain="org.apache.kafka.common.security.plain.PlainLoginModule required user_tester=\"$password\";"
jaas_scram="org.apache.kafka.common.security.scram.ScramLoginModule required;"
# The image maps KAFKA_A_B to a.b, `__` to `_` and `___` to `-`.
docker run -d --name "$name" \
    -p "127.0.0.1:$plain:9092" -p "127.0.0.1:$sasl:9094" \
    -e KAFKA_NODE_ID=1 \
    -e KAFKA_PROCESS_ROLES=broker,controller \
    -e KAFKA_CONTROLLER_QUORUM_VOTERS=1@localhost:9093 \
    -e KAFKA_CONTROLLER_LISTENER_NAMES=CONTROLLER \
    -e KAFKA_INTER_BROKER_LISTENER_NAME=INTERNAL \
    -e KAFKA_LISTENERS=EXTERNAL://:9092,CONTROLLER://:9093,SASL://:9094,INTERNAL://:19092 \
    -e "KAFKA_ADVERTISED_LISTENERS=EXTERNAL://127.0.0.1:$plain,SASL://127.0.0.1:$sasl,INTERNAL://localhost:19092" \
    -e KAFKA_LISTENER_SECURITY_PROTOCOL_MAP=CONTROLLER:PLAINTEXT,EXTERNAL:PLAINTEXT,INTERNAL:PLAINTEXT,SASL:SASL_PLAINTEXT \
    -e KAFKA_LISTENER_NAME_SASL_SASL_ENABLED_MECHANISMS=PLAIN,SCRAM-SHA-256,SCRAM-SHA-512 \
    -e "KAFKA_LISTENER_NAME_SASL_PLAIN_SASL_JAAS_CONFIG=$jaas_plain" \
    -e "KAFKA_LISTENER_NAME_SASL_SCRAM___SHA___256_SASL_JAAS_CONFIG=$jaas_scram" \
    -e "KAFKA_LISTENER_NAME_SASL_SCRAM___SHA___512_SASL_JAAS_CONFIG=$jaas_scram" \
    -e KAFKA_AUTO_CREATE_TOPICS_ENABLE=false \
    -e KAFKA_OFFSETS_TOPIC_REPLICATION_FACTOR=1 \
    -e KAFKA_TRANSACTION_STATE_LOG_REPLICATION_FACTOR=1 \
    -e KAFKA_TRANSACTION_STATE_LOG_MIN_ISR=1 \
    -e KAFKA_GROUP_INITIAL_REBALANCE_DELAY_MS=0 \
    "${KAFKA_IMAGE:-apache/kafka:3.9.1}" >/dev/null
bin=/opt/kafka/bin
inside() { docker exec "$name" "$@"; }
attempt=0
until inside "$bin/kafka-topics.sh" --bootstrap-server localhost:19092 --list >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 120 ]; then docker logs "$name" | tail -40; exit 1; fi
    sleep 1
done

topic() {
    inside "$bin/kafka-topics.sh" --bootstrap-server localhost:19092 --create --topic "$1" \
        --partitions "$2" --replication-factor 1 >/dev/null
}
topic alpha 3
topic beta 1
topic gamma 1
topic delta 2
printf 'one\ntwo\nthree\nfour\nfive\n' | docker exec -i "$name" "$bin/kafka-console-producer.sh" \
    --bootstrap-server localhost:19092 --topic beta >/dev/null
inside "$bin/kafka-consumer-groups.sh" --bootstrap-server localhost:19092 --group g1 \
    --topic beta --reset-offsets --to-offset 3 --execute >/dev/null
for mechanism in SCRAM-SHA-256 SCRAM-SHA-512; do
    inside "$bin/kafka-configs.sh" --bootstrap-server localhost:19092 --alter --entity-type users \
        --entity-name tester --add-config "$mechanism=[password=$password]" >/dev/null
done

export KAFKA_TEST_BOOTSTRAP="127.0.0.1:$plain"
export KAFKA_TEST_SASL_BOOTSTRAP="127.0.0.1:$sasl"
export KAFKA_TEST_SASL_USERNAME=tester
export KAFKA_TEST_SASL_PASSWORD="$password"
cargo test --locked -p services-native --test drivers kafka_broker -- --test-threads=1 --nocapture
cargo test --locked -p services-module-kafka --test integration -- --test-threads=1 --nocapture
