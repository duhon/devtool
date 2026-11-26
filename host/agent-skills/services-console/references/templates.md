# Templates and functions

Templates and Functions are tools in the Console's right rail; the layout is the
interface topic, variables and checks as variables are the variables topic.

- Templates opens a library of named functions with scope tabs: per-service
  sections (REST: check, target, action, headers, payload) plus Global (all
  services). Each definition starts
  with `@name(params) description` at the start of a line, or `@name description`
  for no parameters, followed by literal multiline content until the next
  definition. `\@` escapes a leading literal `@` in a body or a section line. A section line
  (`#target @name(...)`, `#check @`) offers only the templates of that section
  (and built-in ones of it);
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
  and expanded at send time. Every check template works like a section call:
  `#check @name` completes to `#check <title> @name` (the template's own check
  title, else its description), or `@name(` and then the arguments while a parameter
  has no default (`@name()` when all have one). Enter replaces the whole check (up to
  the next check) with the expansion, dropping the template's own check header; the
  call line stays the check's header and Enter regenerates it. An eager call whose
  result starts with `#check title` gives an untitled check that title. The built-in
  `@copy(check = "prev")` (every service, not in the Templates panel; see the
  variables topic) copies another check without its response and titles the copy
  `[vN] <title>`. Picking a complete call (no parameters, or all with
  defaults) from the popup expands it at once, as if Enter was pressed (Tab instead
inserts `@name(` and asks for its arguments); a call
  already typed in full is not offered again. Templates
  remain global when switching Console sessions, including session 0, unlike Hints.
  When editing, preserve section-line calls, `{{@...}}` calls and references
  instead of inlining their results.
- `{{$name(args)}}` (experimental) calls a jq function from the Functions panel
  (`def name(a; b): ...;` text per service plus Global; the service's defs follow
  the Global ones, so they win on a clash). The input `.` is the whole variable
  tree; arguments use the template call syntax and become JSON: a literal is a
  string, a `:path` / `.path` that selects one string is that string, a hash is an
  object, several matches an array; a bare `$g(..)` argument is g's JSON value,
  a bare `@t(..)` its expansion; commas split arguments. String outputs are inserted as is, other
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
  In the arguments completion shows the signature (`target($s, «$e», $n)`)
  plus variables after `.`/`:`, functions after `$`, templates after `@`. `{{$!name(args)}}` is an eager call: when a template
  expands (`#section @name(...)` or `#check ... @name(...)` + Enter) and on Enter at
  the end of a console line holding it, it is replaced by its result (inserted
  verbatim: `{{...}}` in the result stay references). A failing call, unresolved
  argument or a result holding a secret replaces nothing and shows the error
  (use `{{$name(args)}}` for secrets); left in the text it resolves at send time.
