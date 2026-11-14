# Release notes

## Unreleased

- The `#headers` section is `#metadata`: everything that travels with the request (HTTP
  headers, gRPC metadata, authentication, certificates, a Splunk search's `Search-*`
  parameters, Artemis `Message-Persistent`, Valkey `CA-PEM`, Terminal's former `#env`). HTTP services still accept
  `#headers` without listing it. `#options` is for the check itself: `Timeout`,
  `Repeat`, limits, `Format` and `Columns-*`. Saved checks of the other services are not
  converted.
- `Timeout` is a console option for every service: it stops the run like Stop.
- Hints in `#options` are marked `g` (console), `m` (module) or `u` (saved by you).

## 4.2.0

- New MongoDB service module (1.0.0): checks with `#target`, `#collection`, `#action`
  and `#payload`. The payload is strict JSON; completion works while you type: commands,
  stages, operators, field names and values sampled from the collection, with commas
  inserted for you. Discovery reads collections and fields from the server. Add it in
  Settings → Modules.
- New Valkey service module (1.0.0) over a TCP session.
- The editor colors checks the same after scrolling or reloading as after a full pass.
- The hints list returns under the caret when filtering shortens it.

## 4.1.0

- The TCP driver (1.1) keeps a connection open as a session: a module writes to it step
  by step and reads replies as they come, so it can wait for one reply before the next
  command. `caPem` adds a CA to trust, and the server name is still checked.
- A new MongoDB driver runs commands on a server from a `mongodb://` or
  `mongodb+srv://` URI and reads cursors in batches. Server errors come back as
  answers with their code; `timeoutMs` is sent to the server as `maxTimeMS`.

## 4.0.1

- Settings → Sources is a table: name, last update, status and actions per source, the
  location under the name. Click a status to see what changed.
- Each GitHub source has its own token: enter it when adding the source, change or clear
  it with **Edit**. The shared token of 4.0.0 is gone; enter it again on each GitHub
  source.
- **Edit** renames a source; the ↑/↓ order buttons are gone, the order changed nothing.
- A new source is a folder by default.

## 4.0.0

- Every service and side panel is now a WebAssembly module. After the update, open
  Settings → Modules and update every module: REST, GraphQL, gRPC, MySQL, Splunk and
  Terminal 4.0.0, Artemis 5.0.0, OpenSearch 2.0.0 and the panels 4.0.0. Modules of
  3.x do not load in 4.0.
- Request sections have new names in every service: `#endpoint` is `#target`, and
  `#query`, `#message`, `#body`, `#command` and `#search` are `#payload`. REST-like
  services keep only the URL in `#target`; the HTTP method goes in `#action` and is
  GET without it. Saved checks, templates and hints are not converted: rename their
  sections by hand. A method left in `#target` is an error that points to `#action`.
- Settings → Sources has a Local source, its first row. Panels save the Dictionary,
  templates and hints into Local. **Sync** on a source's row merges it with Local file
  by file, both ways: a folder source gets the files written, a GitHub source a commit.
  A change made on both sides opens in the new **Diff** panel to resolve. Local
  changes kept over a source by 3.x, and **Reset to source**, are gone.
- The Transfer panel holds Export and Import of the service's command; Export resolves
  the check's variables. Results and errors stay in its status line.
- The Sessions panel is a table: rename a session in place, create one from another,
  see when each was last used. The side panel stays open when you switch sessions.
- The Console has its own editor instead of CodeMirror.
- Services start sooner: modules start from the list cached by the last run, in
  parallel, the active service first.
- The Agent reads the schema a service discovered (GraphQL types and fields, REST
  paths) in full, page by page, instead of a short summary.
- GraphQL marks object fields again, by syntax and by the discovered schema.
- Panel toolbar lights stay on until you have seen the outcome for 2 seconds.
- A darker background; panels have the editor's background, table headers their own.
  History rows no longer move when you select one.

## 3.7.0

- Find and Replace in every editor. ⌘F (Ctrl+F) opens a find bar in the editor of the
  panel holding focus: the Console, Variables, Dictionary, Templates, Hints, History or
  Export. The bar names that editor ("Find in Console · REST") and searches only it.
  DevTools no longer opens its own search bar for these keys.
- ⌥⌘F (Ctrl+H) adds Replace. Enter, ⇧Enter, ⌘G, ⇧⌘G and F3 move between matches;
  ⌥C, ⌥W and ⌥R toggle Match Case, Whole Word and Regular Expression; Escape closes.
  A regular expression replacement takes `$1` and `$<name>`.
- Replace and Replace All never change Console responses: Replace All reports how many
  matches it left there, and one Undo reverts it. Read-only editors offer Find only.
- Large Console documents stay fast: 20,000 replacements take under half a second, and
  so does their Undo.

## 3.6.0

- `Repeat` in `#options` takes a time with its unit: `Repeat: 5s`, `2m` or `1h`, from 1
  second to 24 hours. A bare number such as `Repeat: 5` is an error with an example.
  `Repeat-Until: changed` replaces `Until: changed`, and completion offers it only under
  a `Repeat` line. The Console status shows the interval in its unit: "next in 2m".
- History shows a check's response the way its `Format` asked for: `vertical` rows or a
  JSON array, as in the Console. Before, History always showed a table.
- Settings → Agent shows the saved Reasoning after a restart; before, it showed the
  model's highest level while chats used the saved one. Reasoning is now a slider with
  one step per level the model supports.
- OpenSearch is a standard module: install it from the Standard source in Settings →
  Modules. Version 1.0.0 replaces the separate OpenSearch 0.1.x package, and saved
  checks and Hints stay.
- The Agent now loads the skills of the standard modules: their descriptions were not
  valid YAML, so Copilot skipped them and answered "Skill not found". Update the modules
  in Settings → Modules (Artemis 4.0.1, the others 3.2.2). A module or source whose skill
  frontmatter is not valid YAML is now refused with the line of the error.

## 3.5.0

- Every service has `#options` with three Console keys. `Repeat: 5` runs the check
  again 5 seconds after each response; `Until: changed` stops it once the response
  differs from the previous one. A repeat also stops on Stop, Shift+Enter on the check,
  an edit of the check or a hidden panel. History keeps one entry per repeat: its last
  run.
- `Format: table`, `vertical` or `json` in `#options` of MySQL, Splunk and Artemis
  checks chooses how the Console shows the rows; `json` shows them as an array of
  objects.
- Lines saved under a `#section` line of Hints are offered only in that section, and
  they are added to the module's own suggestions for it. Completion offers only what
  Hints, the module and discovery provide, with no hidden built-in list.

## 3.4.0

- The Artemis module 4.0.0 has a new request format: `#endpoint` is the broker,
  `#action` is the command with its queue and clauses
  (`BROWSE /DLQ WHERE _AMQ_ORIG_ADDRESS = 'orders' LIMIT 20`), and `#headers` are HTTP
  headers. It adds ADDRESSES, CONSUMERS, PUBLISH, PURGE, MOVE, RETRY, PAUSE, RESUME and
  JOLOKIA, and completion offers the broker's queues. Update the module in Settings →
  Modules; it needs this version.
- Completion offers saved headers only in header sections, and a service's keywords
  only where they fit: REST methods at the start of `#endpoint`, Artemis commands at the
  start of `#action`, SPL words in a Splunk search. Before, `Authorization:` or `QUEUES`
  could be offered in a JSON body.

## 3.3.0

- Modules no longer need GitHub releases. A GitHub source is read like a folder: its
  `modules/*.module.tgz` files in a branch and folder you choose, and Settings → Modules
  offers the newest version of each module. A check lists the folder through GitHub's API
  and downloads a package only when its file changed, so checks stay fast with many
  versions. The README describes where a team can keep its packages: a folder in the main
  branch, a separate branch or a separate repository.
- The Standard source now reads the `modules` branch of `duhon/devtool`, which holds the
  newest package of each standard module; a saved Standard source moves to it by itself.
  Checking for a new application reads only the latest release.
- The session name next to the Console tabs is a centered blue label instead of a
  button-like box.
- The Delete button of a chat in the Agent's `#` list is no longer cut off.
- The Agent's guide to the application is split into smaller topics, so the Agent reads it
  whole; before, the guide was cut short and the Agent could give a wrong link to open the
  application standalone.
- The README explains how a source's Agent skills are named and how to write their
  descriptions so the Agent picks them.

## 3.2.2

- Agent chats belong to Console sessions: each session shows its own chats. The `#` tab,
  now on the left, lists the chats of every session with the session, the number of
  messages and the last activity, newest first. An open chat has Close, a closed one has
  Restore, which brings it into the current session, and Delete removes a chat at once,
  without asking. Deleting a Console session closes its chats. Chats from before this
  version are in the Temporary session.
- Settings has tabs: Modules, Sources and Agent, each with its own status light. The tab
  you chose is remembered.
- Hints: typing `#` at the start of a line offers the service's sections that the hints
  don't have yet.
- Settings → Modules keeps its columns in place when a long description is opened.
- The Export, Import and Hints buttons are named without trailing dots.
- The README shows how to open the application standalone in its own tab.

## 3.2.1

- Settings → Modules is one table with a row for each module and source: its name, source,
  installed version and the newest version available, with Install, Update and Remove. The
  name shows the description, the source opens its repository or folder, and a version shows
  its release notes. A module that failed to load shows its version in red with the error.
- The application is the first row of the table and updates like a module, from any source
  that offers it, a fork included. The separate Current version section is gone.
- Modules are installed from sources only; Install from file is gone. To install your own
  package file, put it in `modules/` of a folder source.
- The Settings → Agent skills section is gone. Skills that can't be read are a warning in the
  Agent status and are listed in its Debug.

## 3.2.0

- Modules are released apart from the application. The installer now installs the
  application only; add the services you need in Settings → Modules, which lists the module
  versions available with their release notes and offers Install and Update. An application
  update no longer reinstalls modules, and a module can be updated without one. Modules
  installed before stay as they are; update them there to give the Agent their skills.
- Sources: Settings → Sources keeps an ordered list of named sources, GitHub repositories
  (private ones use a token saved in Settings or `gh auth token`) and local folders, that
  supply modules and shared Dictionary profiles, templates, hints and Agent skills. Standard,
  this repository, is there from the start. Sources are checked automatically; Settings shows
  what applying one would change, including what you've changed yourself, and you apply it by
  button.
- Your changes stay on top: later sources override earlier ones and your own changes override
  them all, also after a source updates. A Dictionary profile, template section or service
  hints from a source show their source, and Reset puts your change back to the source's; in
  the Dictionary you can reset one variable or the whole profile.
- Agent skills come from modules and sources: each module brings its own skill
  (`module-<id>`), each source its skills (`<source>-<name>`). Settings → Agent skills lists
  them with where they come from. The Team skills folder setting is gone: a folder named
  `skills` becomes the "Team" source automatically; otherwise Settings → Sources explains how
  to add it.

## 3.1.0

- New Artemis service for looking into Apache ActiveMQ Artemis through the broker's web console
  (Jolokia), with the login in the URL: `http://user:password@host:8161/<queue>`. QUEUES lists
  queues with their message, consumer and delivery counters; BROWSE shows messages with their
  properties without consuming them, optionally by a JMS selector; SEND puts a text message on
  a queue; RECEIVE removes messages from a queue and returns them. Bodies the broker cuts over
  management are flagged, and RECEIVE leaves such messages in the queue.
- The STOMP service is removed: as a client protocol it could not browse queues or show what a
  broker holds. Saved STOMP checks no longer run; use Artemis instead.
- The Agent can read a check's Debug: the new `console_read_debug` tool returns the check's
  status and the same Debug you see (from its last run or schema discovery), with secrets masked
  and long values cut. When a check fails or warns, the Agent reads its Debug to find out why.
  Debug lives only in the open panel, so it is gone after a reload.

## 3.0.5

- Agent edits flash: the words the Agent changes in a service's checks are highlighted and fade
  out over about a second, so you can see what it did.
- Terminal outcomes follow the exit code: any non-zero exit code or termination by a signal is
  now ERROR (before, only exit code 1 was; others such as 127 "command not found" were
  WARNING). Commands such as `grep` without matches or `diff` with differences are ERROR too.
- A Terminal command that exits 0 but writes to stderr is WARNING, and its status shows the last
  stderr line.

## 3.0.4

- Agent skills from a team folder: Settings → Agent skills takes a local Team skills folder
  (for example a team repository checkout or a synced drive) with one `<name>/SKILL.md` folder
  per skill. Settings lists the skills found and explains the ones it skipped; a rejected path
  marks the field red. Every following Agent reply can use these skills; used skills show as
  `Skill: name` activity and in the Agent status Debug. The Agent reads only the `SKILL.md`
  text, not other files of a skill.
- Closed Agent chats: the Closed (N) tab lists closed chats, newest first. Restore reopens a
  chat with its history; Delete and Delete all remove it together with its Copilot state.
  Chats still running in this or another DevTools window are kept and reported.
- Terminal: a failed command shows `exit N` (or the signal) and the last stderr line in its
  status; Debug keeps the last 4 KiB of stderr.
- Dictionary no longer silently overwrites a profile saved meanwhile in another DevTools window
  or by hand: the save reports a conflict and keeps the draft, and Discard draft loads the
  saved version. Dictionary rereads its profiles when you return to the panel or the window.
- An error counts as seen only after it has been on screen for half a second, so switching
  away right as a reply arrives keeps the panel's button lit.
- Templates drafts of a removed module no longer make Templates discard every draft; they are
  kept and work again once the module is reinstalled.

## 3.0.3

- New STOMP service for Apache ActiveMQ Artemis (STOMP 1.0–1.2 over TCP or TLS). A check uses
  `SEND|SUBSCRIBE stomp://host:61613/destination` with frame headers, options (Count, Wait,
  Client-Id, Host, Columns) and a body for SEND. SEND waits for the broker's receipt.
  SUBSCRIBE consumes and acknowledges each returned message; messages beyond Count stay in the
  queue. Stop returns the messages already received; Debug lists the frames with the passcode
  hidden.
- Errors with an owner show in its status and Debug and light its check, tab or button until
  seen: Agent approvals and stop failures in the Agent panel, failed session saves in Sessions.
  The top banner is kept for errors without a status (Import, Export, Stop) and stays until
  Dismiss; a failed Import or Export also lights that service's button.
- Removing a module in Settings can also delete its data in every session (consoles, checks,
  responses, history, templates, hints and Templates drafts). By default the data is kept.

## 3.0.2

- Sessions no longer stay blocked as busy after the last native request finishes; the Sessions
  panel re-renders as soon as the transport becomes idle.
- Stopping an Agent turn waits for Copilot to confirm the stop, so a late reply of the stopped
  turn can no longer end the next turn of the same chat. If the confirmation does not arrive
  within 2 seconds, the result carries a warning.
- Dictionary and Variables always save an edit 600 ms after the last keystroke. Reopening the
  panel no longer saves the draft being typed as soon as the profiles reload, so a validation
  error that arrives after you switch away lights the tool button.

## 3.0.1

- Settings no longer hides an unseen warning or error, such as a failed background update
  check, behind a newer successful operation like loading the Copilot models.

## 3.0.0

- The Console is the only interface. The per-service forms, service picker and O button are
  removed; tabs saved by the old interface are not carried over.
- Removed the standalone HTTP interface (`services-devtools web`); the application runs only
  as the Chrome extension.
- Services are modules: REST, MySQL, GraphQL, gRPC, Splunk and Terminal ship as packages in
  `~/Library/Application Support/ServicesDevTools/data/modules` and load through the same
  module API as modules written by anyone else. An update replaces only the modules it ships.
- Settings → Modules lists the installed modules, installs a module from its package file
  (`.module.tgz`) and removes one. The console of a removed module is kept.
- Module SDK: `rust/module-api` and `rust/module-ui`, the example module and guide in
  `rust/examples/echo`, and `cargo run -p xtask -- package` to build package files.

## 2.6.7

- Removed the Refresh models button from Agent settings.

## 2.6.6

- Choose model and reasoning for new Agent chats in Settings; Copilot Fusion by default.

## 2.6.5

- Send with Enter; remove agent hints and author labels.

## 2.6.4

- Add full DOM inspection and per-page write approval.

## 2.6.3

- Unify Settings styling, status and update diagnostics.

## 2.6.2

- Improve agent page reading, search, context and interactions.

## 2.6.1

- Fix the macOS in-app updater using the signing and quarantine handling personally verified with local 2.5.26.
- After validating an approved archive, locally ad-hoc sign both staged native executables (`services-devtools` and `services-install`), remove only their `com.apple.quarantine`, verify their signatures and absence of quarantine, then check runtime health before activation. Preparing the next installer avoids leaving it quarantined for a subsequent update.
- Keep other files' quarantine metadata, archive/source/version/permission checks, user data and transactional rollback. No global macOS security settings are changed.
- Preserve private diagnostics at `data/update-local-signing.json` and `data/update-installer-signing.json`; signing, quarantine-removal and health failures stop installation instead of activating broken executables.
- Existing 2.6.0 and older installations still use their old installer. Install 2.6.1 manually once and reload the extension, or install the local 2.5.27 bootstrap first and update through Settings, to enable the corrected updater for both executables.
- Releases remain ad-hoc signed, not Developer ID signed or notarized. The updater intentionally removes quarantine from the two verified staged executables; managed-device policies can still restrict execution.

## 2.6.0

- Download approved public GitHub release archives directly through the native Rust runtime; Chrome no longer downloads the archive.
- Verify SHA-256 during download and again before installation, with bounded size, HTTPS redirects and download time.
- Keep downloaded files private and temporary without adding a quarantine marker. Preserve any already-existing macOS quarantine metadata through extraction and installation.
- Remove the extension's downloads permission. Save the workspace before updating, retain rollback checks and reload only after confirmed installation.
- Test the transition using the corrected local 2.5.24 bootstrap configured for duhon/devtool on main.
- This remains an updater preview for personal verification, not a confirmed end-to-end installation. Releases are not Developer ID signed or notarized; Gatekeeper can still block execution and is not bypassed.

## 2.5.23

- Preview for personal verification of the in-app updater; use the local 2.5.22 package as the initial installation.
- Check private GitHub Releases through the current Chrome session and show new versions in Settings.
- Notify about unseen updates with a green Settings indicator.
- Save the workspace, download and verify the approved archive, then install and reload after choosing Update.
- Preserve the existing installation if validation or the new runtime health check fails.
- This preview is not Developer ID signed or notarized. macOS Gatekeeper may block the automatic installation; no system protections are disabled.

## 2.5.21

- Add the application Settings panel with the installed version.
- Keep the selected History execution visible and clearly highlighted.
- Load large History snapshots automatically without a manual load-more button.
- Preserve session-specific panel sizes and text-only selection highlighting.
