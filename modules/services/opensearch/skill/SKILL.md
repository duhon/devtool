---
name: module-opensearch
description: "OpenSearch checks in the Services DevTools Console: the URL under #target, the HTTP method under #action, #headers and a JSON or NDJSON #payload for search, documents, indices, bulk and multi-search. Load before writing, editing or explaining OpenSearch requests."
---

# OpenSearch service

The OpenSearch HTTP API. Each check is one request.

```text
#check Search
#target
http://localhost:9200/logs-*/_search?pretty=true
#headers
Accept: application/json
#payload
{"size": 10, "query": {"match_all": {}}}

#check Multi-search
#target
http://localhost:9200/_msearch
#action
POST
#payload
{"index":"logs-*"}
{"size":5,"query":{"match_all":{}}}
```

- `#target` is only the full URL with the index, API path and query string.
  `#action` is the method like REST: GET, POST, PUT, PATCH, DELETE, HEAD or
  OPTIONS; without it the request is a GET, which keeps its `#payload` for
  searches.
- `#headers` uses one `Name: value` per line. Log in with an
  `Authorization` header built from application variables; never put real
  credentials in a check. AWS SigV4 signing is not supported.
- `#payload` is JSON, kept on GET searches. `_bulk`, `_msearch` and
  `_msearch/template` take NDJSON, one record per line; the module adds the
  final newline. A body without Content-Type is sent as JSON, or NDJSON for
  those APIs.

Requests time out after 120 seconds and don't follow redirects or retry.
Writes and deletes are sent as written, without confirmation.

Discovery reads the cluster root (also behind a reverse-proxy mount),
`_mapping` and `_alias` with the check's headers, and never writes. It
completes index and alias names in paths and mapped fields in Query DSL
(`match`, `term`, `terms`, `range`, `exists`, `_source`, `sort`, one `bool`
level) and document bodies. Indices starting with `.` are not offered, but
requests to them work. `_cluster`, `_cat`, `_nodes`, `_tasks` and `_snapshot`
URLs get their API paths without reading mappings.
