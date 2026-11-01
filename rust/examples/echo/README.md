# Writing a module

A module adds a console service — one tab — to Services DevTools. Echo, in this
folder, is the smallest one: it answers each check with its text. Copy it to
start your own.

The standard services (`rust/modules/*`) are modules of the same kind: the
host and the panel make no difference between them and yours.

## What a module is

A module is a **package**, a folder named by the module id with:

| File | Part | Runs in |
| --- | --- | --- |
| `package.json` | `{"id": "echo", "version": "0.1.0"}`, read before any module code runs; optional `minCore`, the oldest application version it works with, and `platform` (`macos-arm64`) | — |
| `module.dylib` | Native part: executes requests (`services_module_api`) | the native host, `services-devtools` |
| `ui.wasm` | Console part: turns the sections of a `#check` into request params (`services_module_ui`) | the DevTools panel |
| `RELEASE_NOTES.md` | Optional: `## X.Y.Z` sections, newest first, shown in Settings → Modules | — |
| `skills/module-<id>/SKILL.md` | Optional: the module's Agent skill, its request syntax and options | the Agent |

Without `ui.wasm` the module is listed, but the console cannot send its checks.
A module whose `minCore` is newer than the application is not loaded.

Every package is in the modules folder,
`~/Library/Application Support/ServicesDevTools/data/modules/<id>/`. A host
loads the packages when it starts; the panel loads each console part from the
host.

## The example

```
rust/examples/echo/
  Cargo.toml      the native part: cdylib + rlib; stabby, services-module-api, serde_json
  manifest.json   id, name, params and the console (sections, payload, response view)
  src/lib.rs      impl Service + services_module_api::export!
  ui/Cargo.toml   the console part: cdylib built for wasm32-unknown-unknown; services-module-ui
  ui/src/lib.rs   impl ServiceUi + services_module_ui::export!
```

### `manifest.json`

- `id` — a lowercase letter, then lowercase letters, digits, `-` or `_`. Not one
  the host answers itself: `agent`, `agent_models`, `agent_sessions`, `agent_skills`,
  `dictionary`, `hints`, `history`, `modules`, `sessions`, `templates`, `updates`, `vars`.
- `name`, `description` — shown in the tab and in Settings → Modules.
- `params` — the request params (`name`, `type`, `required`, and `flag`,
  `alias`, `positional` for command lines).
- `console.sections` — the `#marker` sections of a check, each with a unique
  `name`, a `shape` (`line`, `block`, `pairs`), a highlighting `language` and the
  `param` it fills. An `endpoint` section is required. A `connection` section
  must fill a param.
- `console.payload` — the main section; it must be declared.
- `console.response` — how the console shows an answer: `body-headers`
  (`{"status", "headers", "body"}`, like HTTP), `grid` (rows and columns) or
  `stream` (events, like Terminal).
- Optional: `order`, `command` (with `import`/`export`), `discovery`,
  `keywords`, `hints`, `timeout`, `implicitPayload`, `errorBody` — see
  `rust/contracts/src/manifest.rs`.

The version is the crate version: `Manifest::from_json(…, env!("CARGO_PKG_VERSION"))`
reports it, and the host refuses a package whose `package.json` says another
one. Versions are `X.Y.Z`.

### Native part

Implement `services_module_api::sdk::Service`:

- `manifest()` — the manifest above plus what the module can do (`execute`,
  `discover`, `stream`).
- `execute(call)` — `call.params` holds the request params; return the answer
  as JSON or `Error::new("…")`. `call.cancel` fires when the user stops the
  request.
- Optional: `discover` (schema for completion), `stream` (events), `soft_cancel`,
  `status` and `outcome` (how history labels an answer).

`call.host` is the host: `http` and `http_stream` send HTTP requests through
it, `context` and `log` give the request context and logging. With
`Threads::Host` the module's futures run on the host's threads and must do I/O
through the host; `Threads::Own(name)` gives the module its own tokio runtime.

Export the entry the host calls after loading the library:

```rust
pub fn endpoint(host: EndpointArc) -> std::result::Result<EndpointBox, Status> {
    sdk::serve(Echo, host, Threads::Host)
}
services_module_api::export!(endpoint);
```

The crate must be a `cdylib` and depend on `stabby` directly; stabby checks
the ABI of the types the host and the module share when the host loads it.

### Console part

Implement `services_module_ui::ServiceUi`:

- `request(&Sections)` — the request params of a check (`s.text("text")`,
  `s.pairs("headers")`), or an error the console shows.
- Optional: `check` (reject a check before its sections are read), `import` and
  `export` (command lines; only with a manifest `command`).

Then `services_module_ui::export!(Echo);`. The macro exports the functions the
panel calls (`services_ui_abi`, `services_ui_alloc`, `services_ui_call`,
`services_ui_free`) when built for `wasm32-unknown-unknown`.

## Build and package

In this repository:

```sh
cargo run -p xtask -- package rust/examples/echo    # target/packages/echo-0.1.0-macos-arm64.module.tgz
cargo run -p xtask -- package graphql --out /tmp    # a shipped module, by id
```

`package` builds both parts for release and writes one **package file** per
module, `<id>-<version>-<platform>.module.tgz`. It is a gzip'd tar with
`package.json`, `module.dylib`, `ui.wasm` and, when the module has them,
`RELEASE_NOTES.md` and `skills/module-<id>/` at its root and nothing else
(regular files, at most 512 MiB unpacked). In a module's folder the skill is
`skill/SKILL.md` and the version and `minCore` are in `Cargo.toml`
(`[package.metadata.services] min-core`); `package` names the skill and checks
that the notes start with the version. The panel sends a chosen file to the host in one
message, so it must stay under about 48 MiB.

Outside this repository, depend on `services-module-api` and
`services-module-ui` from this repository (a git dependency), then:

```sh
cargo build --release --lib -p my-module
cargo build --release --lib -p my-module-ui --target wasm32-unknown-unknown
mkdir pkg && cp target/release/libmy_module.dylib pkg/module.dylib \
  && cp target/wasm32-unknown-unknown/release/my_module_ui.wasm pkg/ui.wasm \
  && echo '{"id":"my-module","version":"0.1.0","platform":"macos-arm64"}' > pkg/package.json
tar -czf my-module-0.1.0-macos-arm64.module.tgz -C pkg package.json module.dylib ui.wasm
```

## Install and remove

Put the package file in `modules/` of a folder, add the folder in **Settings →
Sources**, then **Install** its row in **Settings → Modules**. The
host unpacks it next to the modules folder, signs the library ad hoc and
clears its quarantine flag (Chrome marks every file its native host writes),
loads it once to check the id, the version and the ABI, then puts it in place
of `<modules>/<id>`, whatever version and source the module had.
The panel then restarts its host: requests it was running fail with “The
native host was restarted”; other open panels see the module after they
reconnect.

**Remove** in the same row asks first, then deletes the folder. By default the
consoles, checks, history, templates and hints of a removed module are kept and
come back when a module with the same id is installed again; check “Delete them
too, in every session” to delete them with the module. A removed standard module
can be installed again from Settings → Modules.

To share a module, put its package file in a source: `modules/` of a folder, or
a GitHub release tagged `module-<id>-v<version>` with the package file as an
asset. Settings → Modules then has a row for the module and the source, with
the newest version and its release notes, for everyone who added the source;
installing it replaces a module with the same id, whatever its source.

A package the host cannot load stays in the list with its error.

For development, `cargo run -p xtask -- modules` links the debug builds of the
standard modules into the development data folder.

A module runs in the host process with your permissions: install only modules
you trust.
