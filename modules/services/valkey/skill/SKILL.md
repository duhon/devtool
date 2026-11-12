---
name: module-valkey
description: Run standalone Valkey and Redis-compatible commands and live Pub/Sub in Services DevTools. Use for GET, SET, TTL, SCAN, counters, hashes, lists, sets, sorted sets, SUBSCRIBE and PUBLISH.
---

Use one command per check:

```text
#check Get game
#target
{{VALKEY_URL}}
#action
GET game:1
```

The endpoint is redis://[username:password@]host:6379/database or rediss:// for TLS.
The database defaults to 0. URL-encode credentials and prefer a dictionary variable.

Arguments support shell-style quoting or a JSON array of strings. No shell is run.
DevTools variables are resolved after parsing, preserving one argument per value.
Use separate checks for separate commands; connections are fresh for each check.
Select the database in the URL, not in an earlier SELECT check.

Optional #options: Timeout: 30 (1-110 seconds) and CA-PEM: {{VALKEY_CA}}.
CA-PEM is certificate text, not a file path; it requires rediss://.
Certificate/hostname verification stays enabled. AUTH/SELECT must succeed before
the command is sent. Stop and timeout do not roll back writes; do not retry an
uncertain write automatically.

Strings display unchanged; numbers, nulls and arrays display as JSON.
Binary responses use {"encoding":"base64","data":"..."}.
Prefer SCAN to KEYS and follow its cursor to 0. Ask before destructive operations.

SUBSCRIBE news remains running until Stop. PUBLISH news "hello" in a separate
check sends messages. Multiple subscription channels are allowed. Channels are
shared across databases. Messages are not retained. No automatic reconnect.
PSUBSCRIBE, SSUBSCRIBE, MONITOR, HELLO protocol switching, Cluster and Sentinel
are unsupported. The module uses RESP2. Response limit is 16 MiB; subscription
history limit is 1 MiB and fails explicitly when exceeded.

The editor supplies command, key/type, option and hash-field completion.
Hints/discovery fetch bounded read-only metadata and cache it for 30 seconds.
There is no #discovery section. Discovery schemas may be partial or ACL-restricted;
do not treat them as an exhaustive list or proof of command permission.
