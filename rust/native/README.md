# Native runtime

`services-devtools` is the Rust composition root for all six service clients.
It needs neither Node.js, `grpcurl`, the MySQL CLI, nor `protoc`.

The optional Console Agent uses the official Rust Copilot SDK, pinned to 1.0.26,
and a separately installed GitHub Copilot CLI. Authenticate with `gh auth login`
or provide the host's `COPILOT_GITHUB_TOKEN`. `SERVICES_COPILOT_PATH` can specify
an absolute executable; otherwise the companion searches its `PATH`.
The native host loads the login-shell environment as described below.
No Copilot executable is downloaded or bundled by this application.

Copilot starts lazily in SDK Empty mode with application-owned `.agent/` state,
an isolated working directory, filtered child environment, telemetry disabled
and a custom-tool-only allowlist. Permission requests for other tools are
denied. The bundled `services-console` overview is mandatory in every created
or resumed session. `console_skill` reads the versioned application knowledge
topics: overview, interface, discovery, services and variables. Their source
is `agent-skills/services-console/SKILL.md` and its `references/` directory;
they are compiled into the companion, not read from the user's filesystem.
Update these references when changing UI controls or service workflows.
Chat history is
managed by the SDK and resumed after companion restarts; application profiles
and Console documents remain owned by their existing services/browser.
Keep `.agent/` private and treat its persisted conversations as application data.

```text
services-devtools                    Chrome native messaging on stdin/stdout
services-devtools native-host        Explicit native-messaging mode
services-devtools cli <service> ...  Shared-config CLI flags/aliases/positionals
services-devtools web [--port N]     Loopback HTTP/WebSocket gateway (default 4569)
services-devtools --version
```

`SERVICES_DATA_DIR` selects the directory containing `.variable-profiles.json`,
`.dictionary-profiles.json` and `.console-hints.txt`. Source builds default to the repository root. Installed
launchers must set the persistent application-data directory. `SERVICES_UI_DIR`
selects the compiled browser assets for the web adapter. Installed executables
at `<release>/native/bin/services-devtools` otherwise use `<release>/extension`.
Source builds discover `adapters/extension/dist` by walking the executable's
and current working directory's ancestors, without embedding a developer's
filesystem path. `PORT` sets the default web port.

Native replies enforce Chrome's 1 MiB limit on each serialized JSON message
(excluding its four-byte length prefix). The UI negotiates `chunkedResponses`
in its health request; larger replies use ordered 128 KiB UTF-8 fragments and
are reassembled per request, without truncating rows or changing SQL.
The existing 64 MiB total response bound remains an explicit per-request error.
Older clients receive an update-required size error instead of losing the
shared connection. Browser-to-host frames still permit 64 MiB.

Splunk API calls share a per-server queue keyed by URL origin (scheme, host and
port), independent of the check, session, app or credentials. Private lock/state
files in `.splunk-rate-limits/` coordinate native, HTTP and CLI processes using
the same data directory. Every export request and retry waits
at least 13 seconds after the previous call completes: at most five calls in any
minute. Different servers proceed independently. HTTP 429 extends the shared
cooldown using `Retry-After`, or 60 seconds when it is absent. Automatic redirects
and implicit HTTP retries are disabled for Splunk so they cannot bypass pacing.
Waiting remains cancellable; the overall search timeout is 10 minutes including
queue waits. Headers and each wait for stream data have a 120-second timeout;
an active export may run up to the overall deadline. Calls made by
other applications or installations with different data directories are outside
this queue.

Splunk uses `POST search/v2/jobs/export` with JSON streaming and `preview=false` by default,
matching the official SDK's preference for Splunk 9+. HTTP 404 falls back once
to legacy `search/jobs/export` through the same paced queue, with both attempts
visible in Debug; other errors do not trigger a compatibility retry.
The companion parses records as bytes arrive, including split UTF-8 characters,
instead of buffering the full HTTP body. Count limits rows locally:
omitted/blank = 100, positive N = at most N rows, 0 = the whole result. Offset
defaults to 0 and skips the first N rows locally before Count is applied; it is
not sent as an unsupported export parameter. Each run starts a new search,
so Offset is not stable pagination of a saved job and does not avoid scanning
the skipped rows. It closes
the stream as soon as the portion arrives; no status polls or results pagination
are needed. SPL is not rewritten, so global sort/aggregate commands can still
block early final output. `Preview: true` opts into provisional snapshots:
the first snapshot with rows after Offset is returned when Count is reached or
that snapshot ends. A completed preview may contain fewer than Count rows.
Count: 0 returns all remaining rows of that one preview snapshot, not all future
snapshots. Offset restarts for each snapshot; rows from separate previews or the
final result are never combined. An empty portion is skipped while the search
is still previewing. Preview output is labeled provisional in Console, History
and the service result view, with WARNING rather than a final OK status.
With Preview: false, snapshots remain ignored. `resultCount` is the number of
returned rows, not the total matches; Debug's `export` distinguishes EOF, Count
termination and preview completion. Preview does not guarantee immediate
results if the server/search itself delays producing a snapshot.
Malformed/truncated JSON, stream errors and Splunk ERROR/FATAL messages fail the
request explicitly. Individual records and retained results are bounded by the
existing 64 MiB response limit.

Terminal Stop terminates the owned process group and also interrupts pending
stdout/stderr capture after the direct child has exited. Normal completion drains
buffered output for up to two seconds; inherited pipes held open by detached
processes cannot keep a check running indefinitely. Incomplete capture returns an
explicit warning. Detached resources outside the owned group, such as independent
SSH tunnels, must be closed through their own cleanup command.

On Unix, native-messaging startup loads the user's login and interactive shell
environment once per connection, before constructing service clients. The shell
comes from `SHELL`, or the user's account when unset; zsh, bash, sh, dash, ksh and
fish are supported. For zsh, both `.zprofile` and `.zshrc` exports become visible
in the read-only `env` profile, variable substitutions and child commands,
including `PATH`. CLI and web modes retain their caller's environment unchanged.
Exports made only in an already-open terminal cannot be recovered this way.

Shell startup runs in a separate session without a controlling terminal, has no
terminal input and must finish within five seconds. Output
is captured separately from Native Messaging, bounded, and never logged; failure
is reported through the native protocol without silently using Chrome's
environment instead. Shell configuration is executed, not parsed as a dotenv
file. Installation-owned `SERVICES_DATA_DIR` and `SERVICES_UI_DIR` settings are
preserved. Retrying the connection or reopening the panel reloads shell settings.

## Architecture and public library

- `runtime::Runtime::{new,with_data_dir,dispatch}` composes one registry,
  Variables and Dictionary stores, hints store, HTTP client and MySQL pool cache.
- `core::registry::Registry::{resolve,call,discover,stream}` centralizes service
  lookup and variable substitution using `services-contracts` metadata.
- `core::variables::VariableStore::{new,list,dispatch,resolve_params}` re-reads
  ordered profiles from disk, preserves raw and tolerant resolved views,
  validates cross-profile references, rejects cycles, and reserves read-only `env`.
- `services::{http,graphql,openapi,splunk,grpc,mysql,terminal}` own transports
  and response contracts. gRPC uses in-memory `protox`, `prost-reflect`, and
  `tonic`; all four RPC shapes, reflection v1/v1alpha and standard protobuf
  imports are supported. Application bytes retain Buffer-shaped JSON and oneof
  discriminators; well-known protobuf types use canonical JSON.
- `adapters::framing::{encode,FrameReader::feed}` supports fragmented and
  coalesced four-byte little-endian UTF-8 JSON frames, dropping malformed JSON
  frames. Messages larger than 64 MiB are rejected.
- `adapters::native::run(Runtime, AsyncRead, AsyncWrite)` concurrently dispatches
  IDs, serializes output, and cancels/reaps owned child process groups on EOF.
- `adapters::cli::{service_args,parse_params,usage,run}` uses shared descriptors.
  Terminal outputs raw stdout/stderr and returns its exit code. Ctrl+C cancels
  requests/commands; HTTP 4xx/5xx and non-OK gRPC statuses produce nonzero exits.
- `adapters::web::{router,run}` supplies the development HTTP/WS adapter.

Native health, Variables CRUD, discovery, unary results and Terminal
data/exit/stop frames retain their original envelope shapes. Native hints add
`{id,service:"hints",op:"get"|"save",serviceName,text?}` and return `{text}`.

## HTTP routes

- `POST /api/console`, `POST /api/console/discover`
- `POST /api/console/history` (Runtime history operations, replies unwrapped)
- `POST /api/services/{rest|graphql|grpc}/request`
- `POST /api/services/mysql/query`, `POST /api/services/splunk/search`
- `WS /api/services/terminal/stream` (first message starts; subsequent
  `{type:"stop"}` or disconnect cancels; output is stdout/stderr/exit JSON)
- `WS /api/agent/stream` (requires a matching loopback Host/Origin;
  first message contains workspace/session IDs, service and prompt;
  subsequent `agentToolResult` messages answer correlated workspace calls;
  Stop/disconnect cancels the turn)
- `GET /vars/api/profiles`
- `POST /vars/api/profiles/var` (only the fixed `var` profile is writable)
- `GET /dictionary/api/profiles`
- `POST|DELETE /dictionary/api/profiles/{name}`
- `POST /dictionary/api/profiles/{name}/rename`, `POST /dictionary/api/order`
- `GET|PUT /api/console/hints/{service}`
- `GET /api/services` (shared config), `GET /api/health`
- Static files and SPA fallback from `SERVICES_UI_DIR`

## Console execution history

Console execution envelopes may include top-level
`history:{checkId,sessionId,title,source}`. Both identities are UUID strings;
`source` is the complete original check, without a previous response. Calls
without context, discovery, and direct Registry/CLI calls are not recorded.
The Runtime resolves execution parameters and source against the same captured
variable/profile values, then stores an immutable resolved check, effective
parameters, response/status and terminal stdout/stderr events. Viewing history
never resolves variables again. Native terminal exit frames expose metadata
both at `history` and `info.history`; other successful native replies expose it
at `history` and `result.history`. Errors and HTTP/WS payloads expose `history`.
Metadata is `{saved:true,id}` or `{saved:false,error}`. Storage errors never
retry or fail the actual execution. Unresolvable execution variables cannot
produce a resolved snapshot and return an explicit history failure.

Native Runtime requests:

- `{service:"history",op:"list",checkId,before?,limit?}` returns newest-first
  `{entries,nextBefore,total,bytes}`. `before` is an exclusive positive entry-ID
  cursor; limit defaults to 25 and is capped at 50. Each entry has
  `id,startedAt,endedAt,durationMs,status,title,service`; timestamps are epoch
  milliseconds, status is `success`, `error`, or `stopped`. List titles are
  bounded to 512 Unicode characters; the full check remains in the snapshot.
- `{service:"history",op:"get",id,offset?}` returns
  `{id,text,nextOffset}` with at most 128 KiB of UTF-8 text. Offsets are UTF-8
  **bytes**, and must be valid character boundaries. Missing entries, invalid
  arguments and offsets past the snapshot are explicit errors. Native framing
  reserves `id` for request correlation: send the entry identity as `entryId`
  in that transport; Runtime/HTTP also accept that alias.

HTTP `/api/console` accepts the complete Runtime envelope. Existing service
endpoints accept either the envelope or legacy flat parameters with a top-level
`history`. Terminal WebSocket start messages likewise accept an envelope or flat
parameters plus `history`. `/api/console/history` supports both history operations
and uses the usual successful-result unwrapping.

Snapshots live in `.console-history.sqlite3` under `SERVICES_DATA_DIR`, using
bundled SQLite (no external database), private Unix file permissions, a
10-second busy timeout, immediate write transactions and DELETE journaling.
All database work runs on blocking workers. The global logical serialized
payload quota is 100 MiB; crossing it purges oldest completed snapshots to
approximately 50 MiB in the same transaction, never deleting the new entry.
A single 50–100 MiB snapshot may remain alone. Larger snapshots are explicitly
rejected without truncation. FULL auto-vacuum reclaims freed pages; rollback
journals are removed after transactions. The 100 MiB cap bounds logical retained
payload, not peak disk usage: SQLite page/index overhead also consumes space,
and a DELETE rollback journal may temporarily require significant additional
space during a write or purge. Missing databases are
created normally; corrupt/unavailable databases are reported, never replaced.

History masks explicit sensitive-name fields/references (password, token,
authorization, cookie, API/private keys, secrets and credentials), JSON encoded
parameter objects, URL userinfo and recognized sensitive query/header values.
Sensitive variable values are collected only from request/source references,
including transitive alias dependencies, not from unused profile values.
Ordinary Dictionary/Variables values remain resolved and visible. Response bodies
and terminal output are saved verbatim, without masking or pretty-printing, and
may therefore contain credentials returned by the service. Transport/debug
wrappers and duplicated effective parameters are not included. Existing legacy
snapshots are cleaned transactionally on first access; previously masked response
values cannot be recovered. No unmasked original request-source copy is persisted.
This is conservative request name/value redaction,
not secret discovery: arbitrary application data, encoded/transformed secrets,
unrecognized credential names, shell-specific credential syntax and credentials
in ambient child-process environment cannot be comprehensively recognized.
Treat the database as private application data even with masking.

Variables (`service:"vars"`) owns the fixed writable `var` profile and the
read-only live `env` view. Dictionary (`service:"dictionary"`) owns all other
named profiles, with independent CRUD/order and storage. Each reply exposes its
own profiles and a merged `context` for completions and reference resolution.
Explicit references keep their original `{{profile:NAME}}` syntax and can cross
stores; bare `{{:NAME}}` uses `var`, then Dictionary order, then the environment.
`env` is never persisted.

On first use, legacy profiles other than `var` migrate to the Dictionary file.
Names, values and their relative order are preserved; a missing `var` becomes
empty. The original bytes are retained in `.variable-profiles.json.pre-split.bak`.
A cross-process file lock protects migration and read/modify/write operations;
atomic sibling renames and a version marker allow interrupted migration to resume.
Malformed files or conflicting partially migrated values are reported without
overwriting the originals. After migration, each mutation writes only its own
file. An empty Dictionary is valid; `var` cannot be deleted or renamed.

## Validation and compatibility boundaries

`cargo test -p services-native` runs local, bounded fixtures for all service
transports, actual MySQL wire protocol, dynamic gRPC/reflection and four stream
shapes, Variables persistence/cycles, fragmented/concurrent native messages,
Terminal stop/EOF cleanup, HTTP routes, and binary CLI/framing behavior.
Fixtures create and remove data under this crate's `target/test-data`, never
the user's real profiles. `cargo clippy -p services-native --all-targets --
-D warnings` checks all targets.

The Debug envelope is preserved, but events are actual Rust transport/process
observations, not simulated Node `undici` events, JavaScript stacks, or the
unavailable Node TLS socket/certificate inspection APIs. MySQL text is decoded
as UTF-8; unusual server character sets are not emulated. DATE, DATETIME and
TIMESTAMP values are interpreted as host-local wall time and rendered as UTC
JSON, matching mysql2's default, including DST gaps/overlaps and millisecond
truncation. Tests inject explicit timezones or set `TZ` only in isolated CLI
child processes; they never modify the test runner's global environment. Large integers
retain exact Rust/JSON integer values rather than JavaScript precision loss.
The web adapter intentionally listens only on loopback. TLS uses rustls (bundled
WebPKI roots for HTTP and native roots for gRPC); local fixture tests do not
certify interoperability with every external
TLS deployment. There are no placeholder service clients.
