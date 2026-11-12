# MongoDB module release notes

## 1.0.0

- A WebAssembly module for Services DevTools 4.1 that runs MongoDB statements through
  the host `mongodb` driver.
- Sections in the REST style: `#target` is a `mongodb://` or `mongodb+srv://` URI,
  `#collection` and `#action` name the command (`find`, `aggregate`, `insert`, `update`,
  `delete`, `listCollections`, `runCommand`, …) and `#payload` is a relaxed JSON object
  with the command's fields in Extended JSON.
- Documents come back as a table (ObjectId, dates and numbers readable) or as JSON;
  server errors show their code and name after the documents already read.
- Ctrl+Space completes collections, actions, payload keys, query, update and aggregation
  operators and the fields of a collection sampled from a few documents. Discovery only reads.
- `#options`: Timeout, Connect-Timeout, Database, Max-Documents, Batch-Size, Format,
  Columns-Type. The Agent can load the `module-mongodb` skill.
