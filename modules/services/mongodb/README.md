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
Max-Documents: 200
#payload
{"filter": {"age": {"$gt": 21}}, "sort": {"name": 1}, "limit": 10}
```

One check runs one command. `#action` is the command name, `#collection` its collection
(not needed for database-level actions) and `#payload` a JSON object with the command's
own fields. The payload must be strict JSON (no comments, trailing commas, unquoted keys or
single quotes), so a check can be shared with any JSON tool; a run with invalid JSON fails
with `Invalid payload: … (line N, column M)`. Values use Extended JSON (`{"$oid": "…"}`, `{"$date": "…"}`,
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
The database is the URI path. Credentials are never
shown in results, hints, debug data or errors.

### Client certificate (X.509)

For passwordless authentication (for example Atlas) add a `#headers` section:

```text
#target
mongodb+srv://cluster0.xxxxx.mongodb.net/mydb
#headers
Auth: x509
Certificate: {{env:MONGO_CERT}}
```

`Certificate` is the path of a PEM file with the client certificate and key. The module
appends `authMechanism=MONGODB-X509&authSource=$external&tlsCertificateKeyFile=…` to the
`#target` query. `Auth` takes `x509` or `scram` (the default); `CertificatePassword` is the
password of an encrypted key and is treated as a secret. `Auth: x509` needs `Certificate`
and the other way round; a `#target` with a password or its own `authMechanism`,
`authSource` or `tlsCertificateKeyFile` is rejected. Put `tlsCAFile` in the URI for a
private CA.

| Option | Meaning |
| --- | --- |
| `Max-Documents` | Documents shown (1-10000, default 1000); more are marked truncated |
| `Batch-Size` | Documents per server batch |
| `Format` | `table`, `json` or `vertical` |
| `Columns-Type` | `column=json` and other display overrides |
| `Columns-Size` | `column=100`: the width of a column in characters |

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
- comparisons offer sampled values: after `"field": `, after `$eq`/`$ne`/`$gt`/`$gte`/`$lt`/`$lte`
  and inside `$in`/`$nin`/`$all` arrays. A field qualifies when the sample has at least two
  documents and at most 10 distinct scalar values (strings up to 40 characters, numbers,
  booleans; array fields contribute their elements; `_id`, dates and ids never). No extra
  query is made.
- commas come only from completion, so the payload stays strict JSON: after a complete value
  (string, number, literal, closed object or array) the next members are offered with the
  leading `,` glued to the value (the pick replaces the blanks and newline back to it, also
  when a `{` or `"` was already typed for the new member) and the members' indentation;
  there is no comma item; sampled values already in a `$in` list are not offered again; a pick
  placed before an existing member ends with a comma; nothing is offered between a value and
  a comma that is already there. A payload that is not valid is rejected on run with its line
  and column.
- every value position (after `"key": ` or inside an array) also offers `{}`, `[]`, `""` and
  `true`/`false`/`null`; the shape the key expects comes first (stage skeletons, `[` for
  `$in`/`$and`/`$round`, `{` for `$set`/`$cond`/accumulators).

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
