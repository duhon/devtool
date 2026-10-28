# Services DevTools: development handoff

Chrome DevTools/standalone extension for MySQL, REST, GraphQL, gRPC, Splunk,
Terminal, Active Console and Variables. The application, build and test driver
are Rust; Node.js/npm and the former JavaScript application have been removed.
Start with [README.md](README.md) for installation and publishing.

## Source boundaries

- `rust/contracts`: service metadata and Native Messaging protocol constants.
- `rust/native`: registry, parameter resolution, persistent profiles/hints,
  six service clients and native/CLI/HTTP/WebSocket adapters.
- `rust/ui`: Yew/WASM state, service views, editors, Console and Variables.
- `rust/ui/assets`: shared CSS and vendored CodeMirror (browser-only JavaScript).
- `rust/installer`: installation, registration discovery/repair and uninstall.
- `rust/xtask`: builds and Rust-driven real-Chrome acceptance. Its JavaScript
  fixtures execute inside the test browser, never in a server runtime.
- `adapters/extension/src`: manifest, Chrome panel entry and WASM loader.
- `dev`, `install.sh`, `scripts/distribution/install.sh`: shell entry points.

## Commands

```sh
cargo test --workspace
cargo run -p xtask -- build-extension
cargo run -p xtask -- build
cargo test -p services-installer --test packaged -- --ignored
cargo run -p xtask -- test-browser
./dev rest --method=GET --endpoint=https://httpbin.org/get
cargo run -p services-native -- web
```

Browser acceptance requires Chrome for Testing or Chromium; use `CHROME_BIN`
for a nonstandard executable. See the main README for WASM prerequisites.

## Invariants

- Never inspect, overwrite or delete real `.variable-profiles.json`,
  `.console-hints.txt` or clipboard contents. Use disposable test data.
- Installed Chrome launches the companion automatically through Native
  Messaging; do not require a manually started server.
- The companion loads the user's login/interactive shell environment in an
  isolated session before initializing clients. Failures must stay explicit.
- Keep the extension public key/ID and host name stable.
- Variables editors complete only placeholders; Console syntax, saved hints
  and discovery are scoped to their own service.
- Placeholder syntax is `{{env:X}}`, `{{profile:x}}`, `{{:x}}`; shell
  `${VAR}` remains shell syntax.
- Preserve existing worktree changes. Commit only on request and increment
  the extension/workspace patch version by default when committing.
