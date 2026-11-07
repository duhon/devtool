# Writing a module

A module adds a console service — one tab — to Services DevTools. Echo, in this
folder, is the smallest one: it answers each check with its text. Copy it to
start your own.

The standard services (`modules/services/*`) are modules of the same kind: the
host and the panel make no difference between them and yours.

## What a module is

A module is a **package**, a folder named by the module id with:

| File | Part |
| --- | --- |
| `package.json` | The module manifest (below) with the version, read before any module code runs |
| `module.wasm` | The module: highlighting, completion, folds, discovery, export/import, runs and response rendering (`services-sdk`) |
| `RELEASE_NOTES.md` | Optional: `## X.Y.Z` sections, newest first, shown in Settings → Modules |
| `skills/module-<id>/SKILL.md` | Optional: the module's Agent skill, its request syntax and options |

The App runs `module.wasm` in its Kernel, in the DevTools panel. A module never
does I/O itself: it asks the Kernel for driver calls (`http`, `grpc`, `mysql`,
`process`, `tcp`), which the native host executes.

Every package is in the modules folder,
`~/Library/Application Support/ServicesDevTools/data/modules/<id>/`; the panel
loads each `module.wasm` from the host.

## The example

```
examples/echo/
  Cargo.toml      cdylib + rlib; services-sdk, serde_json
  manifest.json   id, kind, name, version, abi, app and the drivers it uses
  src/lib.rs      impl services_sdk::Module + services_sdk::export!
```

### `manifest.json`

- `id` — a lowercase letter, then lowercase letters, digits, `-` or `_`. Not one
  the host answers itself: `agent`, `agent_models`, `agent_sessions`, `agent_skills`,
  `dictionary`, `hints`, `history`, `modules`, `sessions`, `templates`, `updates`, `vars`.
- `kind` — `service` for a console tab.
- `name`, `description` — shown in the tab and in Settings → Modules.
- `abi` — the SDK ABI version (`1`); `app` — the application versions it works with.
- `drivers` — the drivers it calls and their versions (`{"http": "1"}`).
- Optional: `order`, the tab position.

The version is the crate version; `package` writes it into `package.json`.
Versions are `X.Y.Z`.

### The module

Implement `services_sdk::Module`:

- `update(event)` — answers events with commands. `Event::Run { tag, check }`
  runs a check: issue driver calls (`cmd::driver`), then finish with
  `cmd::result` or `cmd::fail` for the same tag. `Event::Cancel` stops it.
- `subscriptions()` — the events it wants (`run`, `hints`, …).
- Optional: `highlight`, `complete`, `fold`, `export`/`import` (command lines),
  and for one-call services `build` (the driver call of a check) with `render`
  (its response text) instead of handling runs.

Then `services_sdk::export!(Echo);`.

## Build and package

In this repository:

```sh
cargo run -p xtask -- package examples/echo    # target/packages/echo-0.1.0.module.tgz
cargo run -p xtask -- package graphql --out target/out    # a shipped module, by id
```

`package` builds the module for `wasm32-unknown-unknown` (release) and writes
one **package file** per module, `<id>-<version>.module.tgz`. It is a gzip'd tar
with `package.json`, `module.wasm` and, when the module has them,
`RELEASE_NOTES.md` and `skills/module-<id>/` at its root and nothing else
(regular files, at most 512 MiB unpacked). In a module's folder the skill is
`skill/SKILL.md` and the version and `minCore` are in `Cargo.toml`
(`[package.metadata.services] min-core`); `package` names the skill, checks that
its frontmatter is valid YAML with `name: module-<id>` and a `description` (put a
value that has `: ` in it in quotes) and that the notes start with the version.
The panel sends a chosen file to the host in one message, so it must stay under
about 48 MiB.

Outside this repository, depend on `services-sdk` from this repository (a git
dependency), then:

```sh
cargo build --release --lib -p my-module --target wasm32-unknown-unknown
mkdir pkg && cp target/wasm32-unknown-unknown/release/my_module.wasm pkg/module.wasm \
  && cp manifest.json pkg/package.json
tar -czf my-module-0.1.0.module.tgz -C pkg package.json module.wasm
```

## Install and remove

Put the package file in `modules/` of a folder, add the folder in **Settings →
Sources**, then **Install** its row in **Settings → Modules**. The
host unpacks it next to the modules folder, clears its quarantine flag (Chrome
marks every file its native host writes), checks its manifest, then puts it in
place of `<modules>/<id>`, whatever version and source the module had.
The panel then restarts its host: requests it was running fail with “The
native host was restarted”; other open panels see the module after they
reconnect.

**Remove** in the same row asks first, then deletes the folder. By default the
consoles, checks, history, templates and hints of a removed module are kept and
come back when a module with the same id is installed again; check “Delete them
too, in every session” to delete them with the module. A removed standard module
can be installed again from Settings → Modules.

To share a module, put its package file in `modules/` of a source: a folder, or
a folder of a GitHub repository's branch (see Extensions in the root README).
Settings → Modules then has a row for the module and the source, with the
newest version and its release notes, for everyone who added the source;
installing it replaces a module with the same id, whatever its source.

A package the host cannot load stays in the list with its error.

A module runs in the panel and calls drivers that the host runs with your
permissions: install only modules you trust.
