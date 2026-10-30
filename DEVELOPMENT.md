# Development

Using the application: [README.md](README.md).

## Build

Requirements: Rust/Cargo on macOS Apple Silicon, plus

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.108 --locked
```

Build the distribution into `dist/distribution/` and install it:

```sh
cargo run -p xtask -- build
./install.sh
```

`build --repository OWNER/REPO --branch BRANCH` sets the update source.
Publishing: [scripts/distribution/README.md](scripts/distribution/README.md).
Extension only: [adapters/extension/README.md](adapters/extension/README.md).

CLI, without Chrome ([flags](adapters/cli/README.md)):

```sh
cargo run -p xtask -- modules   # module packages for the development host
./dev rest --method=GET --endpoint=https://httpbin.org/get
```

## Test

```sh
cargo test --workspace              # everything CI runs
cargo run -p xtask -- test <type>   # one kind at a time
```

| Type | Checks | Where |
|---|---|---|
| `behavior [--module NAME]` | What each module does, through the host protocol; the shared core | `rust/native/tests/modules/`, `tests/core/` |
| `contract` | Rules every module obeys; native messaging, CLI, shell environment, TLS | `rust/native/tests/conformance.rs`, `tests/host/` |
| `ui` | UI models, parsing, completion, rendering | `rust/ui/tests/` |
| `browser [--scenario NAME] [--jobs N] [--list]` | User scenarios in the built extension, isolated Chrome profiles | `rust/xtask/src/browser*` |
| `install` | Install, update, rollback, uninstall in a temporary HOME | `rust/installer/tests` |
| `perf` | Time budgets; release build, ignored by default | `rust/native/tests/perf.rs` |

`browser` needs Chrome for Testing or Chromium (`CHROME_BIN`). Details:
[rust/testkit/README.md](rust/testkit/README.md).

## Layout

- `rust/native` — the host: variables, history, templates, hints, updates,
  the agent, the module registry and loader, native-messaging and CLI adapters.
- `rust/contracts` — module manifest, package format, protocol constants.
- `rust/module-api`, `rust/module-ui` — SDKs of a module's native part
  (`module.dylib`) and console part (`ui.wasm`).
- `rust/modules/<id>` — the six shipped modules.
- `rust/examples/echo` — an unshipped example module and the module guide.
- `rust/ui` — the browser UI (Yew/WebAssembly); `rust/ui/assets` — styles and
  vendored CodeMirror.
- `rust/installer` — install, update, rollback, uninstall.
- `rust/testkit` — test host, module catalog, wire fixtures.
- `rust/xtask` — builds, module packages, test commands.
- `adapters/extension/src` — Chrome manifest, HTML, minimal bootstrap.
- `install.sh`, `dev` — root entry points.

## Rules

- Never inspect, overwrite or delete the real `.variable-profiles.json`,
  `.console-hints.txt` or clipboard contents; use disposable test data.
- Installed Chrome launches the host through Native Messaging; do not require
  a manually started server.
- The host loads the user's login/interactive shell environment in an isolated
  session before it serves requests; failures must stay explicit.
- Keep the extension public key/ID and the native host name stable.
- Variables editors complete only placeholders; Console syntax, saved hints
  and discovery are scoped to their own service.
- Placeholder syntax is `{{env:X}}`, `{{profile:x}}`, `{{:x}}`; shell `${VAR}`
  remains shell syntax.
- Preserve existing worktree changes. Commit only on request and increment the
  extension/workspace patch version by default when committing.
