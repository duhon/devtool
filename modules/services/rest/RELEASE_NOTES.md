# REST module release notes

## 4.1.0

- `#headers` is `#metadata`; `#headers` still works but is not offered.

## 4.0.0

- A WebAssembly module for Services DevTools 4.0; it needs that version or newer.
- `#endpoint` is `#target` and `#body` is `#payload`. Saved checks, templates and
  hints are not converted: rename their sections by hand.
- `#target` holds only the URL. The HTTP method goes in `#action` and is GET
  without it; a method left in `#target` is an error that points to `#action`.
- The Agent reads the schema the module discovered in full, not a short summary.

## 3.2.2

- The Agent can load the module's skill, `module-rest`. Its description was not valid
  YAML, so Copilot skipped the skill and answered "Skill not found".

## 3.2.1

- Settings → Modules shows the REST module's name and description before it is
  installed.

## 3.2.0

- The REST module is released apart from Services DevTools: install and update it in
  Settings → Modules. It works with Services DevTools 3.2.0 or newer.
- The module brings its Agent skill, `module-rest`, with its request syntax and examples.
