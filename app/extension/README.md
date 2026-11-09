# Services — Rust/WebAssembly DevTools extension

The extension uses Native Messaging to communicate with the Rust executable.
It needs neither a Node.js runtime nor a local HTTP server. See
[DEVELOPMENT.md](../../DEVELOPMENT.md) for building, installing and publishing.

## Build from source

With Rust/Cargo installed:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.108 --locked
cargo run -p xtask -- build-extension
```

This assembles `app/extension/dist/` from the App (`app/shell`, built to
`app.wasm`), its generated WASM glue, the Shell's `theme.css` and the Chrome entry
points in `src/`. The public key in
`src/manifest.json` preserves the extension ID without a private key.

Build and install the matching Rust native host:

```sh
cargo run -p xtask -- build
./install.sh
```

Load the installed `~/Library/Application Support/ServicesDevTools/extension`
folder through `chrome://extensions` → Developer mode → Load unpacked.
Alternatively, load the source build's `app/extension/dist/` when
iterating on the UI; both builds use the same ID and installed host.

Open DevTools → Services, or:

```text
chrome-extension://ckicpgdfkgcmgnclbfbcbkillcpaffnb/services.html
```

## Runtime boundaries

`app/shell/` owns application state, layout, the Console, side panels hosting and
the browser transport bindings; services and panels are wasm modules its Kernel
(`app/kernel/`) runs. `src/services.js` only loads the generated WASM module;
`devtools.js` registers the Chrome panel. The text editor and the other widgets are
part of the WASM module (`app/widgets/`), with their CSS next to them. The one
stylesheet linked from `services.html` is the Shell's theme (`theme.css`); no legacy web
application or Node build script is used.

`host/` runs the drivers the modules call. Native Messaging carries the
length-prefixed JSON protocol. The installation preserves the registered host name
`com.chrome_extentions.host` and the application data.

After a host update, reload the extension and reopen panels. An installed
host and a source-checkout host registration share the same name, so only
one registration can be active at a time.
