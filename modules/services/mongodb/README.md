# MongoDB for Services DevTools

MongoDB service module for **Services DevTools 4.1.0+**: one WASM module, SDK ABI 2,
host `mongodb` driver 1. No external CLI or native library is needed.

## Install

Install **MongoDB** in Settings → Modules.

## Checks

```text
#check Adult users
#target
mongodb://{{USER}}:{{PASSWORD}}@localhost:27017/app
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

One check runs one command. `#action` is the command name, `#collection` its collection
(not needed for database-level actions) and `#payload` a JSON object with the command's
own fields. Relaxed JSON is accepted (comments, trailing commas, unquoted keys, single
quotes); values use Extended JSON (`{"$oid": "…"}`, `{"$date": "…"}`,
`{"$numberLong": "…"}`). The module builds `{<action>: <collection>, …payload}` for the
driver. Unknown actions list the supported ones; `runCommand` sends any command.

| Action | Payload | Command |
| --- | --- | --- |
| `find` | `filter, sort, projection, limit, skip, hint, …` | `find` |
| `aggregate` | `{"pipeline": [...]}` or `[...]` | `aggregate` with `cursor: {}` |
| `count`, `countDocuments` | `{"filter": …}` | `count` / `aggregate` `$match` + `$group` |
| `distinct` | `{"key": …, "filter": …}` | `distinct` |
| `insert` | `{"documents": [...]}`, `[...]` or one document | `insert` |
| `update` | `{"filter"/"q": …, "update"/"u": …, "upsert", "multi"}` | `update` |
| `delete` | `{"filter"/"q": …, "limit": 1}` (`{}` must be explicit) | `delete` |
| `findAndModify` | `{"filter": …, "update"/"remove": …, "new"}` | `findAndModify` |
| `createIndexes`, `dropIndexes`, `listIndexes`, `drop`, `create`, `collStats` | command fields | same name |
| `listCollections`, `listDatabases`, `ping`, `dbStats`, `serverStatus`, `buildInfo`, `hello`, `connectionStatus`, `dropDatabase` | command fields | same name, value `1` |
| `runCommand` | the whole command document | as written |

Syntax errors name the line and column.

## Target and options

`#target` is a `mongodb://` or `mongodb+srv://` URI with `{{VARIABLE}}` substitution.
The database is the URI path, else `Database:` in `#options`. Credentials are never
shown in results, hints, debug data or errors.

| Option | Meaning |
| --- | --- |
| `Timeout` | Seconds for the whole call (1-3600, default 30) |
| `Connect-Timeout` | Seconds to connect (1-120) |
| `Max-Documents` | Documents shown (1-10000, default 1000); more are marked truncated |
| `Batch-Size` | Documents per server batch |
| `Database` | Database when the URI has none |
| `Format` | `table`, `json` or `vertical` |
| `Columns-Type` | `column=json` and other display overrides |

## Results

Cursor commands show a table of the top-level fields of the first documents; other
commands show the reply document. A server error shows its code and name after the
documents read before it. The result is one plain driver call, not a stream.

## Completion

Discovery starts by itself: once the target is complete (the cursor left its line, or
another section began) the collections are listed, and the collections the check names
(`#collection`, and `from`/`coll` in the payload, at most four, only if they exist) are
sampled from 20 documents. A target with `{{variables}}` is resolved first. Partial or
invalid targets stay quiet; Discover forces a refresh.

Completion knows the structure of the payload, like GraphQL's:

- keys valid for the action, query, update, aggregation and expression operators, and the
  fields of the scope as keys of filters, projections, sorts and updates;
- values: collections after `from`/`coll`/`$out`/`into`, `localField` of the collection and
  `foreignField` of `from`, field paths `"$field"` of the stages before the cursor (including
  fields added by `$addFields`, `$group`, `$project`, `$lookup` `as`), `$$let` variables,
  `$$ROOT`, `$$NOW`, sort directions, booleans and BSON `$type` names;
- scope: stages inside `$lookup`/`$unionWith` pipelines complete the fields of their own
  collection; `$facet` and `$graphLookup` are followed too;
- skeletons: picking `$lookup`, `$group`, `$match`, … inserts the stage with the cursor in
  the first value; an empty payload offers the starter of the action. Objects and arrays
  are inserted as blocks laid out like JSON completion: the cursor on its own line, indented
  two spaces deeper than the line, the closing bracket at the indentation of the line.

Highlighting tells apart data keys (`property`), `$operators` and stages (`keyword`), the
fixed fields of a command (`pipeline`, `from`, … as `attribute`), Extended JSON wrappers
(`type`), field paths such as `"$storeId"` and `"$$sid"` (`variable-3`), strings, numbers,
`true`/`false`/`null` and comments.

Discovery only reads, uses short timeouts, is cached per target, database and collection,
and fails softly with a status note.

## Safety

Commands run as written: inserts, updates, deletes and drops change the database.
Use a read-only user for exploration.

## Tests

`cargo test -p services-module-mongodb` runs the unit and module tests.
`scripts/test-integration.sh` starts `mongo:7` in Docker and runs the packaged WASM against
the real host driver.
