---
name: module-mongodb
description: "MongoDB checks in the Services DevTools Console: the mongodb:// #target, #collection, #action and JSON #payload sections, #options and how results are shown. Load before writing, editing or explaining MongoDB requests."
---

# MongoDB service

```text
#check Adult users
#target
mongodb://{{mongo:USER}}:{{mongo:PASSWORD}}@{{mongo:HOST}}/app
#collection
users
#action
find
#options
Timeout: 30
Max-Documents: 200
#payload
{"filter": {"age": {"$gt": 21}}, "sort": {"name": 1}, "limit": 10}
```

One check runs one command. Confirm collections and fields through the discovered
schema (or `listCollections`, a `find` with `"limit": 1`) before using them. The editor
discovers collections once the target is complete and completes keys, values and stage
skeletons from them.

## Sections

- `#target`: a `mongodb://` or `mongodb+srv://` URI; `{{VARIABLE}}` works. The database
  is the URI path, else `Database:` in `#options`. Credentials belong in the URI (or in
  variables); they are never shown in results, hints or errors. Read preference, TLS and
  replica set come from the URI query.
- `#collection`: the collection; not needed for database-level actions.
- `#action`: the command name (case-insensitive). The module sends
  `{<action>: <collection>, ...payload}` (`1` instead of the collection for database
  actions). An unknown action answers with the supported list; `runCommand` sends any
  command.
- `#payload`: a JSON object with the command's fields. Relaxed JSON is accepted
  (comments, trailing commas, unquoted keys, single quotes). Use Extended JSON for typed
  values: `{"$oid": "…"}`, `{"$date": "2024-01-01T00:00:00Z"}`, `{"$numberLong": "5"}`,
  `{"$numberDecimal": "1.5"}`, `{"$regularExpression": {"pattern": "^a", "options": "i"}}`.
  Plain integers become int32/int64, fractions double. Errors name line and column.
- `#options`: `Timeout` (seconds, default 30), `Connect-Timeout` (seconds),
  `Max-Documents` (default 1000, at most 10000), `Batch-Size`, `Database`,
  `Format: table|json|vertical`, `Columns-Type: name=json …` (display only).

## Actions and payloads

| Action | Payload |
| --- | --- |
| `find` | `{"filter", "sort", "projection", "limit", "skip", "hint", "collation", "batchSize", "maxTimeMS", …}` |
| `aggregate` | `{"pipeline": [...]}` or just `[...]` (`cursor` is added) |
| `countDocuments` | `{"filter", "skip", "limit"}` (runs an aggregate `$group`) |
| `count` | `{"filter"}` (sent as `query`) |
| `distinct` | `{"key": "field", "filter"}` |
| `insert` | `{"documents": [...]}`, `[...]` or one document |
| `update` | `{"filter" or "q", "update" or "u", "upsert", "multi", "arrayFilters", "collation", "hint"}` or a raw `"updates"` |
| `delete` | `{"filter" or "q", "limit"}`; `limit` defaults to 1 (0 = all); `{}` must be explicit |
| `findAndModify` | `{"filter", "update" or "remove": true, "new", "upsert", "sort", "projection"}` |
| `createIndexes` | `{"indexes": [...]}`, `[...]` or one spec with `key` (name is generated) |
| `dropIndexes`, `listIndexes`, `drop`, `create`, `collStats` | the command's own fields |
| `listCollections`, `listDatabases`, `ping`, `dbStats`, `serverStatus`, `buildInfo`, `hello`, `connectionStatus`, `dropDatabase` | the command's own fields |
| `runCommand` | the whole command document, e.g. `{"explain": {"find": "users"}, "verbosity": "queryPlanner"}` |

Examples:

```text
#collection
orders
#action
aggregate
#payload
[{"$match": {"status": "paid"}}, {"$group": {"_id": "$user", "total": {"$sum": "$amount"}}}]
```

```text
#collection
users
#action
update
#payload
{"filter": {"_id": {"$oid": "65f0c0ffee0000000000000a"}}, "update": {"$set": {"active": true}}, "upsert": false}
```

## Results

Cursor results (find, aggregate, listCollections, listIndexes) are a table whose columns
are the top-level fields of the first documents; `ObjectId("…")` and `ISODate("…")`
print readable, nested values are compact JSON. `Format: json` prints the documents as
relaxed JSON. Other commands print the reply document. Hitting `Max-Documents` is
marked as truncated. A server error prints as `ERROR code (codeName): message` after the
documents read before it; write errors in a reply are errors too.

## Safety

Never run writes (`insert`, `update`, `delete`, `findAndModify`), index changes or drops
unless the user explicitly asks for them. Prefer `find` with `limit` and `countDocuments`
for exploration. Do not print credentials; keep them in variables.
