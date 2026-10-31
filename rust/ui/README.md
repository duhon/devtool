# Rust browser application

`services-ui` is a Rust 2024 Yew client. Application logic, the native-messaging
transport, Console parsers/completion/formatting and Variables
editing are authored in Rust. Only generated wasm-bindgen glue and the vendored
CodeMirror editor/addons are JavaScript runtime dependencies.

## Integration

Build `cargo build -p services-ui --target wasm32-unknown-unknown --release`,
then run wasm-bindgen 0.2.108 with `--target web --out-name services_ui`.
Import and await the generated module's default `init()`; its WASM start
function mounts Yew at `#app`. No additional application JavaScript is loaded.
Keep `assets/` styles and CodeMirror CSS/scripts, including SQL,
JavaScript, overlay, show-hint, foldcode and foldgutter. UI-specific styles are
embedded by the Rust component. CSP must allow `wasm-unsafe-eval`.

The embedded Agent also exposes bounded inspected-page DOM tools in DevTools.
`browser/page_agent.rs` owns per-turn/site approval and revokes access on Stop,
completion and application teardown/hide. It evaluates the embedded static
`assets/page-agent.js` through `chrome.devtools.inspectedWindow.eval`, targeting
only that DevTools window's main inspected document without adding extension
permissions. No arbitrary-JavaScript tool is exposed. Standalone Console cannot
control a page. The script returns bounded untrusted text/control descriptions,
not field contents or browser storage, and validates origin, document identity,
unique controls and permitted input types before actions. Native SDK tool
replies still pass through the existing credential redactor.

The guide uses a separate top-layer popover and shadow DOM, with constructable
stylesheets and textContent (no HTML injection or target styling). Its pointer-
transparent spotlight and comic callout follow the original target. The show
tool stays pending until manual Next/Back/Finish/Escape or target loss, then
returns an untrusted decision for fresh inspection. The controller owns guide
identities, polling, cleanup and a five-minute limit; the page has a 30-second
heartbeat lease for abrupt panel loss. Guide-only consent and starting a guide
block mutating page tools for the whole turn, including after clearing a hint.

Page acceptance uses a separate real HTTP page and the SDK fixture. A test-only
`inspectedWindow` shim routes evaluations to that fixed CDP page, covering DOM
effects, consent, navigation, cancellation and rejection paths. This checks the
DevTools API contract; it does not replace a real DevTools-panel lifecycle test.

The main Console editor uses the local CodeMirror `native-direct` scrollbar
model: the real scroller stays visible, with no synchronized proxy bars or
synthetic 50px clipping gap. Preserve the paired vendor metrics changes and
`.CodeMirror-native-direct` CSS when updating CodeMirror. Other editors retain
its standard scrollbar model; native appearance and fading are browser-owned.

Chrome uses `runtime.connectNative(services_contracts::HOST_NAME)`, validates
protocol 1, and reads `runtime.lastError` synchronously on disconnect. Unary
requests, Variables, discovery and streams share independently correlated IDs.
Native errors include install/reload guidance; retry establishes a new port.
Health negotiates chunked replies: per-request assemblers validate byte offsets,
total size (at most 64 MiB), correlation and JSON before delivering the original
response. Pending-request teardown also drops incomplete fragments. Connection
failures stay in the affected check, with a compact Retry connection button
instead of a duplicate top-level error banner.
Extension clipboard Import uses the companion's `clipboardRead` operation;
Export has a DOM copy fallback.
Native host hints use `{service:"hints",op:"get"|"save",serviceName,text}`.

Templates are global, not part of Console session snapshots. The right Templates
panel edits per-service/section raw definitions (`@name description` plus a literal
multiline body); drafts persist separately in `servicesConsole.templateDrafts`.
The native `.console-templates.json` library is served over native messaging.
Saves validate definitions, lock across
processes, compare the previous section text and atomically rename a private file.
Conflicts preserve drafts instead of overwriting another window's changes.
Shared parsing/section catalogs live in `services-contracts::templates`. Completion
offers Splunk's `options` block instead of separate `from`, `to`, `app`, and `count`
sections. Separate `#from`, `#to`, `#app`, and `#count` sections are rejected;
their parameters belong only in `#options`, including in templates.
Completion inserts literal bodies with one undo step, normalizing inline `#section @name`
and replacing `#check @name` with the entire supplied check. Check templates
without their own check header receive `#check <description>` (falling back to
the template name for an empty description); explicit check headers are preserved.
Template suggestions open on Space immediately after a section marker; `@name`
on the same marker line filters them. Standalone references and ordinary section
content do not trigger template suggestions.
Template definitions reuse the main service's Console syntax mode, initialized
to the selected section. Each `@name` definition header is highlighted separately
and resets the embedded SQL/JSON/language state for the next template body.

History opens a right panel for the highlighted check. Its list initially fills
the panel without an empty editor; clicking an execution opens a read-only
snapshot, and clicking it again collapses it. Opening the panel always reloads
the list from the database, alongside automatic updates while visible; there is
no separate Refresh button. A persisted
workspace UUID and per-check UUIDv7 metadata (`workspaceId` / `checkIdentities`)
remain outside the request text. Editing or renaming a check keeps its identity;
editor generation snapshots restore it on Undo, and same-workspace cut/paste
moves retain it. Copies and copied Console sessions receive new identities.
Temporary session 0 retains identities only for its current in-memory workspace.
Execution snapshots live in the native embedded SQLite store, read over native
messaging, not in localStorage or Console session copies. Requests pass
the original check context to native execution, which records resolved requests
with sensitive credentials masked. Response bodies are preserved verbatim,
including any credentials or application-owned debug fields in the response.
Transport debug wrappers and duplicate effective parameters are omitted.
The viewer uses the Console's shared response renderer: JSON formatting,
MySQL/Splunk tables (including MySQL `\G`), and joined Terminal stdout/stderr
without stream-event or completion-metadata wrappers. It uses response outcome labels,
showing the recorded completion time without rewriting stored response bodies.
The right panel fetches metadata pages
and bounded UTF-8 detail chunks; it never loads the entire database. The global
payload budget is 100 MiB, with oldest executions removed toward 50 MiB when full.
This is not a strict disk-space cap: SQLite metadata and temporary rollback
journals consume additional space.

Console sessions persist transactionally in IndexedDB (`servicesConsole`,
`sessions` store, `library` record), not in quota-limited Web Storage.
The former `servicesConsole.sessions.v1` localStorage library migrates intact;
its original key is removed only after the IndexedDB transaction commits and
the source is checked for concurrent changes. Failed migration preserves it.
Every new application
starts in empty temporary session 0; temporary contents are never persisted.
The Sessions tool lists numbered workspaces and supports switching, deleting and
creating a named copy. Creating from 0 always starts empty, not from its edits.
Copies activate immediately; named sessions autosave all six services' checks,
responses, `var`, hints and invalid drafts. Dictionary, actual
environment variables and Agent chats remain global. The active session name
is above the right rail. Switching/deleting/copying is blocked while requests,
Agent turns/approvals or pending configuration saves are active.
Initial migration preserves the former service storage, native variables and
hints in an Imported session without activating it or removing the old files.
Independent edits from other windows merge by field; conflicting writes,
malformed storage and quota failures are reported, not silently overwritten.
Editor autosaves are ordered and coalesce pending snapshots without truncating
responses. Variables and Hints report success only after commit. The Sessions
signal shows pending writes; persistence failures remain visible, and closing a
window with pending/failed saves requests confirmation. IndexedDB still has a
browser-managed storage quota; failures never replace the last committed data.
An unavailable companion leaves temporary work usable while import is pending;
creating saved sessions waits for a successful import. Sessions saved by 2.x
with legacy-interface tabs still open; those tabs are dropped. Editor documents retain undo, cursor and folds across
service/request/profile switches, with caches isolated by Console session.
Profile labels rename inline; profiles and Console service tabs support drag
ordering. Console response ownership persists with documents,
so literal `#check`/`#responce` lines in Terminal output remain output rather
than executable request boundaries. Per-service saved/discovered hints use fuzzy
matching, scoped profile variables, SQL aliases, GraphQL types and nested
REST/gRPC JSON fields, including missing-comma insertion.
Console blocks start with `#check`. Saved legacy `#request` headers are migrated
without rewriting response output; the old marker remains readable for compatibility.
Console's left rail numbers real check blocks in document order. The check
containing the cursor is active while the cursor is visible; once scrolling
hides it, the top visible editor line determines the active check. Revealing
the cursor restores its check. Clicking a numbered tab moves the cursor to
that check. Execution, status, Debug, Stop, export and Agent context share this
selection. The right tool rail opens
Hints, Variables, Dictionary, Sessions or Agent in a fixed half-width panel. The main
status and Debug belong to the highlighted check in the selected service.
The right panel has its own status and Debug for the selected tool/profile/chat;
its operations never replace the main check's status or diagnostics.
Each column owns its Debug height: opening or resizing one does not move or
shrink the other column. Agent history keeps its reading position when Debug
changes size, without browser scroll anchoring to reveal the composer or messages.
The Console is the only interface; the 2.x per-service forms were removed in 3.0.
Service tabs and both side rails dock directly against the central workspace
frame; the service strip has no separate underline or gap above the editor.
Right-panel profile tabs and the tab-styled Hints title occupy the same top row
above their editor. Profile tabs use a Yew portal into a persistent header host;
the independent Variables component retains its state and owns their callbacks.
Hints autosave valid text into the active session; invalid drafts persist there
without replacing the last valid hints. Variable drafts are session-local.
Variables exposes only fixed `var` and read-only `env`; Dictionary exposes the
remaining named profiles and CRUD/order controls. Both Console tools have
independent component instances, drafts, queues and selection, and use separate
native services. Dictionary saves carry the profile the draft started from (`base`); if
another window saved that profile meanwhile, the save fails with CONFLICT, the draft is
kept and **Discard draft** loads the saved version. Dictionary re-reads its profiles when
its panel is shown again or the window regains focus. Session `var` is passed with each call, discovery and
Terminal stream and combined with global Dictionary/env for backend resolution;
it never replaces the legacy native variable file. Their replies share a merged
completion context.
Console user edits use CodeMirror's exact UTF-16 change ranges, rather than a
whole-document prefix/suffix guess. Response ownership follows cut/copy/paste
and undo generations. A check inserted into output splits its owned ranges:
the new request is executable, while literal markers in the old output remain
output. Generation snapshots contain only range metadata, not document copies.

## Embedded Agent

The Console-only Agent panel uses Copilot through the native companion. Its chat-session tabs occupy the shared
top header. Sessions, names, transcripts and drafts persist locally; closing
the panel or changing services does not destroy a chat or cancel a turn.
Each status ends with a small red S control. The main S cancels only the
operation currently displayed for the highlighted check, including discovery
when that is the displayed operation. The Agent S cancels only the selected
chat and its child requests, not other chats or independent user requests.
Configuration autosaves can be cancelled before dispatch; already-dispatched
local storage writes cannot be rolled back.
Switching checks, services, profiles or chats restores that owner's status
and diagnostics rather than combining concurrent operations into one strip.
Check/service/chat/profile tabs and tool buttons show a centered 12px signal
on their outer edge: left on the check rail, right on the tool rail, top on
horizontal tabs. The signal is always present, black when ready or viewed,
blinking green while running, green for success, red for errors, and yellow
for stopped/warning/approval. Keeping the matching check, profile or chat on
screen for 0.5 s (`SEEN_AFTER_MS`) acknowledges its non-running signal without
clearing its status or Debug; switching away sooner leaves it lit.
Running signals keep blinking even while viewed. A new background outcome
lights the signal again. Service and tool signals aggregate only unviewed
outcomes plus running work, prioritizing running, error, yellow, then success;
opening a service alone does not acknowledge its other checks. Variables and
Dictionary independently aggregate all their profiles, even with the panel closed.
Different checks support concurrent execution. Re-running an active check cancels
its current request and rejects the new run with a local error in that check's
status and Debug; other checks and chats keep running.
Agent-panel Stop and closing a chat also cancel its child requests and revoke
outstanding execution approvals.
Hiding the Services DevTools tab or closing its page cancels all work owned
by that window. Service and tool-panel switches do not.
MySQL uses a separate control connection to KILL QUERY while retaining the
original connection, preventing accidental cancellation of another pooled
query. Already committed changes are not rolled back. For HTTP APIs, remote
processing may continue after client cancellation; the stopped result says so.

Agent tabs use the shared Console tab styling. Double-click a tab to rename
inline (Enter/blur saves, Escape cancels); there is no separate name field.
Each tab has a close control. Closing stops its turn and revokes pending
approvals, but keeps the per-chat history and SDK session. Closed IDs persist
under `servicesConsole.agentClosedSessions` and are excluded when a new panel
loads. Closing the last tab creates a fresh empty chat. The **Closed (N)** tab
lists closed chats, newest first: Restore reopens one with its history and SDK
session; Delete and Delete all (confirmed) ask the native `agent_sessions`
service to remove `<data>/.agent/sessions/<id>/`, then drop the browser records.
Chats running in another connection, or a request with an invalid ID, are not
deleted and stay listed; errors appear in the list's status.

Enter sends a chat message; Shift+Enter inserts a newline. There is no Send
button. Empty drafts and sends during an active turn are ignored. Assistant
messages render Markdown paragraphs, emphasis, lists, code blocks and tables as
Yew nodes. Raw HTML stays literal, unsafe link schemes are inert, and images
never load external resources. User messages remain literal text.

Settings loads the Copilot model catalog through the native SDK once per DevTools
window; reopen DevTools to pick up catalog changes. The model input
filters suggestions by name, ID and published capability category; Reasoning
lists only that model's supported levels. Without saved preferences, new chats
use Copilot Fusion (the SDK's `auto` router), which manages Reasoning itself.
Choices persist locally and are captured when a new chat is created. Existing
chats and resumed SDK sessions retain their original model and reasoning settings.

Settings → Agent skills sets a **Team skills folder**: an absolute local path
(`~/` allowed), such as a team repository checkout or a synced drive, with one
`<name>/SKILL.md` folder per skill. The native host (`agent_skills`
`get`/`save`) stores it in `<data>/.agent/skills.json`, rejects relative or
missing paths (red input plus a Settings status error) and lists the skills it
found and the ones it could not read (frontmatter needs `name` and
`description`). Every following Agent reply, in new and existing chats, hands
the folder to the SDK as a skill directory with the built-in `skill` tool; no
other skills (personal `~/.copilot/skills` or bundled ones) are loaded. A skill
only supplies its `SKILL.md` instructions: the Agent has no file tools, so files
beside it are not readable. A folder that later disappears doesn't stop the
reply; it ends with a warning. Skills Copilot used appear as `Skill: name`
activity and in the Agent status debug (`skills`).

Each message automatically provides Console request context; the Agent can read
selected response pages and cached schema without a separate sharing toggle.
Known credential values and
credential-shaped fields are masked by the companion. This is not a guarantee
that arbitrary business data is anonymous; inspect the selected check first.
Environment values are never part of the context API.

Workspace tools read the live App model rather than scraping the page.
Check IDs and document revisions reject stale writes. Agent changes use
CodeMirror `replaceRange`, the same UTF-16 tracking and normal Undo, rather than
rewriting storage. The words an edit changed (or the line it removed text from)
flash for 1.2 s and fade (`cm-agent-edit`), so the user sees what the Agent did.
Omitted connection directives remain local and are preserved;
masked credentials cannot be inserted into a request.
Check context exposes plain titles, without the directive marker. Agent
create/edit titles must not contain `#check` or legacy `#request` prefixes.
`console_edit_check` accepts title, request or both; title-only edits preserve
the request and its captured output and remain a single Undo step.
Reads use bounded Unicode-character pages. Executing requires a separate
visible approval for the exact request revision and variable snapshot;
parameters are resolved and frozen locally before dispatch. No SDK shell,
filesystem, browser or external MCP tools are exposed.

Browser acceptance selectors: `console-editor-editor`, `console-service-rest`
(and the other Console languages), `console-variables-toggle`,
`console-dictionary-toggle`, `console-variables-editor`,
`console-dictionary-editor`, `console-tool-panel`, `native-status`.
The editor test ID is on the CodeMirror wrapper; integrations can use its
CodeMirror instance API or the editor's keyboard input.

## Validation

`cargo test -p services-ui` runs pure Rust tests and captured reference fixtures
from the former JavaScript parser/formatter tests. These JSON fixtures do not
depend on the deleted implementation. Tests live outside the sources: shared
editor, parsing and storage logic in `tests/core/`, one file per service or
panel in `tests/modules/`, helpers in `tests/support/`. WASM builds validate
browser code.

Build the distribution with `cargo run -p xtask -- build`, then run
`cargo run -p xtask -- test browser` (`--list` shows the scenarios). The Rust
CDP driver tests the archived extension using real Chrome and disposable
native-host data. Fixtures exercise the Console editor and side panels,
Variables/Dictionary hover and autosave, service-specific hints, SQL aliases, nested GraphQL
snippets, gRPC discovery, saved-hint persistence and stale replies, and
clipboard permission failures. It never reads or modifies the OS clipboard.
No Node.js, npm or Playwright is needed.

`cargo run -p xtask -- test-agent` runs the Agent subset:
live edits and Undo, conflicts, execution approval/denial/Stop, captured response
reads, cross-window session restoration and SDK subprocess cleanup. It uses the
real Rust SDK with a deterministic local protocol peer, without model requests
or private application data; the same scenarios also run in `test browser`.

Contextual completions remain lexical/schema based rather than language-server
analysis. CodeMirror SQL/JSON modes remain browser dependencies.
