# Console interface

Services DevTools has one interface, the Console. The Agent is embedded in the
Console and runs in the Chrome extension over native messaging. The Agent panel
is the `agent` topic; working with the inspected web page is the `page` topic.

It opens in two ways: inside Chrome DevTools as its Services tab, or standalone
in a browser tab at `chrome-extension://ckicpgdfkgcmgnclbfbcbkillcpaffnb/services.html`
(paste it into the address bar; the address is the same for every installation).
Standalone inspects no web page, so the page tools are unavailable there, and
like any new window it starts in empty session 0. To keep the inspected page in
a separate window, undock DevTools instead: the DevTools ⋮ menu → Dock side →
Undock into separate window.

## Layout and controls

- The top service tabs select a service; each installed module adds one.
- The middle is the editable document for the selected service. It contains
  `#check` requests and their owned captured responses.
- The left rail numbers checks in document order and navigates to them.
- The right rail has Sessions, Agent, Import/Export where supported, Hints,
  Variables and Dictionary. A tool opens in the right half-width panel.
- Hints edits saved completions; it is not the discovery/schema panel. Typing
  `#` at the start of a line offers the service's sections the hints lack.
  Lines under a `#section` line of Hints are offered only in that section;
  lines before any section are offered where their shape fits.
- Templates opens a library of named functions with scope tabs: per-service
  sections (REST: check, target, action, headers, payload) plus Global (all
  services). Each definition starts
  with `@name(params) description` at the start of a line, or `@name description`
  for no parameters, followed by literal multiline content until the next
  definition. `\@` escapes a leading literal `@` in a body or a section line. A section line
  (`#target @name(...)`, `#check @`) offers only the templates of that section;
  a live call `{{@name(...)}}` may also use the Global ones, and on a name clash
  the section's template wins. Definitions autosave as typed, errors included: the
  status shows the first error, and a definition with an error (or a repeated
  name) is skipped while the others keep working.
- Parameters: `name`, `name: domain`, `name = "default"` or
  `name: domain = "default"`. A domain is one `{{...}}` reference evaluated as a
  list (for example `{{:services}}`, `{{.*.type}}`) or an inline list
  `"a" | "b"`; it only provides completion options. A `{{param}}` inside a reference
  domain is replaced by the argument already typed for that parameter (a `:path`
  argument by its value), for example `env: {{.data.{{group}}~}}`; while it is not
  typed the domain offers nothing. Example:
  `@url(service: {{:services}}, type = "qa") Service URL`, body
  `https://{{service}}.{{:base_domain}}/{{type}}`.
- In a body `{{param}}` is replaced by the argument and `{{@other(args)}}` is
  expanded; every other reference (`{{:x}}`, `{{.var.x}}`) stays in the text and
  resolves at send time. `\{{` anywhere (bodies, sections, variable values) is a
  literal `{{`: it is neither substituted nor resolved, and the `\` is dropped on
  send. A parameter is substituted inside other references too, and references
  nest: `{{.tree.{{a}}.{{b}}}}` or `{{.a.{{:b}}}}` builds the path from
  the inner references first, then resolves it. Arguments are comma separated: a quoted literal
  `"text"` (escapes `\"` and `\\`), a bare identifier (a parameter of the
  enclosing template), or a variable reference `:path` / `.path` (kept live as
  `{{:path}}`). Trailing arguments with defaults may be omitted; missing required
  or extra arguments are errors. Nesting is cycle-checked, depth limit 16.
- In a request, type `@` after a section marker (for example `#target @`) to
  search visible templates by name and description; choosing one inserts `@name(`
  and argument completion offers the parameter's options as quoted literals. A
  `@word` typed by hand and never accepted stays plain text. Press Enter at the end
  of a section line with a complete call (`#target @url("checkout", "qa")`) to
  expand it: the call stays on the section line as metadata (it is not sent) and the
  section body is silently replaced by the result. Editing the arguments and
  pressing Enter again regenerates the body, overwriting manual edits. Only Enter
  triggers this. `{{@name(args)}}` inside a body is a live call: kept in the text
  and expanded at send time. A check template without parameters is inserted whole
  by `#check @name`; one that does not start with a check header gets
  `#check <description>` (or the template name) added. A check template with
  parameters works like a section call: `#check @name` completes to
  `#check <description> @name(` and then the arguments, and Enter replaces the whole check (up to the next check) with the
  expansion, dropping the template's own check header; the call line stays the
  check's header and Enter regenerates it. Templates
  remain global when switching Console sessions, including session 0, unlike Hints.
  When editing, preserve section-line calls, `{{@...}}` calls and references
  instead of inlining their results.
- `{{$name(args)}}` (experimental) calls a jq function from the Functions panel
  (`def name(a; b): ...;` text per service plus Global; the service's defs follow
  the Global ones, so they win on a clash). The input `.` is the whole variable
  tree; arguments use the template call syntax and become JSON: a literal is a
  string, a `:path` / `.path` that selects one string is that string, a hash is an
  object, several matches are an array. String outputs are inserted as is, other
  outputs as compact JSON, several outputs joined by newlines; the result is data
  (not resolved again) and JSON-escaped in JSON bodies. A result containing a
  secret value is masked in History. Inside a template body a bare parameter
  argument (`{{$f(param)}}`) is bound like `{{@other(param)}}`, and a parameter
  domain may be `{{$f(earlier)}}`: its list output (an array's items) gives the
  options once the earlier parameter is typed. The Functions panel has a tab for
  the current service and a Global tab, saves every edit into the Local source's
  `functions.json` (`{"<service>" | "*": defs}`) even when it does not compile, and
  shows the first error in its status with the tab and line. In `#options`,
  `Transform: {{$name(args)}}` runs a function on the run's result instead (`.` is the
  data the service is about to show); see Console options in the services topic.
  Inside the arguments completion shows only the signature (`target($s; «$e»; $n)`,
  the current parameter marked). `{{$!name(args)}}` is an eager call: when a template
  expands (`#section @name(...)` or `#check ... @name(...)` + Enter) and on Enter at
  the end of a console line holding it, it is replaced by its result (inserted
  verbatim: `{{...}}` in the result stay references). A failing call, unresolved
  argument or a result holding a secret value replaces nothing and shows the error
  (use `{{$name(args)}}` for secrets); left in the text it resolves at send time
  like `{{$name(args)}}`.
- Variables contains the session's variables and the read-only Environment.
  Dictionary contains named sets. Read the variables topic before explaining their behavior.
- The main status strip and Debug belong only to the highlighted check in the
  selected service. Switching checks restores that check's own last operation.
  The right tool panel has an independent status and Debug for its selected
  tool/set/chat; Agent activity never replaces the main request status.
  console_read_debug reads the same status and Debug for any check by ID
  (credentials masked, long values clipped, the response payload left to
  console_read_response). Status and Debug are kept in memory only: after the
  Console is reopened or reloaded, a check has none until it runs again.
- Errors with an owner are local: they show in that owner's status and Debug and
  light its check, tab or button until seen on screen. Pending Agent approvals
  and failures to cancel Agent requests show in the Agent panel, module list
  errors in Settings, failed session saves and layout writes in Sessions.
- Errors without a status (Import, Export, Stop, Shift+Enter outside a check,
  startup) are global: a top banner that stays until Dismiss. A failed Import or
  Export also lights that service's Import or Export button until Dismiss.

Do not invent a Discover, Discovery or Schema button in the REST interface.
Read the discovery topic for schema acquisition.

## Statuses and signals

Each status ends with a small red S button. The main S stops only the operation
shown for the highlighted check, including discovery if that is what the strip
shows. The Agent S stops only the selected chat and requests it started in any
service. Neither S stops independent user requests or other chats.
Switching a service/check/chat restores its own status and Debug.
Check, service, chat and set tabs and tool buttons have centered 12px edge
signals: left on the check rail, right on the tool rail, top on horizontal tabs.
Black means ready or viewed, blinking green means running, green means success,
red means error, and yellow means stopped, warning or awaiting approval.
Viewing the matching check/set/chat for at least 0.5 s acknowledges its
non-running signal, not its status or Debug; a shorter glance leaves it lit.
Running signals never go dark while work continues.
New background outcomes light the signal again. Service and tool signals
aggregate running work and unviewed outcomes, prioritizing running over error,
yellow and success. Opening a service does not acknowledge its other checks.
Variables and Dictionary independently aggregate all their sets, including
background saves and errors with the panel closed. Configuration autosaves can
be stopped before dispatch, not rolled back after a local storage write is sent.

## Running and stopping checks

Different checks can run concurrently, including long-lived Terminal commands.
Re-running an active check cancels its current request, rejects the new run, and
shows a local error in that check's status and Debug, without a global banner.
Other checks and chats keep running. Agent-panel Stop or closing a
chat also cancels that chat's child requests and revokes pending approval.
Hiding the Services DevTools tab, closing DevTools, or closing the application
page cancels all operations owned by that window, including background services
and Agent chats. Switching services or hiding just the Agent panel does not.
MySQL cancellation uses KILL QUERY on the executing connection. Cancellation
does not roll back committed changes. For HTTP APIs, stopping the client
request does not guarantee that the remote server stops processing it.

## Console sessions

The current Console session name is above the right rail. Sessions opens a
numbered table with Switch, Delete and Create from actions and a new-name field.
Session 0 is temporary, cannot be deleted, and creating from it always makes an
empty named session. Copies activate immediately. Named sessions autosave checks
and responses in every service, var, hints and invalid drafts.
Dictionary and real env are global. Agent chats belong to the session where
they were opened; Create from does not copy them, deleting a session closes them.
Session 0 chats are shared by session 0 of every window.
Each new window/DevTools opening starts in empty session 0; users explicitly
select saved workspaces. Old data is preserved in Imported on first migration.
Stop active requests/Agent turns and let configuration saves finish before
switching, deleting or copying sessions. Deleting the active session returns to
0. Temporary work lasts only for the current application instance.

Named Console sessions use browser IndexedDB, including large inline responses.
Old localStorage sessions migrate automatically; their source is removed only
after a successful commit. Wait for the Sessions saving signal to finish before
switching. A storage error means the visible edits may not be saved: export
them before switching or reloading. Session storage is separate from the
companion's SQLite execution History.

## History

History shows executions for the selected check, backed by one global SQLite store in the companion's data directory.
Stable check IDs survive rename, parameter edits, moves and undo/redo. Copying
a check or Console session creates new identities and separate history.
Each execution captures resolved request sections and its
own response, status and terminal output. Snapshots are immutable, read-only,
and paged; opening one never re-resolves current Variables or Dictionary values.
Passwords, tokens, authorization, cookies and known sensitive-variable values
are masked in the request; ordinary variables remain resolved. Response bodies
and terminal output are preserved verbatim, including any credentials or
application-owned debug fields returned by the service. Transport debug wrappers
and duplicated effective parameters are omitted. Request masking cannot discover
every secret in arbitrary application data.
The global logical budget is 100 MiB across all checks/services. Crossing it
purges oldest completed entries toward 50 MiB, retaining the new entry; a
snapshot over 100 MiB is rejected explicitly, never silently truncated.
History storage failures do not fail or repeat the actual request and are
reported separately in the History indicator.
A repeating check (`Repeat:` in `#options`) records one execution when the
repeat ends: its last run, timed from the first start.

## Keyboard and editing

In the service editor, Shift+Enter runs the active check; on a running check it
cancels the run, and on a check waiting to repeat it stops the repeat. The visible cursor
selects its check, even if multiple checks fit in the viewport. Scrolling keeps
that selection until the cursor leaves view, then the top visible editor line
selects the check; revealing the cursor restores its selection. Numbered check
tabs also move the cursor. Execution, status, Debug, Stop, export and Agent
context use the same active check.
Enter inserts a newline. Ctrl+Space requests completions. Ctrl+Q toggles a fold
at the cursor. Tab indents a selection or inserts two spaces.
Alt+Space requests discovery for services that have it.
These are the application's bindings; do not substitute Command for Control
when explaining Ctrl+Space on macOS.

Agent edits use the same editor history and can be undone normally. The words
an edit changed briefly flash and fade so the user sees what changed. Check IDs
and revisions come from console_context, not the left rail's display numbers.

## Settings

Settings has the tabs Modules, Sources and Agent; without the native host only
Agent is shown. Settings → Agent holds the defaults for new Agent chats; read the
`agent` topic.
Settings → Modules is a table with a row per module and source: installed and
newest versions with Install, Update and Remove. Its first row is the
application itself, updated the same way and never removed.
Settings → Sources lists named sources, GitHub repositories or local folders,
that supply modules and shared Dictionary sets, templates, hints and skills.
The application checks them automatically; the user applies a source's changes
and installs its modules by button. Later sources override earlier ones and the
user's own changes override them all: a Dictionary variable, a template section
or a module's hints changed by the user shows the source it overrides, and
Reset returns it to the source's. A source's set can't be renamed or deleted.
