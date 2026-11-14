# MongoDB module release notes

## 1.1.0

- X.509 client-certificate authentication through a new `#metadata` section
  (`Auth: x509`, `Certificate: <PEM file>`, optional `CertificatePassword`); the module
  adds `authMechanism=MONGODB-X509`, `authSource=$external` and `tlsCertificateKeyFile`
  to the `#target`.
- `Timeout` moved to the console (global `#options` key); the module no longer parses it.
- New `#options` key `Columns-Size`. `Connect-Timeout` and `Database` are removed: the
  database is the URI path.

## 1.0.0

- A WebAssembly module for Services DevTools 4.1 that runs MongoDB statements through
  the host `mongodb` driver.
- Sections in the REST style: `#target` is a `mongodb://` or `mongodb+srv://` URI,
  `#collection` and `#action` name the command (`find`, `aggregate`, `insert`, `update`,
  `delete`, `listCollections`, `runCommand`, …) and `#payload` is a strict JSON object
  with the command's fields in Extended JSON.
- Documents come back as a table (ObjectId, dates and numbers readable) or as JSON;
  server errors show their code and name after the documents already read.
- Discovery starts on its own once the target is complete and samples the collections the
  check names; completion proposes keys, values (collections, field paths, `$$variables`,
  finite sets), operators with descriptions and stage skeletons, scoped to the collection
  of nested `$lookup`/`$unionWith` pipelines. Objects and arrays are inserted as indented
  blocks, and every value position also offers `{}`, `[]`, `""` and the literals, led by the
  container the key expects (stage skeleton, `[` for `$in`/`$and`/`$round`, `{` for `$set`/`$cond`).
  Comparisons (`"field": |`, `$eq`…`$lte`, and inside `$in`/`$nin`/`$all`) offer the values
  the sampled documents show for fields with few distinct scalar values. Discovery only reads.
- The payload is highlighted by token: keys, `$operators`, command fields, Extended JSON
  wrappers, field paths, strings, numbers and atoms.
- `#options`: Timeout, Connect-Timeout, Database, Max-Documents, Batch-Size, Format,
  Columns-Type. The Agent can load the `module-mongodb` skill.
