# Valkey for Services DevTools

Valkey/Redis-compatible service module for **Services DevTools 4.1.0+**.
One WASM module, SDK ABI 2, host TCP driver 1.1. No native module library,
external CLI or changes to the application are needed.

## Install

Install **Valkey** in Settings → Modules.

This package replaces the old native Valkey module. Do not install it in
Services DevTools 3.x or 4.0: TCP sessions require 4.1.
The module does not migrate old application's documents or history.

## Commands

```text
#check Get a game
#target
redis://localhost:6380/0
#action
GET game:1
```

One check executes one
command, with shell-style quoting or a JSON array of string arguments:

```text
#action
SET game:1 "The Witcher 3" EX 60
```

```text
#action
["SET", "json:key", "{\"name\":\"Alice\",\"text\":\"line 1\\nline 2\"}"]
```

No shell runs; variables, pipes and substitutions of a shell are not evaluated.
DevTools `{{VARIABLE}}` substitution is supported. Arguments are parsed before
variable resolution, so spaces and quotes in a variable stay in one argument.
Use separate checks for separate commands.

URLs are `redis://[username:password@]host:port/database` or `rediss://…` for TLS.
Port defaults to 6379, database to 0; URL-encode special characters in credentials.
Prefer storing the entire endpoint in `{{VALKEY_URL}}`.

```text
#target
{{VALKEY_URL}}
#options
Timeout: 30
CA-PEM: {{VALKEY_CA}}
#action
PING
```

`Timeout` is 1–110 seconds, default 30. It covers the command or subscription
setup, not an established subscription. `CA-PEM` is optional, requires TLS and
holds the actual PEM certificate text, usually in a dictionary variable with
its line breaks preserved. `CA-Cert` file paths are no longer supported.
System CA trust and hostname verification are enabled; there is no insecure option.

AUTH and SELECT run sequentially on the same connection. If either fails, the
user command is not sent. Each check has a new connection: SELECT, AUTH and
transaction state do not persist across checks.

String replies remain text, including strings containing JSON. Integers,
nulls and arrays display as JSON. Invalid UTF-8 bytes display as
`{"encoding":"base64","data":"…"}`. Server errors fail the check. The module
speaks RESP2; `HELLO` protocol switching is intentionally rejected.

Responses are limited to 16 MiB, 100,000 RESP elements and 64 levels of nesting.
Stop or timeout closes the connection but does
not roll back server changes. Sent commands are never automatically retried.
Cluster, Sentinel, MONITOR and replication streams are not supported.
Commands are not filtered for safety: use care with DEL/FLUSHDB and other writes.

## Pub/Sub

```text
#check Subscribe news
#target
redis://localhost:6380/0
#action
SUBSCRIBE news
```

Publish from another check:

```text
#check Publish news
#target
redis://localhost:6380/0
#action
PUBLISH news "hello"
```

Live JSON lines contain `type: subscribed` and then `type: message`, with channel
and payload. Multiple channels are supported. Stop closes the connection without
a duplicate warning. Channels are shared across databases; messages are not retained
or replayed. A disconnected subscription fails without automatic reconnection.
PSUBSCRIBE and SSUBSCRIBE are not supported. Subscription output is limited to
1 MiB per check; exceeding it fails explicitly rather than silently dropping data.

## Editor

Command names are available immediately, even in an empty #action and offline.
Completion also knows common command options and replaces the whole current
argument, including its quotes. Completion does not offer ordinary stored values.

The application's Hints event triggers read-only discovery in the background:
COMMAND, SCAN, TYPE, and HSCAN when hash fields are needed. The editor uses the
result for key/type and field completion (for example `HGET game:2 `).
GET favors string keys; HGET offers hashes. Keys with spaces are quoted on insertion.
Other commands do not all have full argument grammars.

Discovery caches live only in memory for 30 seconds, separated by resolved endpoint,
credentials, database, CA, prefix and requested hash. The existing Discover action
refreshes the general snapshot. There is **no `#discovery` section**.

Scans return at most 200 names, stop after 32 pages or two seconds of scanning,
and are non-atomic. Discovery has a maximum 10-second operation timeout.
Partial data and ACL denials appear in the module schema under `complete` and
`unavailable`; locally known command completion still works. Server-supported
commands are not necessarily permitted to the current ACL user.
HSCAN receives field/value pairs but discards the values; ordinary key values are
never fetched for discovery. Binary names are represented in the schema but are not
offered as textual completion; TYPE is only fetched for UTF-8 key names.

Highlighting is owned by the module: commands, keys, strings, options and numbers,
not the old generic shell highlighter. It is lexical highlighting, not a complete
server-version-specific grammar or inline semantic validation.

## Build and development

Run from the repository root; the module is a workspace crate and is packaged by
`cargo run -p xtask -- package valkey`. Docker and OpenSSL are needed only for
integration tests.

```sh
cargo test -p services-module-valkey
sh modules/services/valkey/scripts/test-integration.sh
```

The integration script builds the module, loads its actual WASM through ABI 2
in a WASM interpreter, and runs its driver commands against the **actual host
TCP driver** and a fresh isolated Valkey 8 Docker container.
It covers command replies, database/ACL sequencing, TLS trust and hostname checks,
Pub/Sub/Stop, completion discovery and caching. It does not connect to your
localhost:6380 or run FLUSHDB on an existing server. The owned container and
temporary test certificates are removed on exit.
