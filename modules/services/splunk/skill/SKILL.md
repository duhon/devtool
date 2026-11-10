---
name: module-splunk
description: "Splunk searches in the Services DevTools Console: #target, #payload SPL, #options (From, To, Count, Preview, Offset, Columns-Type, Columns-Size), the rate limit and preview results. Load before writing, editing or explaining Splunk requests."
---

# Splunk service

Use `#target` for the base URL, optional `#headers`, and `#payload` with SPL.
`#options` accepts From, To, App, Count, Preview, Offset, Columns-Type and
Columns-Size as Name: value lines. These parameters
must be inside `#options`, not separate sections. Do not invent indexes or use
Terminal to work around missing Splunk context.
There is no MySQL-style schema for Splunk. Use explicit literal field names:
`Columns-Type: message.timestamp=datetime message.message=json`.
Use `Columns-Size: message.message=100` to wrap a column at at most 100 Unicode
characters of content per line (not pixels). Headers wrap too; short columns
are not widened unnecessarily. Wrapping does not truncate data, and the actual
line breaks and ASCII borders survive plain-text copying. Field names containing
dots are literal names, not object paths. Supported types are text, number, boolean, datetime and json.
Unspecified text is not automatically parsed as JSON and has no content folds.
All Splunk API calls share a per-server queue capped at five calls per minute,
with at least 13 seconds after a call finishes. Searches use a single streaming
`search/v2/jobs/export` request, without job creation, status polling or a separate
results fetch. HTTP 404 falls back once to legacy `search/jobs/export`, observing
the same queue; other failures do not trigger this fallback.
Count is a client-side row limit: default 100 when omitted,
0 for all rows, or a positive integer for at most N rows. Offset defaults to 0:
skip that many rows before collecting Count rows. Each execution starts a new
search and skips rows locally, so Offset does not accelerate the search or
provide stable pages from a saved job. Existing
Console templates explicitly use Count: 30. Once N rows arrive, the client
closes the response without waiting for EOF; resultCount counts returned rows,
not the unknown total number of matches. Preview defaults to false. With
Preview: true, return one provisional snapshot as soon as Count is reached or
that snapshot ends; fewer than Count rows may be available. Count: 0 means the
entire selected snapshot, not all successive previews. Offset resets for each
snapshot; snapshots with no rows after Offset are skipped until a usable
preview or the final result arrives. Never mix rows from different snapshots.
Preview output carries a visible provisional label and WARNING status in both
Console and History; it is not automatically refreshed into a final result.
The original SPL is not rewritten. Blocking commands such as a
global sort can still delay the first final rows; export cannot supply a final
top-N before Splunk has determined it. The table is displayed once the requested
portion is available, not progressively row by row. Preview itself does not
guarantee immediate output if Splunk has not produced a snapshot.
Do not retry a queued check or start extra checks to accelerate it. Queue waits
are included in the 10-minute timeout. Debug records stream completion, Count
termination, messages and rate-limit retries. Stop closes the request; it does
not roll back remote side effects or guarantee immediate server-side cleanup.
