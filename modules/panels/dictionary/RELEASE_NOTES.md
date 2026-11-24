# Dictionary panel release notes

## 5.0.0

- Every set is a first-level node of the variable tree: `{{.<set>.NAME}}` (was
  `{{<set>:NAME}}`), and `{{:NAME}}` from the first set, in order, that has it. Values may be
  nested hashes.
- Needs Services DevTools 6.0.0.

## 4.0.0

- The Dictionary panel as a WebAssembly module for Services DevTools 4.0.
- Saves into the Local source; Settings → Sources syncs it with shared sources.
