# Import-free WebAssembly patches

Sources are copied from the Cargo registry, never modified there:

- `lrpar` 0.14.2, Apache-2.0 OR MIT.
- `promql-parser` 0.11.1, Apache-2.0.

`lrpar` selects browser `web-time` on wasm32. Its generated parser retains
`Instant::now()` in CPCT+ error recovery, introducing JavaScript imports even
though PromQL's build already selects `RecoveryKind::None`.

The local wasm32 build returns an ordinary parse error immediately instead of
entering recovery. Native builds retain upstream recovery behavior. The browser
clock dependency is removed; remaining time types come from `std` without calls
to the unsupported wasm host clock.

`promql-parser` enables chrono's default clock and browser features transitively.
The local manifest requests only chrono's `std` feature: AST timestamp arithmetic
and formatting work without a current-time API or JavaScript imports.

Both patches are selected through the workspace's `[patch.crates-io]`; `vendor/lrpar`
is the patched `lrpar`. `cargo check` alone cannot prove load compatibility: the built
`module.wasm` must have no imports.
