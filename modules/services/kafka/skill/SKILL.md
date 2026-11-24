---
name: module-kafka
description: "Kafka checks in Services DevTools: BROKERS, TOPICS, DESCRIBE, OFFSETS, READ, TAIL, PRODUCE, GROUPS, GROUP, LAG, CONFIG and the admin writes CREATE, DELETE, ALTER, PURGE, RESET, COMMIT. Load before writing Kafka checks. Runs on the host's native kafka 1.1 driver."
---

# Kafka service

Use `#target` for comma-separated bootstrap `host:port` addresses (a `kafka://` prefix is
accepted and ignored), not a URL with credentials or a path.
Credentials belong in variable references in `#options`, never in the target.

`#action` takes one command:

- `BROKERS`, `TOPICS`, `DESCRIBE /topic`, `GROUPS`, `GROUP /group`, `CONFIG /topic`.
- `OFFSETS /topic [PARTITION n | PARTITIONS n,m] [AT BEGINNING | END | TIMESTAMP value]`.
- `READ /topic [PARTITION n | PARTITIONS n,m] [FROM BEGINNING | END | LAST n | OFFSET n | TIMESTAMP value] [WHERE KEY = 'text' | WHERE VALUE CONTAINS 'text'] [LIMIT n]`.
- `TAIL` takes the same clauses as READ. It defaults to FROM END; READ defaults
  to FROM LAST 20. READ defaults to LIMIT 20, other lists and TAIL to LIMIT 100.
- `PRODUCE /topic [PARTITION n]` sends exactly one record from `#payload`.
- `LAG /group` reads committed offsets and log ends; it does not join the group.

Admin writes change the cluster immediately, without confirmation:

- `CREATE /topic [PARTITIONS n] [REPLICAS r]`; topic configs as `key=value`
  lines in `#payload`. Omitted counts use the broker defaults.
- `DELETE /topic`; `DELETE GROUP /group` (the group must have no members).
- `ALTER /topic PARTITIONS n` raises the partition total (never lowers it).
- `ALTER CONFIG /topic key=value key2=` sets keys; `key=` resets one to its
  default. Values may hold commas (`cleanup.policy=compact,delete`).
- `PURGE /topic [PARTITION(S) …] [BEFORE OFFSET n]` deletes records below n,
  default the log end (everything).
- `RESET /group /topic TO BEGINNING | END | OFFSET n | TIMESTAMP value [PARTITION(S) …]`
  moves committed offsets; `COMMIT /group /topic [PARTITION(S) …] OFFSET n`
  sets them. Both need an empty group (stop its consumers first).

Writes report one row per resource or partition: status created, deleted,
altered, purged, committed or failed. On a timeout the change may still be
applied; inspect before repeating. Discovery completes topics and consumer groups.

Quote arguments with single/double quotes; double the quote to include it.
Timestamp is epoch milliseconds or RFC3339 with a timezone.
LAST is an offset range per partition, not an exact count of existing records.
There is no global order across partitions.

`#metadata` is for PRODUCE only; `#payload` is for PRODUCE and CREATE:

    Key: order-123
    Header: content-type=application/json
    Header: trace=first
    Header: trace=second
    Null-Header: flag

Key-Base64 and Header-Base64 support standard base64 bytes. Headers preserve order
and duplicates. Key and Key-Base64 are mutually exclusive.

`#options`:

- Format: table, vertical, json.
- Isolation: read-committed (default), read-uncommitted.
- Timeout is the Console's: whole seconds, 1 to 3600 (`Timeout: 30`). The
  module does not see it; every Kafka call and TAIL ends after 30 s.
- Max-Bytes: integer bytes, default 1048576, max 8388608. Bounds serialized
  requests, individual replies, retained records and the final rendered result.
- Scan-Limit: max records examined for WHERE, default 1000, max 10000, at least LIMIT.
- Security-Protocol: plaintext (default), ssl, sasl_plaintext, sasl_ssl.
- Sasl-Mechanism: PLAIN, SCRAM-SHA-256, SCRAM-SHA-512, OAUTHBEARER.
- Sasl-Username, Sasl-Password, Sasl-Token: variable references.
- Ca-Pem, Certificate-Pem, Key-Pem: variable references resolving to PEM strings.
  Certificate and key must be provided together. No certificate paths are read
  by this module.
- Payload-Encoding: text (default), json, base64; PRODUCE only.
- Tombstone: true; PRODUCE only, requires a key and no payload.
- Transform: `{{$name(args)}}` (the Console's) reshapes the result with a jq
  function. `.` is, for READ/TAIL, the array of records `{topic, partition,
  offset, timestampMs, timestampType, key, headers: [{name, value}], value}`
  with bytes decoded (JSON, else text, else `{base64}`); for LAG the rows; for
  other lists their items; else the answer's data. An array of objects shows as
  a table, a string as text, anything else as JSON.
- Repeat and Repeat-Until are the Console's options. Never automatically repeat
  PRODUCE after a timeout or unknown delivery outcome.

READ/TAIL have no group membership or offset commits. Snapshot boundaries and
partition membership are fixed per run. Filtering is client-side, bounded by
Scan-Limit, not a broker query. A filtered run can return fewer than LIMIT.
JSON output preserves key/value/header bytes as base64 and offsets as decimal
strings. Table/vertical output decodes UTF-8 and JSON for display.

Stop does not undo queued production or an admin write. Unknown delivery means
the record may have been written. An offset outside retention must fail, not
reset silently.
