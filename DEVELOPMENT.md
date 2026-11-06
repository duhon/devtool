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

The installer installs the application only; `build` also writes the module
packages to `dist/modules/`. Add `dist/` as a folder source in Settings →
Sources and install them from Settings → Modules.

`build --repository OWNER/REPO --branch BRANCH` sets the update source.
Publishing: [scripts/distribution/README.md](scripts/distribution/README.md).
Extension only: [adapters/extension/README.md](adapters/extension/README.md).

### Pre-release

`./prerelease` builds the working tree (`build --prerelease`, into
`dist/prerelease/`) and installs it beside the release, for a smoke test before
publishing. It is a separate extension ("Services (DevTools) Pre-release", panel
"Services Pre-release") with its own native host, folder
`~/Library/Application Support/ServicesDevTools-Prerelease` and data, and no
in-app updates; the installed release is not touched.

- First time: `./prerelease`, then Load unpacked its `extension` folder (the
  installer prints the path).
- Update: `./prerelease`, then **Reload** the pre-release in
  `chrome://extensions` and reopen DevTools.
- Modules: add `dist/` as a folder source in the pre-release's Settings →
  Sources.
- Remove: `~/Library/Application\ Support/ServicesDevTools-Prerelease/uninstall`,
  then remove the extension in Chrome.

## Test

```sh
cargo test --workspace              # everything CI runs
cargo run -p xtask -- test <type>   # one kind at a time
```

| Type | Checks | Where |
|---|---|---|
| `behavior [--module NAME]` | What each module does, through the host protocol; the shared core | `rust/native/tests/modules/`, `tests/core/` |
| `contract` | Rules every module obeys; native messaging, shell environment, TLS | `rust/native/tests/conformance.rs`, `tests/host/` |
| `ui` | UI models, parsing, completion, rendering | `rust/ui/tests/` |
| `browser [--scenario NAME] [--jobs N] [--list]` | User scenarios in the built extension, isolated Chrome profiles | `rust/xtask/src/browser*` |
| `install` | Install, update, rollback, uninstall in a temporary HOME | `rust/installer/tests` |
| `perf` | Time budgets; release build, ignored by default | `rust/native/tests/perf.rs` |

`browser` needs Chrome for Testing or Chromium (`CHROME_BIN`). Details:
[rust/testkit/README.md](rust/testkit/README.md).

## Layout

- `rust/native` — the host: variables, history, templates, hints, updates,
  the agent, the module registry and loader, native-messaging adapter.
- `rust/contracts` — module manifest, package format, protocol constants.
- `rust/module-api`, `rust/module-ui` — SDKs of a module's native part
  (`module.dylib`) and console part (`ui.wasm`).
- `rust/modules/<id>` — the standard modules, each released on its own with its
  version, `RELEASE_NOTES.md` and Agent skill (`skill/SKILL.md`).
- `rust/examples/echo` — an unshipped example module and the module guide.
- `rust/ui` — the browser UI (Yew/WebAssembly), including its text editor;
  `rust/ui/assets` — styles.
- `rust/installer` — install, update, rollback, uninstall.
- `rust/testkit` — test host, module catalog, wire fixtures.
- `rust/xtask` — builds, module packages, test commands.
- `adapters/extension/src` — Chrome manifest, HTML, minimal bootstrap.
- `install.sh`, `prerelease`, `dev` — root entry points.

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
