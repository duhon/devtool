# Prometheus service for Services DevTools

PromQL service module. Queries and discovery use the Host's `http` driver.
Results are copyable Unicode/Braille charts, text tables or JSON.

## Development

PromQL parsing uses `promql-parser` 0.11.1. Patched copies of it and `lrpar` in the
repository's `vendor/` keep the WebAssembly output free of JavaScript imports, as the
module loader requires; see [vendor/promql-parser/PATCH.md](../../../vendor/promql-parser/PATCH.md).
They remove browser clock dependencies, not PromQL grammar features.

```sh
cargo test -p services-module-prometheus
cargo run -p xtask -- package prometheus
```

## Query document

```text
#target
{{env:PROMETHEUS_URL}}

#metadata
Authorization: Bearer {{var:token}}
Query-Mode: range
Query-Start: now-1h
Query-End: now
Query-Step: 30s

#options
Format: table

#payload
sum by (service) (
  rate(http_requests_total{namespace="production"}[5m])
)
```

Use the server's HTTP(S) base URL in `#target`, including its reverse-proxy prefix,
but without `/api/v1/query`. Reserved `Query-*` metadata configures execution and
is not sent as HTTP headers. Other metadata, including authorization and tenant
headers, is sent through the HTTP driver.

Instant mode evaluates an expression at one time:

```text
#target
http://localhost:9090

#metadata
Query-Mode: instant
Query-Time: now

#payload
up
```

Range mode evaluates repeatedly from `Query-Start` to `Query-End` with
`Query-Step`. PromQL's `[5m]` is an expression's range selector, not the query's
evaluation interval. Relative bounds are computed once per run; Repeat computes
them again. Absolute timestamps allow reproducible queries.

`Format: json` shows the API response. Tables show timestamps, labels and values.
An empty result is successful. Server warnings and information annotations remain
visible. Special sample values and native histograms must not be silently lost.

Discovery is on demand, not a scan of all series when opening the service.
Local PromQL parsing is editor assistance; the server remains authoritative for
its supported language and experimental functions.

The default client request timeout is 120 seconds. `Query-Timeout` requests a
server evaluation timeout; it does not extend the client deadline. Range bounds
are inclusive and the step must be positive. Contradictory mode parameters and
unknown `Query-*` parameters are reported rather than silently ignored.

Optional `Columns-Type` and `Columns-Size` in `#options` use the existing table
syntax, for example `Columns-Size: labels=60 value=20`. Long cells wrap rather
than truncate. Discovery suggestions are bounded and cached separately for each
resolved server and header set; use Discover to refresh a cached context.

## Copyable charts

Set `Format: chart` in `#options`. The graph is the actual response text, rendered
with Ratatui's Braille marker into a buffer; no terminal backend, ANSI colors,
SVG or browser canvas is used. Copy the whole check and response and send it in a
monospaced code block without line wrapping.

```text
#options
Format: chart
Chart-Width: 96
Chart-Height: 20
Repeat: 2s
```

Use range mode with moving bounds such as `now-15m` and `now` for a live graph.
Repeat replaces the response with a completed frame, leaving the previous one
visible during the request. This is stable frame replacement, not animation
between sample values. Instant queries show a single point per series; previous
instant results are not combined into an invented time history.

Each label set has a separate graph, in sorted label order. UTC interval, step
and complete labels travel with the copied text. Width defaults to 96 columns
(40-160 allowed); height defaults to 20 rows (8-40 allowed), including axes.
`Chart-Min` and `Chart-Max` together set a fixed finite Y scale. Without them,
each series' scale expands but does not shrink across repeats of the same check.
Changing the query/connection/options, stopping, or restoring a workspace resets
this in-memory scale. Scale state is bounded to 32 active/recent checks.

Non-finite values and missing evaluation steps break lines. Explicit Y bounds
clamp out-of-scale points to the graph edge and report the clipped count.
High-density samples are reduced only for drawing, retaining first, minimum,
maximum and last points per horizontal dot column. Table/JSON data is unchanged.
Native histograms and string results are not silently converted to numbers:
use suitable PromQL histogram functions or table/JSON. `Columns-*` and `Chart-*`
options are deliberately separate.

## Connection boundaries

Bearer authentication and custom HTTP headers are supported. The existing HTTP
driver does not expose per-request custom CA or mTLS client certificates. This
module does not start tunnels, acquire OAuth tokens or read Kubernetes credentials.
Stop cancels the client request, not a guaranteed immediate server-side query kill.
