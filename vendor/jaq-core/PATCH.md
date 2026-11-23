# Patched jaq-core 3.1.2

Copy of [jaq-core 3.1.2](https://crates.io/crates/jaq-core/3.1.2) (MIT, see
`LICENSE-MIT`), used through `[patch.crates-io]` in the root `Cargo.toml`. Upstream
jaq has no way to bound an evaluation; the Functions panel evaluates user jq on the
page's main thread, where an endless loop hangs the page and deep recursion overflows
the stack (and kills the kernel). The patch:

- `src/limit.rs` (new, `pub mod limit` in `src/lib.rs`): a per-thread step budget and
  nesting-depth limit (`limit::set(steps, depth)`, `limit::clear()`,
  `limit::tripped()`). Without `set`, evaluation is unlimited (upstream behaviour).
  Once a limit is reached, every further step fails, so a caught error cannot keep
  the evaluation going.
- `src/filter.rs`:
  - `Id::run` spends a step (and holds one nesting level) for the duration of the
    call, and wraps the output stream of calls (`CallDef`, `Native`, `Var`) in
    `limit::Limited`, which spends a step (and nests a level) per pulled value.
    Streams with at most one value are not wrapped.
  - `Id::paths` / `Id::update` are split into a limited wrapper and the unchanged
    `paths_unlimited` / `update_unlimited`.
  - `recurse_run` (`..`) wraps each level in `limit::Limited`.
- `src/rc_list.rs`: `List` drops its nodes iteratively (upstream drops recursively, so
  a long path list, e.g. from an endless `path(...)`, overflowed the stack); `pop` no
  longer moves out of the `Drop` type.

To update jaq-core, copy the new release here and re-apply these changes.
