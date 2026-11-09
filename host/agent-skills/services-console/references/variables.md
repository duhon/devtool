# Variables and Dictionary

These are different panels (modules) and right-side tools, not two names for one
editor.

- Variables owns the session's own variables (`var`, writable) and the read-only
  Environment (`env`, the user's login-shell environment from the companion, never
  persisted).
- Dictionary owns named sets of variables (`dev`, `stage`, …), with create, rename,
  delete and order controls. Existing names such as `store` are examples, not
  guaranteed to exist in the user's application. Sets from a source can't be
  renamed or deleted; the user's changes to them show the source and can be reset.
- Both feed completions and cross-set references.
  Editor values are Name: value lines, not JSON.

References are `{{var:NAME}}`, `{{env:NAME}}`, `{{<set>:NAME}}` for an explicit
owner, or `{{:NAME}}` for a bare name. Bare-name lookup uses Variables, then the
Dictionary sets in order, then the environment. The App resolves them before a
request reaches the companion. Preserve references rather than replacing them with
resolved secrets. Read console_context for available reference names; values of
environment variables are never exposed through Agent context. Do not infer or
recover them.

Variables are part of the Console session; Dictionary is shared by every session
and window. There is currently no Agent tool to create or change variables or
sets. Direct the user to Variables or Dictionary instead of claiming a change was
saved.

Valid edits autosave. Invalid drafts are retained and reported without replacing
valid stored values. A Dictionary CONFLICT status means another window saved the
set after the draft started; the draft is kept until the user discards it to load
the saved version. Connection/authentication references must resolve before a
request or discovery can use them.
Known credentials are masked in model context; arbitrary business data is not
guaranteed anonymous. The embedded Agent receives Console context automatically.

Execution approval is bound to the exact check revision and variable snapshot.
Changing a referenced variable while approval is pending requires a new approval;
never tell the user that an earlier approval still covers a changed target.
