# Kafka for Services DevTools

Kafka service module for **Services DevTools 5.1.0+**: one WASM module, SDK ABI 2,
host `kafka` driver 1 (`krafka`, pure Rust). No Kafka CLI or JVM is needed.

`BROKERS`, `TOPICS`, `DESCRIBE`, `OFFSETS`, `READ`, `TAIL`, `PRODUCE`, `GROUPS`, `GROUP`,
`LAG` and `CONFIG`; the syntax is in [skill/SKILL.md](skill/SKILL.md). Reads use a standalone
consumer with explicit assignment: no group joins, no commits, no topic creation.

`host/scripts/kafka-integration.sh` (opt-in, Docker) runs the host driver and this module
against a throwaway single-node KRaft broker.
