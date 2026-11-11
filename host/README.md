# Native host

`services-native` (binary `services-devtools`) is the native host of Services DevTools.
It knows no services: the modules run in the App's Kernel, and the host gives them
the drivers they call (`http`, `grpc`, `mongodb`, `mysql`, `process`, `tcp`) plus the shared
stores: execution history, the modules folder and its installer, sources and updates,
the environment and the Agent companion. It needs neither Node.js, `grpcurl`, the
MySQL CLI, nor `protoc`.

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
topics: overview, interface, agent, page, discovery, services and variables.
Their source is `agent-skills/services-console/SKILL.md` and its `references/`
directory; they are compiled into the companion, not read from the user's
filesystem. Update these references when changing UI controls or service
workflows, and keep each topic under the size checked by the agent tests: a
larger tool reply reaches Copilot only as an unreadable file path.
Chat history is
managed by the SDK and resumed after companion restarts; application variables
and Console documents remain owned by their existing services/browser.
Keep `.agent/` private and treat its persisted conversations as application data.

```text
services-devtools                    Chrome native messaging on stdin/stdout
services-devtools native-host        Explicit native-messaging mode
services-devtools --version
```

`SERVICES_DATA_DIR` selects the application-data directory (the modules folder,
`.console-history.sqlite3`, sources, `.agent/`). It is required; installed launchers
set the persistent application-data directory.
Source builds discover `app/extension/dist` by walking the executable's
and current working directory's ancestors, without embedding a developer's
filesystem path.

Native replies enforce Chrome's 1 MiB limit on each serialized JSON message
(excluding its four-byte length prefix). The UI negotiates `chunkedResponses`
in its health request; larger replies use ordered 128 KiB UTF-8 fragments and
are reassembled per request, without truncating rows or changing SQL.
The existing 64 MiB total response bound remains an explicit per-request error.
Older clients receive an update-required size error instead of losing the
shared connection. Browser-to-host frames still permit 64 MiB.

Terminal Stop terminates the owned process group and also interrupts pending
stdout/stderr capture after the direct child has exited. Normal completion drains
buffered output for up to two seconds; inherited pipes held open by detached
processes cannot keep a check running indefinitely. Incomplete capture returns an
explicit warning. Detached resources outside the owned group, such as independent
SSH tunnels, must be closed through their own cleanup command.

On Unix, native-messaging startup loads the user's login and interactive shell
environment once per connection, before constructing drivers. The shell
comes from `SHELL`, or the user's account when unset; zsh, bash, sh, dash, ksh and
fish are supported. For zsh, both `.zprofile` and `.zshrc` exports become visible
in the read-only Environment (`env`), variable substitutions and child commands,
including `PATH`.
Exports made only in an already-open terminal cannot be recovered this way.

Shell startup runs in a separate session without a controlling terminal, has no
terminal input and must finish within five seconds. Output
is captured separately from Native Messaging, bounded, and never logged; failure
is reported through the native protocol without silently using Chrome's
environment instead. Shell configuration is executed, not parsed as a dotenv
file. The installation-owned `SERVICES_DATA_DIR` setting is
preserved. Retrying the connection or reopening the panel reloads shell settings.

## Architecture and public library

- `runtime::Runtime::{new,with_data_dir,dispatch}` composes the module registry,
  history store, sources, drivers and Agent, and answers the App's requests by
  `service`: `drivers`, `history`, `modules`, `sources`, `updates`, `env`,
  `agent_models`, `agent_sessions` (and `health`).
- `drivers::Drivers::{call,stream}` run the minimal, service-agnostic drivers
  (`drivers/{http,grpc,mongodb,mysql,process,tcp}.rs`); `drivers::versions()` is what `health`
  reports and module manifests are checked against (`major.minor`: a module needing
  tcp `1.1` refuses a host with `1`). A tcp session (`session: true`, a stream) keeps one
  connection open: the App sends `{"id","type":"input","dataBase64"}` for its stream id,
  `adapters::native` queues it (bounded) to that stream, and the driver writes it and
  answers `written`. gRPC uses in-memory `protox`,
  `prost-reflect` and `tonic`; all four RPC shapes, reflection v1/v1alpha and standard
  protobuf imports are supported. MongoDB uses the official `mongodb` crate (rustls,
  hickory DNS for `mongodb+srv://`, zlib compression); documents are canonical Extended JSON.
- `core::registry::Registry` loads the wasm module packages of the modules folder and
  keeps those that failed with their error; `core::install` installs and removes them.
- `core::history`, `core::sources`, `core::updates` own the history database, the
  sources (GitHub repositories and folders) and the application updates.
- `adapters::framing::{encode,FrameReader::feed}` supports fragmented and
  coalesced four-byte little-endian UTF-8 JSON frames, dropping malformed JSON
  frames. Messages larger than 64 MiB are rejected.
- `adapters::native::run(Runtime, AsyncRead, AsyncWrite)` concurrently dispatches
  IDs, serializes output, and cancels/reaps owned child process groups on EOF.

Variables are resolved in the App (the Variables and Dictionary panel modules)
before a driver call; the host gets the finished request.

## Console execution history

The History panel module saves each finished run with its `history.insert` command,
which the App sends as `{service:"history",op:"insert",checkId,sessionId,title,module,
started,ended,status,text}`: both identities are UUID strings, `status` is `success`,
`error` or `stopped`, `text` the resolved check with its response; `defer:true` keeps a
repeat's earlier runs back until `commit`. Viewing history never resolves variables
again. Storage errors never retry or fail the actual execution.

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

The History module masks, before it saves, explicit sensitive-name fields/references (password, token,
authorization, cookie, API/private keys, secrets and credentials), JSON encoded
parameter objects, URL userinfo and recognized sensitive query/header values.
Sensitive variable values are collected only from request/source references,
including transitive alias dependencies, not from unused variable values.
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

## Validation and compatibility boundaries

`cargo test -p services-native` runs local, bounded fixtures for the drivers
(`tests/drivers/`: actual MySQL wire protocol, dynamic gRPC/reflection and four stream
shapes, process stop/EOF cleanup, TCP, MongoDB against `MONGODB_URL` when set), history (`tests/core/`), the native protocol,
packages, shell environment and TLS (`tests/host/`) and the rules every module obeys
(`tests/conformance.rs`). Fixtures create and remove data under this crate's
`target/test-data`, never the user's real data. `cargo clippy -p services-native --all-targets --
-D warnings` checks all targets.

The Debug envelope is preserved, but events are actual Rust transport/process
observations, not simulated Node `undici` events, JavaScript stacks, or the
unavailable Node TLS socket/certificate inspection APIs. MySQL text is decoded
as UTF-8; unusual server character sets are not emulated. DATE, DATETIME and
TIMESTAMP values are interpreted as host-local wall time and rendered as UTC
JSON, matching mysql2's default, including DST gaps/overlaps and millisecond
truncation. Tests inject explicit timezones or set `TZ` only in isolated host
child processes; they never modify the test runner's global environment. Large integers
retain exact Rust/JSON integer values rather than JavaScript precision loss.
TLS uses rustls (bundled
WebPKI roots for HTTP and native roots for gRPC); local fixture tests do not
certify interoperability with every external
TLS deployment. There are no placeholder drivers.
