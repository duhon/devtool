# Functions panel release notes

## 1.0.0

- Experimental. New Functions panel: jq definitions (`def name(a; b): …;`) of the current service and a Global
  tab, called from requests, templates and variables by `{{$name(args)}}` with the variable tree
  as input; the service's definitions follow the Global ones.
- Inside `{{$name(` the completion shows the function's signature with the current parameter
  marked, and suggests variables after `.` / `:`, functions after `$` and templates after `@`.
- Arguments are separated by commas; a bare `$other(args)` argument passes that function's JSON
  value and a bare `@template(args)` its expansion.
- `{{$!name(args)}}` inserts the function's result into the text when a template expands or on
  Enter at the end of its line; results holding a secret stay calls (use `{{$name(args)}}`).
- Every edit is saved into the Local source's `functions.json`, even with an error; the status
  shows the first error with its tab and line.
- Needs Services DevTools 6.0.0.
