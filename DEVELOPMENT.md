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
Publishing: [tools/scripts/distribution/README.md](tools/scripts/distribution/README.md).
Extension only: [app/extension/README.md](app/extension/README.md).

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
| `behavior [--module ID]` | What each module does (its crate's tests); the host's drivers and history | `modules/*/<id>/tests/`, `host/tests/{drivers,core}/` |
| `contract` | Rules every module obeys; native messaging, packages, shell environment, TLS | `host/tests/conformance.rs`, `host/tests/host/`, `api/protocol` |
| `ui` | Kernel document model, widgets, Shell logic: parsing, completion, rendering | `app/kernel/tests/`, `app/widgets/tests/`, `app/shell/tests/` |
| `browser [--scenario NAME] [--jobs N] [--list]` | User scenarios in the built extension, isolated Chrome profiles | `tools/xtask/src/browser*` |
| `install` | Install, update, rollback, uninstall in a temporary HOME | `installer/tests` |
| `perf` | Time budgets; release build, ignored by default | `host/tests/perf.rs` |

`browser` needs Chrome for Testing or Chromium (`CHROME_BIN`). Details:
[tools/testkit/README.md](tools/testkit/README.md).

## Layout

- `host` — the native host: drivers (`http`, `grpc`, `mongodb`, `mysql`, `process`, `tcp`),
  history, the module registry and installer, sources, updates, the Agent companion,
  the native-messaging adapter. It knows no services.
- `api/protocol` (`services-contracts`) — native-messaging protocol, package format,
  templates and agent contracts shared by the host and the App.
- `api/sdk` (`services-sdk`) — the module SDK: the wasm ABI, `Module`, `Event`/`Cmd`,
  the UI tree, driver requests and shared languages (`lang/`).
- `app/kernel` — the Kernel, no UI: runs wasm modules in the App (`modules`: registry,
  loader, console services), the document model (`document`: checks, responses,
  identities), the editor bridge to modules (`editor`: highlighting, folds, completion,
  templates), the request pipeline (`requests`), the runtime the Shell drives (`runtime`:
  runs with Stop and Repeat, Agent turns, module effects, on a `Platform`), the window's startup (`startup`:
  module list, session database and library, Retry, on a `Boot`), the workspace and its saving
  (`workspace`, `storage`), the Host connection (`host`), Web Locks (`locks`), the
  inspected page (`page`), updates, module events and activity.
- `app/widgets` — generic UI widgets (Yew/WebAssembly) with no application logic: the
  UI-tree view, the text editor, find bar, check rail, status bar, tree, resizer,
  indicator; each widget's CSS lies next to it.
- `app/shell` — the App (`app.wasm`): layout, theme, toolbar, side panels, Console,
  and the wiring of Kernel, widgets and Host (it forwards widget events to the Kernel
  and renders the Kernel's events) (`language.rs` gives the editor its
  language from the Kernel; `link()` links the App's modules into the Kernel — tests
  call it first).
- `app/extension/src` — Chrome manifest, HTML, minimal bootstrap.
- `modules/services/<id>`, `modules/panels/<id>` — the standard modules (services and
  panels), each a wasm crate with its `manifest.json`; services are released on their
  own with their version, `RELEASE_NOTES.md` and Agent skill (`skill/SKILL.md`).
- `examples/echo` — an unshipped example module and the module guide.
- `installer` — install, update, rollback, uninstall.
- `tools/testkit` — test host, module catalog, wire fixtures.
- `tools/xtask` — builds, module packages, test commands.
- `install.sh`, `prerelease` — root entry points.

There are no `assets/` folders: CSS and scripts lie next to the code that owns them
and are embedded with `include_str!`. The one exception is the Shell's
`app/shell/src/theme.css`, which `services.html` links as `theme.css`.

## Rules

- Never inspect, overwrite or delete the user's real application data
  (`~/Library/Application Support/ServicesDevTools/data`) or clipboard contents; use
  disposable test data.
- Installed Chrome launches the host through Native Messaging; do not require
  a manually started server.
- The host loads the user's login/interactive shell environment in an isolated
  session before it serves requests; failures must stay explicit.
- Keep the extension public key/ID and the native host name stable.
- Variables editors complete only placeholders; Console syntax, saved hints
  and discovery are scoped to their own service.
- Placeholder syntax is `{{var:x}}`, `{{env:X}}`, `{{<set>:x}}` (a Dictionary set),
  `{{:x}}`; shell `${VAR}` remains shell syntax.
- Preserve existing worktree changes. Commit only on request and increment the
  extension/workspace patch version by default when committing.
