# Terminal module release notes

## 5.0.0

- `#env` is `#metadata`: the variables a command runs with travel with it. Saved
  checks, templates and hints are not converted: rename the section by hand.

## 4.0.0

- A WebAssembly module for Services DevTools 4.0; it needs that version or newer.
- `#endpoint` is `#target` and `#command` is `#payload`. Saved checks, templates and
  hints are not converted: rename their sections by hand.

## 3.2.2

- The Agent can load the module's skill, `module-terminal`. Its description was not valid
  YAML, so Copilot skipped the skill and answered "Skill not found".

## 3.2.1

- Settings → Modules shows the Terminal module's name and description before it is
  installed.

## 3.2.0

- The Terminal module is released apart from Services DevTools: install and update it in
  Settings → Modules. It works with Services DevTools 3.2.0 or newer.
- The module brings its Agent skill, `module-terminal`, with its request syntax and examples.
