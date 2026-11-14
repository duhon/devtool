## 2.0.0

- `Timeout` moved to the console (global `#options` key); the module no longer parses it.
- `CA-PEM` is now `CA-Certificate` and moved from `#options` to the new `#metadata` section. `#options` keeps only the console keys.

## 1.0.0

- Rewrite the module for Services DevTools 4.1: one portable WASM module using
  services-sdk ABI 2 and the generic TCP 1.1 session driver. No native module or
  external valkey-cli is needed.
- Authenticate and select the database before sending the user's command.
- Preserve standalone commands, ACL/password authentication, verified TLS,
  binary replies, and live SUBSCRIBE/PUBLISH with Stop.
- Add module-owned command highlighting and contextual completion. Hints fetch
  bounded command/key/type metadata and requested hash fields, cached for
  30 seconds; no discovery console section is added.
- Use the #target and #action sections.
- Replace CA-Cert file paths with CA-PEM text, including dictionary variables.
- RESP2 is used; HELLO protocol switching, other subscription modes, Cluster
  and Sentinel are not supported. Responses are limited to 16 MiB and live
  subscription history to 1 MiB; exceeding a limit fails explicitly.
