---
name: module-prometheus
description: "Prometheus PromQL queries, instant and range evaluation, metrics and labels. Use when the user asks to query Prometheus or write PromQL."
---

# Prometheus

Write PromQL in `#payload`. Set the HTTP(S) server base URL in `#target`; preserve
any reverse-proxy path prefix. Never invent credentials or metric names.
Use discovery when metric or label names are unknown.

`#metadata` holds HTTP headers and module-owned execution parameters:

- `Query-Mode: instant` evaluates once; `Query-Time: now` is the default.
- `Query-Mode: range` requires a start and a positive step; the end may be `now`.
- `Query-Start`, `Query-End`, `Query-Time`: absolute timestamps or relative `now`
  expressions, such as `now-1h`.
- `Query-Step: 30s` is the evaluation spacing, not a PromQL range selector.
- `Query-Timeout` controls the requested server evaluation timeout.

Use existing `{{var:name}}`, `{{env:NAME}}` or Dictionary placeholders, including
for sensitive header values. Do not copy credentials into query documents.
Keep PromQL expressions unchanged unless the user asks to edit them.

`#options` accepts `Format: table`, `Format: json` or `Format: chart`.
Charts are copyable Unicode/Braille text, not SVG. Use range mode for time-series
graphs; instant queries show a single point, not accumulated history.
`Chart-Width` defaults to 96 (40-160), `Chart-Height` to 20 (8-40).
Set both `Chart-Min` and `Chart-Max` for a fixed Y scale. Otherwise scales expand
but do not shrink across repeats of the same check. `Repeat: 2s` refreshes a
moving range, such as `now-15m` to `now`. No animation between values is implied.
Send copied graphs in a monospaced code block without wrapping.
Empty results are valid; report server errors, warnings and information notes.
Native histograms are not ordinary numeric samples.

For rates, use a range selector appropriate to the metric's scrape interval.
Do not apply `rate()` to gauges just because they are numeric. Preserve label
matching and aggregation semantics when modifying expressions.
