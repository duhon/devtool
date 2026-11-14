# OpenSearch module release notes

## 2.1.0

- `#headers` is `#metadata`; `#headers` still works but is not offered.

## 2.0.0

- A WebAssembly module for Services DevTools 4.0; it needs that version or newer.
- `#endpoint` is `#target` and `#body` is `#payload`. Saved checks, templates and
  hints are not converted: rename their sections by hand.
- `#target` holds only the URL. The HTTP method goes in `#action` and is GET
  without it; a method left in `#target` is an error that points to `#action`.
- The Agent reads the schema the module discovered in full, not a short summary.

## 1.0.0

- The OpenSearch module ships with Services DevTools: install and update it in
  Settings → Modules. It works with Services DevTools 3.3.0 or newer and replaces the
  0.1.x module from a folder source, whose checks and hints keep working.
- `#endpoint` is `METHOD URL` like REST, `#body` is JSON or, for `_bulk` and `_msearch`,
  NDJSON. Discovery completes index and alias names and mapped fields in Query DSL and
  document bodies.
- The module brings its Agent skill, `module-opensearch`, with its request syntax and
  examples.
