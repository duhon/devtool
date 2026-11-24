# Kubernetes module release notes

## 1.0.0

- A WebAssembly module for Services DevTools that runs kubectl-style verbs through the host
  `kubernetes` driver.
- `Transform: {{$name(args)}}` in `#options` works here: `.` is the rows as objects keyed by column header (with the whole API object behind each row under `object`), the log lines (or log objects) or text lines, or with `Format: json|yaml` the API object; an array of objects shows as a table, lines as text. `logs -f` and `watch` are transformed when they end; on Stop the output so far is transformed as text.
- `Format: table` shows the wide columns of the server table too (`IP`, `NODE`… like `kubectl -o wide`); the default view keeps hiding them.
- Discovered names stay offered (and visible to the Agent) after they age; the next hint after 10 minutes refreshes them, and a failed refresh keeps the old ones.
