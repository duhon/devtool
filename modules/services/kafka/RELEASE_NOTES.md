## 0.1.0

- Standalone Kafka service module and versioned native-driver contract.
- Metadata, partition offsets, bounded snapshot/live reads, message production,
  group inspection, lag and topic configuration.
- Admin writes: `CREATE`, `DELETE` (topic or `GROUP`), `ALTER` (partitions or `CONFIG`),
  `PURGE`, `RESET` and `COMMIT` group offsets; one result row per change.
- Table, vertical and lossless JSON output; scoped topic and consumer-group discovery;
  action completions that follow the command grammar.
- `Transform` support: `.` is the records of READ/TAIL, the rows of LAG, else the
  listed items or the answer.
- Runs on the host's native `kafka` 1.1 driver.
