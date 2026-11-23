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
  Editor values are `Name: value` lines, not JSON.

## Data: strings and hashes

A line `name: value` is a string. `name:` followed by deeper-indented lines is a
hash; numbered keys (`1:`, `2:`) make a list. `name:` with nothing below is an
empty string. Keys are `[A-Za-z0-9_-]+`; a line without `:`, a duplicate key at the
same level or inconsistent indentation is an error with a line number. `env` is
strings only and read-only.

```text
base_domain: site.example
services:
  1: catalog
  2: checkout
envs:
  qa:
    1: catalog
  stage: test
```

## References

All panels form one tree. The root `.` holds each panel as a first-level key in
priority order: `var`, then the Dictionary sets in their order (`dev`, `stage`, …),
then `env`.

- Absolute: `{{.var.region}}`, `{{.dev.host}}`, `{{.env.HOME}}`.
- Relative: `{{:services.2}}` is a path from the root of a panel. Panels are tried
  in priority order; the first where the path gives a non-empty result wins.
- `*` is every child, one level (`{{.*.type}}` is `type` of every panel); `**` is
  any depth (`{{:envs.**}}`); a trailing `~` gives keys instead of values
  (`{{:envs~}}`). A missing key or a step into a string selects nothing.
- `{{name}}` is a template parameter, valid only inside a template body.
  `{{@name(args)}}` is a live template call (see the interface topic).

Contexts decide types. In request text a reference must select exactly one string:
a hash gives `expected a string, got a hash`, several values give
`expected a string, got a list (N)`, nothing gives `Unknown variable …`. In list
contexts (template parameter domains, completion options) a single hash yields its
values and every value must be a string. With the example above, `{{:envs.*}}` is
catalog, `{{:envs.**}}` is catalog|test, `{{:envs~}}` is qa|stage and
`{{:envs.stage}}` is test; `{{:envs}}` as a list is an error (its child `qa` is a hash).

Values may contain references; they resolve recursively with cycle detection and a
depth limit of 16. A value is treated as a secret when any segment of its path is
sensitive (password, token, …). The old `{{var:x}}`, `{{env:X}}` and `{{set:x}}`
forms are no longer supported.

The App resolves references before a request reaches the companion. Preserve
references rather than replacing them with resolved secrets. Read console_context
for available reference names; values of environment variables are never exposed
through Agent context. Do not infer or recover them.

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
