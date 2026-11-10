# Splunk module release notes

## 4.0.0

- A WebAssembly module for Services DevTools 4.0; it needs that version or newer.
- `#endpoint` is `#target` and `#search` is `#payload`. Saved checks, templates and
  hints are not converted: rename their sections by hand.

## 3.2.2

- The Agent can load the module's skill, `module-splunk`. Its description was not valid
  YAML, so Copilot skipped the skill and answered "Skill not found".

## 3.2.1

- Settings → Modules shows the Splunk module's name and description before it is
  installed.

## 3.2.0

- The Splunk module is released apart from Services DevTools: install and update it in
  Settings → Modules. It works with Services DevTools 3.2.0 or newer.
- The module brings its Agent skill, `module-splunk`, with its request syntax and examples.
