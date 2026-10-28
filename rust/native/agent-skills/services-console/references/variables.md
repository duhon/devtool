# Variables and Dictionary

These are different application services and right-side tools, not two names
for one editor.

- Variables owns the fixed writable `var` profile and the read-only live `env`
  profile. Environment is provided by the companion and is never persisted.
- Dictionary owns all other named profiles, with create, rename, delete and
  order controls. Existing names such as `store` are examples, not guaranteed
  to exist in the user's application.
- Both expose a merged context for completions and cross-profile references.
  Editor values are Name: value lines, not JSON.

References are `{{profile:NAME}}` for an explicit profile or `{{:NAME}}` for a
bare name. Bare-name lookup uses var, then Dictionary's profile order, then env.
Preserve references rather than replacing them with resolved secrets.
Read console_context for available reference names; values of environment
variables are never exposed through Agent context. Do not infer or recover them.

The companion stores Variables in `.variable-profiles.json` and Dictionary in
`.dictionary-profiles.json` under the configured application data directory.
These filenames explain persistence; the Agent must not read or edit them.
There is currently no Agent tool to create or change profiles. Direct the user
to Variables or Dictionary instead of claiming a profile was saved.

Valid edits autosave through their own service. Invalid drafts are retained and
reported without replacing valid stored values. Connection/authentication
references must resolve before a request or discovery can use them.
Known credentials are masked in model context; arbitrary business data is not
guaranteed anonymous. The embedded Agent receives Console context automatically.

Execution approval is bound to the exact check revision and variable snapshot.
Changing a referenced profile while approval is pending requires a new approval;
never tell the user that an earlier approval still covers a changed target.
