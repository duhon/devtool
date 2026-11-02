---
name: module-mysql
description: "MySQL checks in the Services DevTools Console: the mysql:// #endpoint, #query SQL, #options Columns-Type and how results are shown. Load before writing, editing or explaining MySQL requests."
---

# MySQL service

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

Use the discovered schema when available; do not invent table names. For
example, Magento commonly uses catalog_product_entity, but confirm the actual
schema before proposing a query.
