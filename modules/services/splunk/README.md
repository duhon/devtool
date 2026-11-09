# Splunk module

The Splunk service: SPL searches over the export API through the host's `http` driver.

Splunk API calls share a per-server queue keyed by URL origin (scheme, host and
port), independent of the check, session, app or credentials. The module paces
through the Kernel: a shared lock per origin (`cmd::lock`, Web Locks between the App's
windows) and the next allowed time in its kv. Every export request and retry waits
at least 13 seconds after the previous call completes: at most five calls in any
minute. Different servers proceed independently. HTTP 429 extends the shared
cooldown using `Retry-After`, or 60 seconds when it is absent. Automatic redirects
and implicit HTTP retries are disabled for Splunk so they cannot bypass pacing.
Waiting remains cancellable; the overall search timeout is 10 minutes including
queue waits. Headers and each wait for stream data have a 120-second timeout;
an active export may run up to the overall deadline. Calls made by
other applications or installations are outside this queue.

Splunk uses `POST search/v2/jobs/export` with JSON streaming and `preview=false` by default,
matching the official SDK's preference for Splunk 9+. HTTP 404 falls back once
to legacy `search/jobs/export` through the same paced queue, with both attempts
visible in Debug; other errors do not trigger a compatibility retry.
The module parses records as bytes arrive, including split UTF-8 characters,
instead of buffering the full HTTP body. Count limits rows locally:
omitted/blank = 100, positive N = at most N rows, 0 = the whole result. Offset
defaults to 0 and skips the first N rows locally before Count is applied; it is
not sent as an unsupported export parameter. Each run starts a new search,
so Offset is not stable pagination of a saved job and does not avoid scanning
the skipped rows. It closes
the stream as soon as the portion arrives; no status polls or results pagination
are needed. SPL is not rewritten, so global sort/aggregate commands can still
block early final output. `Preview: true` opts into provisional snapshots:
the first snapshot with rows after Offset is returned when Count is reached or
that snapshot ends. A completed preview may contain fewer than Count rows.
Count: 0 returns all remaining rows of that one preview snapshot, not all future
snapshots. Offset restarts for each snapshot; rows from separate previews or the
final result are never combined. An empty portion is skipped while the search
is still previewing. Preview output is labeled provisional in Console, History
and the service result view, with WARNING rather than a final OK status.
With Preview: false, snapshots remain ignored. `resultCount` is the number of
returned rows, not the total matches; Debug's `export` distinguishes EOF, Count
termination and preview completion. Preview does not guarantee immediate
results if the server/search itself delays producing a snapshot.
Malformed/truncated JSON, stream errors and Splunk ERROR/FATAL messages fail the
request explicitly. Individual records and retained results are bounded by the
existing 64 MiB response limit.
