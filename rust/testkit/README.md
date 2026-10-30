# Tests

Tests talk to modules only through `services_testkit::Host` (the JSON
protocol: `host.call`, `host.discover`, `host.op`, `host.stream`), the same
way the panel does. This crate also provides disposable data directories, the
HTTP and MySQL wire fixtures, and `MODULES`, the catalog conformance iterates
over. Failure messages print the request and reply, with the process
environment redacted.

Adding a module: list it in `src/catalog.rs` (conformance then checks it) and
put its behavior in `rust/native/tests/modules/<name>.rs`.

## Browser scenarios

`cargo run -p xtask -- test browser` installs the built extension into
isolated Chrome profiles and runs independent scenarios (`--list` prints them)
over Native Messaging: Console services, sessions, Templates, History, Splunk
streaming, Agent, page tools, Stop, window closing, modules and updates. Each
scenario starts on a fresh page with reset storage and data files and waits
for conditions instead of fixed pauses. Scenarios run on parallel jobs, each
with its own HOME, Chrome, host and fixtures (`--jobs N`, default half the
CPUs, at most 4); load-sensitive layout, table-performance and page-tools
scenarios run alone afterwards. `test-browser`, `test-agent` and
`test-settings` remain as aliases.

The driver talks to Chrome through the DevTools Protocol from Rust, without
Node.js or Playwright; browser-only JavaScript fixtures run inside Chrome and
the fixture server is Rust. Install Chrome for Testing or Chromium and set
`CHROME_BIN` if it is not in `/Applications`; an existing cached Chromium is
also found, and no browser is downloaded (CI installs a pinned Chrome version
with `browser-actions/setup-chrome`). The clean-install check also verifies
registration of an already-running custom Chrome without a restart. All test
installations, environment files and Variables are disposable.
