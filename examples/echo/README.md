# Writing a module

A module adds a **service** (a Console tab: a request language and how to run it) or
a **panel** (a right-side tool such as Variables or History) to Services DevTools.
Echo, in this folder, is the smallest service: it answers each check with its text.
Copy it to start your own.

The standard modules (`modules/services/*`, `modules/panels/*`) are modules of the same
kind: the host and the App make no difference between them and yours.

## What a module is

A module is a Rust crate built for `wasm32-unknown-unknown` with `services-sdk`
(`api/sdk`). The App runs its `module.wasm` in its **Kernel**, in the DevTools panel. A
module never does I/O itself: it asks the Kernel for commands, and the Kernel runs
driver calls (`http`, `grpc`, `mysql`, `process`, `tcp`) on the native host, which
knows no services.

Installed, a module is a **package**, a folder named by the module id with:

| File | Part |
| --- | --- |
| `package.json` | The module manifest (below) with the version, read before any module code runs |
| `module.wasm` | The module |
| `RELEASE_NOTES.md` | Optional: `## X.Y.Z` sections, newest first, shown in Settings → Modules |
| `skills/module-<id>/SKILL.md` | Optional: the module's Agent skill, its request syntax and options |

Every package is in the modules folder,
`~/Library/Application Support/ServicesDevTools/data/modules/<id>/`; the App loads each
`module.wasm` from the host.

## The example

```
examples/echo/
  Cargo.toml      crate-type cdylib (the wasm) + rlib (tests); services-sdk, serde_json
  manifest.json   id, kind, name, version, abi, app and the drivers it uses
  src/lib.rs      impl services_sdk::Module + services_sdk::export!
```

### `manifest.json`

- `id` — a lowercase letter, then lowercase letters, digits, `-` or `_`. Not one
  the host answers itself: `agent`, `agent_models`, `agent_sessions`, `agent_skills`,
  `dictionary`, `hints`, `history`, `modules`, `sessions`, `templates`, `updates`, `vars`.
- `kind` — `service` for a Console tab, `panel` for a side tool.
- `name`, `description` — shown in the tab and in Settings → Modules.
- `abi` — the SDK ABI version (`services_sdk::ABI`, `2`); `app` — the application
  versions it works with (`">=3.7"`).
- `drivers` — the drivers it calls and their versions (`{"http": "1"}`); the Kernel
  refuses a module whose drivers the host lacks.
- Optional: `order`, the tab or tool position and the order of the hooks; `toolbar`, the
  position of a panel's toolbar button when it differs from `order` (Export and Import sit
  at 2).

The version is the crate version; `package` writes it into `package.json`.
Versions are `X.Y.Z`.

## The module: model, events, commands

A module is a model (`impl services_sdk::Module`, `Default`) exported with
`services_sdk::export!(MyModule);`. The Kernel sends it events and gets back commands,
like Elm's `update`:

- `update(event) -> Vec<Cmd>` — the only place the model changes. `Event` brings
  `Init { settings }`, `Ui { msg, value }` (the module's own `Msg` from its UI tree),
  `Reply { tag, result }` and `End { tag }` (answers to its commands), `Run { tag, check }`
  and `Cancel { tag }`, `Document`, `Discover`, `Hints`, `Request`, `Workspace`,
  `Storage`, `Shown`/`Hidden`, `Modules`, `ToolCall`.
- `subscriptions()` — which of the optional events it wants (`run`, `document`,
  `discover`, `hints`, `requests`, `workspace`, `modules`, `tools`).
- `type Msg` — the module's own UI messages (`Serialize + Deserialize`).

Commands are built with `services_sdk::cmd`. Every command that answers has a `tag`;
the answer comes back as `Event::Reply` with the same tag (a stream: several replies,
then `End`). The common ones:

| Command | Does |
| --- | --- |
| `cmd::driver(tag, &DriverCall)` / `cmd::cancel(tag)` | Runs (cancels) a driver call on the host |
| `cmd::resolve(tag, text)`, `run::resolve_value` | Resolves `{{…}}` variables |
| `cmd::output`, `cmd::run_status`, `cmd::result`, `cmd::fail` | Show and finish a run |
| `cmd::kv_get/kv_put/kv_delete/kv_watch` | The module's own storage; `session` keys go with the Console session |
| `cmd::lock`/`unlock`, `cmd::timer` | A lock shared by the App's windows; a one-shot timer |
| `cmd::status`, `cmd::status_with`, `cmd::notify` | The module's status line, Debug and notices |
| `cmd::contribute` | Variables, templates or hints the module adds to every editor |
| `cmd::history_insert`, `cmd::sources_read`, `cmd::env`, `cmd::clipboard_write`, `cmd::document_read/insert` | Shared stores and the Console document |

The Kernel answers a command it does not know with an error; a few (modules,
sources, updates, Agent, page) are for the built-in panels only.

### Synchronous calls

Some blocks are synchronous calls instead of events. They read the model and must not
change it, and should stay within a few milliseconds:

- editor (both kinds): `highlight(state, lines, columns)` (tokens per line, with a
  state carried between lines), `complete(check, pos)`, `fold(check)`;
- service: `build(check) -> DriverCall` and `render(chunk) -> Rendered` for a service
  whose check is one driver call (the Kernel runs it), or subscribe `run` and handle
  `Event::Run`/`Cancel` yourself (several calls, streams, pacing); `export`/`import`
  turn a check into a command line and back;
- panel: `before_request(call)` may change or reject any driver call (panels run in
  `order`), `after_response(call, response)` sees each finished run (History saves it),
  `resolve(text, json, last)` resolves the `{{…}}` variables the panel owns.

A `Check` is the check's text split into `#section`s (`check.text_of("target")`);
`services_sdk::lang` has the shared request languages (headers, JSON, SQL, shell, REST
endpoints) to highlight and complete them.

### UI tree

`view() -> Option<ui::Node>` is the module's UI while it is shown: a panel's body, a
service's Options form. It is data, not HTML or CSS: `ui::panel`, `row`, `column`,
`tabs`, `list`, `table`, `text`, `markdown`, `tree`, `button`, `input`, `select`,
`checkbox`, `slider`, `indicator`, `viewer` and `editor`. Look helpers: `ui::link`
(inline clickable text), `ui::heading` (bold), `ui::badge` (bordered chip) and
`ui::spacer()` (inside a `row`, pushes the following items to the end). `.variant(..)`
takes space-separated names (`"link danger"`); `TableRow::new(key, cells).variant("active")`
highlights a row. Use `danger` only on a confirm button. A handler holds a `Msg`; the
Kernel sends it back as `Event::Ui`. The App draws the tree with its widgets and
theme, so a module looks like the rest of the App.

Shared widget features: any node takes `.title(tip)` and `.label(aria)`;
`ui::tab(..).closable(&msg).confirm(q).plain().title(t)` and
`ui::tabs(..).on_add(&msg).add_prompt(q)` make closable tabs and a `+`;
`TableRow::new(..).detail(nodes)` puts content under a row and `ui::table(..).widths(vec![40, 60])`
fixes column widths; `ui::field(label, control)`, `ui::anchor(label, url)`,
`ui::combobox(options, value, &msg)` (`Choice::new(v, l).detail(d)`), `ui::slider(..).labels(..)
.marker(i, tip)`, `ui::input(..).size(20)`, `ui::editor(..).placeholder(t).debounce(ms)`.
Variants: panel `flush`/`padded`/`editor`; text `warning`, `bold`, `small`, `truncate`,
`paragraph`, `pre`; button `tab`, `inline`; `grow`; row `nowrap`, `end`, `bar` (a toolbar on the table-header background); column `ruled`,
`loose`, `indent`, `inset`, `tight`, `quote`; list `dense`, `capped`; table `roomy`, row
`current`, `muted`. A module's status line can carry Stop:
`cmd::status_stoppable(text, "busy", &Msg::Stop, "my-stop")`; `cmd::workspace_retry(tag)`
reconnects like the App's Retry.

A panel's header row, level with the Console's service tabs, is either its title or its
own tabs, never both:

- `ui::panel(title, body, footer)` shows the title; `.indicator(state)` adds a light to it
  (`idle`, `busy`, `ok`, `warning`, `error`).
- `ui::tabbed_panel(ui::tabs(tabs, active, &Msg::Tab).key("my-tabs"), body, footer)` puts
  the tab strip in the header; the active tab's body comes before `body`.
- `.actions(vec![…])` adds compact items after the title or the tabs (origin text, Reset).
- In the body, a `column` keeps its content's height and the body scrolls; a
  `.variant("primary")` column fills the rest and scrolls by itself (a chat transcript).

```rust
#[derive(Serialize, Deserialize)]
pub enum Msg { Refresh }

impl Module for Counter {
    type Msg = Msg;
    fn update(&mut self, event: Event<Msg>) -> Vec<Cmd> {
        match event {
            Event::Ui { msg: Msg::Refresh, .. } => self.count += 1,
            _ => {}
        }
        Vec::new()
    }
    fn view(&self) -> Option<ui::Node> {
        Some(ui::panel("Counter", vec![ui::text(self.count.to_string())],
            vec![ui::button("Refresh", &Msg::Refresh)]))
    }
}
```

### Drivers

A driver call is `DriverCall::new(driver, &request)` with the request type of its SDK
module: `http::Request`, `grpc::Request` (raw protobuf; `.proto` and reflection stay in
the module), `mysql::Request`, `process::Request` (a program in its own process group)
or `tcp::Request` (the fallback for protocols without a driver). A failed call answers
with a `Failure` (`driver.rs`): where it failed and why. Drivers are minimal and know no
services: anything service-specific (pagination, pacing, parsing) is the module's.

## Tests

Test the module in its crate (`tests/<id>.rs`), natively: create the model, send it
events, check the commands and synchronous answers, and answer its driver calls with
`Event::Reply` as the host would. `services_sdk::Linked` serves a module through the
same wire encoding the Kernel uses, without wasm. The standard modules' tests
(`modules/*/<id>/tests/`) are examples; `cargo run -p xtask -- test behavior --module
<id>` runs one module's.

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
