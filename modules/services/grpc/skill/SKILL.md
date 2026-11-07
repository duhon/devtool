---
name: module-grpc
description: "gRPC checks in the Services DevTools Console: #endpoint, metadata #headers, #proto and the #message JSON. Load before writing, editing or explaining gRPC requests."
---

# gRPC service

Use `#endpoint` with the selected transport/server/service/method address,
optional `#headers` for metadata, optional `#proto`, and `#message` with JSON.
Inspect the existing endpoint and schema rather than guessing a method name.
Calls go through the Host gRPC driver: unary and streaming RPC shapes; a JSON
array in `#message` sends several messages to a client-streaming method. Without
`#proto` the descriptors come from server reflection, loaded while the check is
edited; with `#proto` the source is compiled in memory (`google/protobuf/*`
imports are built in). A non-zero gRPC status prints `{"error": message}`. A
missing reflection response is not proof that the server has no callable methods.
