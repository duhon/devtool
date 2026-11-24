# Templates panel release notes

## 5.0.0

- Templates are functions: `@name(param: {{:services}}, type: "qa" | "stage" = "qa") Description`.
- New "Global" tab: templates every live call `{{@name(...)}}` may use, in any service. A section line (`#target @name(...)`, `#check @`) offers only the templates of its section.
- A literal leading `@` is now written `\@` (was `@@`), in template bodies and on section lines.
- `\{{` is a literal `{{` everywhere (template bodies, requests, variable values): not substituted, sent without the `\`.
- References nest: `{{.tree.{{a}}.{{b}}}}` substitutes template parameters inside the path, and `{{.a.{{:b}}}}` resolves the inner variable first, then the built path.
- A parameter domain may depend on earlier arguments: with `@t(a: {{.tree~}}, b: {{.tree.{{a}}~}})`, `b` offers the keys under the `a` that was typed, and nothing until it is.
- Check templates with parameters: `#check @name` becomes `#check <description> @name(` and asks for the arguments, and Enter replaces the whole check with the expansion; the line stays as the check header, and Enter again regenerates the check.
- No more drafts and Discard draft: every edit is saved as typed. The status shows the first error; a definition with an error is skipped and the others keep working.
- Each template has its own alternating background; a reference in a definition that offers no options is shown in red.
- Needs Services DevTools 6.0.0.

## 4.0.0

- The Templates panel as a WebAssembly module for Services DevTools 4.0.
- Saves into the Local source; Settings → Sources syncs it with shared sources.
