# GraphQL module release notes

## 4.0.0

- A WebAssembly module for Services DevTools 4.0; it needs that version or newer.
- `#endpoint` is `#target` and `#query` is `#payload`. Saved checks, templates and
  hints are not converted: rename their sections by hand.
- The Agent reads the schema the module discovered in full, not a short summary.
- Object fields are marked again, by syntax and by the discovered schema.

## 3.2.2

- The Agent can load the module's skill, `module-graphql`. Its description was not valid
  YAML, so Copilot skipped the skill and answered "Skill not found".

## 3.2.1

- Settings → Modules shows the GraphQL module's name and description before it is
  installed.

## 3.2.0

- The GraphQL module is released apart from Services DevTools: install and update it in
  Settings → Modules. It works with Services DevTools 3.2.0 or newer.
- The module brings its Agent skill, `module-graphql`, with its request syntax and examples.
