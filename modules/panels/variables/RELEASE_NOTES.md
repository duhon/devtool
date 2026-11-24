# Variables panel release notes

## 5.0.0

- Variables are a tree: a value is a string or, by indentation, a hash of nested values.
- References are paths from the tree root: `{{.var.x}}`, `{{.env.X}}`, `{{.<set>.x}}`; `{{:x}}`
  looks in Variables, then Dictionary sets in order, then the environment. `*`/`**` select
  children/descendants and a trailing `~` the keys. The old `{{var:x}}` and `{{env:X}}` forms
  are gone: rewrite saved checks by hand.
- Needs Services DevTools 6.0.0.

## 4.0.0

- The Variables panel as a WebAssembly module for Services DevTools 4.0.
