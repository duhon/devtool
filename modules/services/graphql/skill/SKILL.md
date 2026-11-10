---
name: module-graphql
description: "GraphQL checks in the Services DevTools Console: #target, #headers, #payload and #variables. Load before writing, editing or explaining GraphQL requests."
---

# GraphQL service

Use `#target` with the GraphQL URL, optional `#headers`, `#payload` with the
operation and optional `#variables` containing a JSON object. Keep fragments,
operation definitions and variable references intact. Schema is obtained through
introspection; ordinary query execution is separate.
