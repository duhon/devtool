# OpenSearch module release notes

## 1.0.0

- The OpenSearch module ships with Services DevTools: install and update it in
  Settings → Modules. It works with Services DevTools 3.3.0 or newer and replaces the
  0.1.x module from a folder source, whose checks and hints keep working.
- `#endpoint` is `METHOD URL` like REST, `#body` is JSON or, for `_bulk` and `_msearch`,
  NDJSON. Discovery completes index and alias names and mapped fields in Query DSL and
  document bodies.
- The module brings its Agent skill, `module-opensearch`, with its request syntax and
  examples.
