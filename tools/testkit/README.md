# Tests

Host tests talk to the host only through `services_testkit::Host` (the JSON
protocol: `host.call`, `host.discover`, `host.op`, `host.stream`), the same way the
App does; `Host` wraps the in-process `Runtime`. This crate also provides disposable
data directories, the HTTP and MySQL wire fixtures, module packages for install tests
and `MODULES`, the catalog conformance iterates over. Failure messages print the
request and reply, with the process environment redacted.

Modules are tested in their own crates (`modules/*/<id>/tests/`), calling their
`Module` directly: events in, commands and editor answers out (see
[examples/echo/README.md](../../examples/echo/README.md), "Tests"). A module the host
serves is listed in `src/catalog.rs`, so conformance checks it.

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

Timing checks have no fixed millisecond limits: each compares a median of
several runs with its median on the reference machine, scaled by a fixed
workload timed next to the check, plus 10%. `SERVICES_BROWSER_CPU_THROTTLE=4`
slows every page's CPU fourfold to reproduce slow runners.
