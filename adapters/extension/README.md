# Services — Rust/WebAssembly DevTools extension

The extension uses Native Messaging to communicate with the Rust executable.
It needs neither a Node.js runtime nor a local HTTP server. See the
[main README](../../README.md) for the macOS installer and publishing.

## Build from source

With Rust/Cargo installed:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.108 --locked
cargo run -p xtask -- build-extension
```

This assembles `adapters/extension/dist/` from the Rust UI, generated WASM
glue, Chrome entry points and vendored editor assets. The public key in
`src/manifest.json` preserves the extension ID without a private key.

Build and install the matching Rust native host:

```sh
cargo run -p xtask -- build
./install.sh
```

Load the installed `~/Library/Application Support/ServicesDevTools/extension`
folder through `chrome://extensions` → Developer mode → Load unpacked.
Alternatively, load the source build's `adapters/extension/dist/` when
iterating on the UI; both builds use the same ID and installed host.

Open DevTools → Services, or:

```text
chrome-extension://ckicpgdfkgcmgnclbfbcbkillcpaffnb/panel.html
```

## Runtime boundaries

`rust/ui/` owns application state, service views, Console, Variables and
the browser transport bindings. `src/panel.js` only loads the generated
WASM module; `devtools.js` registers the Chrome panel. Vendored CodeMirror
is an editor dependency, not a parallel JavaScript application.
Styles and editor dependencies live in `rust/ui/assets/`; no legacy web
application or Node build script is used.

`rust/native/` owns request execution. Native Messaging retains the
length-prefixed JSON protocol and service names so transport and service
layers stay independent. The installation preserves the registered host name
`com.chrome_extentions.host` and persistent Variables file.

After a host update, reload the extension and reopen panels. An installed
host and a source-checkout host registration share the same name, so only
one registration can be active at a time.
