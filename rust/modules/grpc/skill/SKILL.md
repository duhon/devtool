---
name: module-grpc
description: "gRPC checks in the Services DevTools Console: #endpoint, metadata #headers, #proto and the #message JSON. Load before writing, editing or explaining gRPC requests."
---

# gRPC service

Use `#endpoint` with the selected transport/server/service/method address,
optional `#headers` for metadata, optional `#proto`, and `#message` with JSON.
Inspect the existing endpoint and schema rather than guessing a method name.
The native client supports unary and streaming RPC shapes, reflection and
in-memory proto compilation. A missing reflection response is not proof that
the server has no callable methods.
