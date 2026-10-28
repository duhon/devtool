# Service clients and request syntax

Console supports mysql, rest, graphql, grpc, splunk and terminal. Each service
has its own document. A full check starts with `#check Title`; section contents
start on the following line. Captured response blocks are owned output, not new
requests, even if the returned text contains directive-shaped lines.

The examples below are full documents. For console_edit_check or
console_create_check, pass only the request sections, without a `#check` header.
Their `title` is a plain name, for example `"products overview"`, not
`"#check products overview"`: the application supplies the marker.
`console_edit_check` can rename a check by passing only `title` alongside
service, checkId and revision. Omit request to preserve its body and responses;
pass title and request together only when both should change.
Use real context and variable references; these example hosts/schema names are
illustrative, not known workspace resources.

## MySQL

```text
#check Product example
#endpoint
mysql://{{store:USER}}:{{store:PASSWORD}}@{{store:HOST}}/{{store:DATABASE}}
#query
SELECT * FROM catalog_product_entity LIMIT 10;
```

Confirm tables and columns through the actual cached schema before using them.
Do not assume every Magento database has identical tables or permissions.

Result columns are highlighted using the query's MySQL column metadata,
including aliases: numbers, booleans, dates/times and native JSON. An optional
`#options` section can override text columns containing JSON, for example
`Columns-Type: feed_data=json enabled=boolean`. Supported types are text, number,
boolean, datetime and json. Assignments are whitespace-separated and use exact
returned column names. Overrides affect display only, not the underlying values.
JSON stays compact in horizontal text tables; a query ending in `\G` uses a
vertical record layout with indented JSON. Cell contents are flat, never folding
JSON trees. Console tables remain copyable plain text, including their borders.

## REST

```text
#check Book example
#endpoint
PUT https://fakerestapi.azurewebsites.net/api/v1/Books/123
#headers
Content-Type: application/json
Accept: application/json
#body
{"id": 123}
```

The method and URL are together under `#endpoint`, not a separate bare method
line. Supported methods are GET, POST, PUT, PATCH, DELETE and HEAD. `#headers`
uses one Name: value per line. `#body` is optional request text.
Path parameters, enum suggestions and JSON body templates depend on matching
discovered metadata; consult the discovery topic, not a guessed UI button.

## GraphQL

Use `#endpoint` with the GraphQL URL, optional `#headers`, `#query` with the
operation and optional `#variables` containing a JSON object. Keep fragments,
operation definitions and variable references intact. Schema is obtained through
introspection; ordinary query execution is separate.

## gRPC

Use `#endpoint` with the selected transport/server/service/method address,
optional `#headers` for metadata, optional `#proto`, and `#message` with JSON.
Inspect the existing endpoint and schema rather than guessing a method name.
The native client supports unary and streaming RPC shapes, reflection and
in-memory proto compilation. A missing reflection response is not proof that
the server has no callable methods.

## Splunk

Use `#endpoint` for the base URL, optional `#headers`, and `#search` with SPL.
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
dots are literal names, not object paths. Types match the MySQL list above.
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

## Terminal

Use `#endpoint` with the configured shell/command endpoint, `#command` for the
command text and optional `#env` for Name: value entries. This is an application
service, not general shell permission for the Agent. Running a Terminal check
still requires the user's explicit request and the separate one-time approval.
Never use it to read credentials, browser storage or application data files.
