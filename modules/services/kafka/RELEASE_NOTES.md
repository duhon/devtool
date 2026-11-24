## 0.1.0

- Standalone Kafka service module and versioned native-driver contract.
- Metadata, partition offsets, bounded snapshot/live reads, message production,
  group inspection, lag and read-only topic configuration.
- Table, vertical and lossless JSON output; scoped topic discovery.
- `Transform` support: `.` is the records of READ/TAIL, the rows of LAG, else the
  listed items or the answer.
- Runs on the host's native `kafka:1` driver.
