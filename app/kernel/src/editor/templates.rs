//! Template calls in the editors. On a `#section` line `@` lists the visible templates and
//! completes the arguments of the call from their domains; Enter at the end of the line expands
//! the call into the section's body. `{{@name(args)}}` calls in a body complete the same way.
use super::{Candidate, Completion, Services, fuzzy_match, template_sections};
use crate::document::{self, Position, ResponseRegion, is_check};
use services_contracts::templates::{self, Arg, Domain, Library, Template};
use services_sdk::refs::{self, Root};
use std::collections::BTreeMap;

fn name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}

fn utf16(text: &str) -> usize {
    text.encode_utf16().count()
}

fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `name(a, b = "default")`; just the name for a template without parameters.
pub fn signature(template: &Template) -> String {
    if template.params.is_empty() {
        return template.name.clone();
    }
    let params: Vec<String> = template
        .params
        .iter()
        .map(|param| match &param.default {
            Some(default) => format!("{} = {}", param.name, quoted(default)),
            None => param.name.clone(),
        })
        .collect();
    format!("{}({})", template.name, params.join(", "))
}

/// `name` and the text between the parentheses of an unfinished call `@name(args`.
pub fn open_call(text: &str) -> Option<(&str, &str)> {
    open_with(text, '@')
}

/// [`open_call`] of a call that starts with `sigil` (`$` for a function).
pub fn open_with(text: &str, sigil: char) -> Option<(&str, &str)> {
    let rest = text.strip_prefix(sigil)?;
    let end = rest.find(|c| !name_char(c))?;
    let args = rest[end..].strip_prefix('(')?;
    let (mut quote, mut escape) = (false, false);
    for c in args.chars() {
        match c {
            _ if escape => escape = false,
            '\\' if quote => escape = true,
            '"' => quote = !quote,
            ')' if !quote => return None,
            _ => {}
        }
    }
    Some((&rest[..end], args))
}

/// Those of `templates` whose name or description matches `query`.
fn matching(templates: Vec<Template>, query: &str) -> Vec<Template> {
    let query = query.to_lowercase();
    templates
        .into_iter()
        .filter(|template| {
            fuzzy_match(&template.name, &query)
                || template.description.to_lowercase().contains(&query)
        })
        .take(100)
        .collect()
}

/// A live call's templates: `open` + name + `(` (or `close` when it has no parameters).
pub fn name_candidates(
    library: &Library,
    service: &str,
    section: &str,
    query: &str,
    open: &str,
    close: &str,
) -> Vec<Candidate> {
    matching(templates::visible(library, service, section), query)
        .into_iter()
        .map(|template| {
            let call = template.params.is_empty();
            Candidate {
                text: if call {
                    format!("{open}{}{close}", template.name)
                } else {
                    format!("{open}{}(", template.name)
                },
                label: Some(signature(&template)),
                detail: template.description,
                badge: None,
                continue_hint: !call,
                literal: true,
            }
        })
        .collect()
}

/// The values of the arguments typed before the current one (`typed` is the text of the call
/// up to that argument): literals as they are, variables resolved; parameters of an enclosing
/// template and arguments that do not parse have none.
fn typed_values(template: &Template, typed: &str, root: &Root) -> BTreeMap<String, String> {
    let typed = typed.trim_end().trim_end_matches(',');
    let Ok(call) = templates::parse_call(&format!("x({typed})")) else {
        return BTreeMap::new();
    };
    let mut values = BTreeMap::new();
    for (param, arg) in template.params.iter().zip(call.args) {
        let value = match arg {
            Arg::Literal(value) => value,
            Arg::Variable { absolute, path } => {
                let reference = format!("{{{{{}{path}}}}}", if absolute { '.' } else { ':' });
                match refs::resolve(&reference, root, false, &mut |_, _| Err(String::new())) {
                    Ok(resolved) => resolved.text,
                    Err(_) => continue,
                }
            }
            Arg::Param(_) => continue,
        };
        values.insert(param.name.clone(), value);
    }
    values
}

/// `reference` with a nested path (`{{.a.{{:b}}}}`) rebuilt from its inner references.
pub fn built(root: &Root, reference: &refs::Ref) -> Result<refs::Ref, String> {
    let refs::Ref::Dynamic(body) = reference else {
        return Ok(reference.clone());
    };
    let path = refs::resolve(body, root, false, &mut |_, _| Err(String::new()))?.text;
    let text = format!("{{{{{path}}}}}");
    match refs::scan(&text).as_slice() {
        [found] if found.start == 0 && found.end == text.len() => Ok(found.reference.clone()),
        _ => Err(format!("{reference}: `{text}` is not a variable reference")),
    }
}

/// The options of one parameter: its domain (its `{{param}}` replaced by `values`; none while
/// one of them has no value), or its default when it has none; an error when the domain does
/// not evaluate to a list. A function call domain (`{{$name(args)}}`, among `defs`) offers
/// its output.
fn options(
    template: &Template,
    index: usize,
    root: &Root,
    values: &BTreeMap<String, String>,
    defs: &str,
) -> Result<Vec<refs::ListItem>, String> {
    let param = &template.params[index];
    let plain = |value: &String| refs::ListItem {
        key: None,
        value: value.clone(),
    };
    Ok(match &param.domain {
        Some(Domain::Inline(values)) => values.iter().map(plain).collect(),
        Some(Domain::Reference(text)) => {
            let Ok(text) = templates::fill_domain(text, values) else {
                return Ok(Vec::new());
            };
            let Some(found) = refs::scan(&text).into_iter().next() else {
                return Ok(Vec::new());
            };
            if let refs::Ref::Function { .. } = found.reference {
                return super::functions::options(&text, root, defs);
            }
            let reference = built(root, &found.reference)?;
            let shown = reference.to_string();
            let selection = refs::eval(root, &reference)
                .filter(|selection| !selection.items.is_empty())
                .ok_or_else(|| format!("{shown}: unknown variable"))?;
            refs::as_list(&selection).map_err(|error| format!("{shown}: {error}"))?
        }
        None => param.default.iter().map(plain).collect(),
    })
}

/// The argument the cursor is in: its index and where it starts in `args`.
fn current_argument(args: &str) -> (usize, usize) {
    let (mut quote, mut escape) = (false, false);
    let (mut index, mut start) = (0, 0);
    for (at, c) in args.char_indices() {
        match c {
            _ if escape => escape = false,
            '\\' if quote => escape = true,
            '"' => quote = !quote,
            ',' if !quote => {
                index += 1;
                start = at + 1;
            }
            _ => {}
        }
    }
    let skipped = args[start..].len() - args[start..].trim_start().len();
    (index, start + skipped)
}

/// The domain options of the argument being typed in `args` (the text of the call after its
/// `(`, which starts at UTF-16 column `args_col`) as quoted literals. `suffix` is the text after
/// the cursor and `close` ends the call after its last argument.
pub fn argument_completion(
    template: &Template,
    (root, defs): (&Root, &str),
    (args_col, args): (usize, &str),
    suffix: &str,
    close: &str,
    ch: usize,
) -> Option<Completion> {
    let (index, start) = current_argument(args);
    if index >= template.params.len() {
        return None;
    }
    let partial = &args[start..];
    let query = partial.strip_prefix('"').unwrap_or(partial);
    let unquoted = partial.trim_start();
    if let Some(candidates) = super::references::variable_argument(root, unquoted) {
        let from = args_col + utf16(&args[..args.len() - unquoted.len()]);
        return Some(Completion {
            from,
            to: ch,
            candidates: candidates.into_iter().take(100).collect(),
            comma: None,
            automatic: true,
        });
    }
    if matches!(query.chars().next(), Some(':' | '.')) {
        return None;
    }
    let last = index + 1 == template.params.len();
    let named = |item: &refs::ListItem| match &item.key {
        Some(key) if !key.chars().all(|c| c.is_ascii_digit()) => {
            format!("{key} — {}", item.value)
        }
        _ => item.value.clone(),
    };
    let mut seen = std::collections::BTreeSet::new();
    let values = typed_values(template, &args[..start], root);
    let options = match options(template, index, root, &values, defs) {
        Ok(options) => options,
        // The error is shown as the only item; choosing it leaves the text as it is.
        Err(error) => {
            return Some(Completion {
                from: args_col + utf16(&args[..start]),
                to: ch,
                candidates: vec![Candidate {
                    text: partial.to_owned(),
                    label: Some(error),
                    detail: template.params[index].name.clone(),
                    badge: Some('!'),
                    continue_hint: false,
                    literal: true,
                }],
                comma: None,
                automatic: true,
            });
        }
    };
    let candidates: Vec<Candidate> = options
        .into_iter()
        .filter(|item| fuzzy_match(&named(item), query) && seen.insert(item.value.clone()))
        .take(100)
        .map(|item| Candidate {
            text: format!("{}{}", quoted(&item.value), if last { close } else { ", " }),
            label: Some(named(&item)),
            detail: template.params[index].name.clone(),
            badge: None,
            continue_hint: !last,
            literal: true,
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    // Typed closers (an auto-closed quote or parenthesis) are part of what is replaced.
    let mut rest = suffix;
    if partial.starts_with('"') && !partial[1..].contains('"') {
        rest = rest.strip_prefix('"').unwrap_or(rest);
    }
    if last {
        rest = rest.strip_prefix(')').unwrap_or(rest);
        if close.ends_with("}}") {
            rest = rest.strip_prefix("}}").unwrap_or(rest);
        }
    }
    Some(Completion {
        from: args_col + utf16(&args[..start]),
        to: ch + utf16(&suffix[..suffix.len() - rest.len()]),
        candidates,
        comma: None,
        automatic: true,
    })
}

/// Where the call `@name` or `@name(…` starts in the text of a `#check` line, after its title.
fn call_start(text: &str) -> Option<usize> {
    text.match_indices('@').map(|(at, _)| at).find(|&at| {
        let rest = &text[at + 1..];
        let end = rest.find(|c| !name_char(c)).unwrap_or(rest.len());
        (at == 0 || text[..at].ends_with(char::is_whitespace))
            && end > 0
            && (rest[end..].is_empty() || rest[end..].starts_with('('))
    })
}

/// `#name` at the start of `before` (a marker line up to the cursor): the section name and
/// the text after it.
fn marker_prefix(before: &str) -> Option<(String, &str)> {
    let typed = before.trim_start().strip_prefix('#')?;
    let end = typed
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_')))
        .unwrap_or(typed.len());
    let (name, tail) = typed.split_at(end);
    (!name.is_empty() && (tail.is_empty() || tail.starts_with(char::is_whitespace)))
        .then(|| (name.to_ascii_lowercase(), tail))
}

#[allow(clippy::too_many_arguments)]
pub fn suggest(
    services: &dyn Services,
    service: &str,
    field: &str,
    source: &str,
    line: usize,
    ch: usize,
    library: &Library,
    scope: (&Root, &str),
) -> Option<Completion> {
    if !field.is_empty() {
        return None;
    }
    let lines: Vec<_> = source.split('\n').collect();
    let current = *lines.get(line)?;
    let mut units = 0;
    let before: String = current
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= ch
        })
        .collect();
    let (section, tail) = marker_prefix(&before)?;
    if !template_sections(services.markers(service)).contains(&section) {
        return None;
    }
    let call = if section == "check" {
        call_start(tail).map(|at| &tail[at..])
    } else {
        Some(tail.trim_start())
    };
    if let Some((name, args)) = call.and_then(open_call) {
        let template = templates::own(library, service, &section)
            .into_iter()
            .find(|template| template.name == name)?;
        let args_at = before.len() - args.len();
        return argument_completion(
            &template,
            scope,
            (utf16(&before[..args_at]), args),
            &current[before.len()..],
            ")",
            ch,
        );
    }
    let reference = before.rfind('@');
    let (prefix, query) = match reference {
        Some(start) => (&before[..start], &before[start + 1..]),
        None if before.ends_with(char::is_whitespace) => (before.as_str(), ""),
        None => return None,
    };
    if !query.chars().all(name_char) || prefix.trim().strip_prefix('#') != Some(section.as_str()) {
        return None;
    }
    let candidates: Vec<_> = matching(templates::own(library, service, &section), query)
        .into_iter()
        .map(|template| {
            if section == "check" && !template.params.is_empty() {
                let title = if template.description.is_empty() {
                    String::new()
                } else {
                    format!("{} ", template.description)
                };
                return Candidate {
                    text: format!("#check {title}@{}(", template.name),
                    label: Some(signature(&template)),
                    detail: template.description,
                    badge: None,
                    literal: true,
                    continue_hint: true,
                };
            }
            if section == "check" {
                let text = if !template.body.lines().next().is_some_and(is_check) {
                    let title = if template.description.is_empty() {
                        &template.name
                    } else {
                        &template.description
                    };
                    format!("#check {title}\n{}", template.body)
                } else {
                    template.body.clone()
                };
                return Candidate {
                    text,
                    label: Some(format!("@{}", template.name)),
                    detail: template.description,
                    badge: None,
                    literal: true,
                    continue_hint: false,
                };
            }
            let zero = template.params.is_empty();
            Candidate {
                text: format!(" @{}{}", template.name, if zero { "" } else { "(" }),
                label: Some(signature(&template)),
                detail: template.description,
                badge: None,
                literal: true,
                continue_hint: !zero,
            }
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let from = if section == "check" {
        0
    } else {
        utf16(prefix.trim_end())
    };
    let after = &current[before.len()..];
    let (after, reference_length) = match after.strip_prefix('@') {
        Some(suffix) if reference.is_none() => (suffix, 1),
        _ => (after, 0),
    };
    let suffix: String = after.chars().take_while(|c| name_char(*c)).collect();
    Some(Completion {
        from,
        to: ch + reference_length + utf16(&suffix),
        candidates,
        comma: None,
        automatic: true,
    })
}

/// What Enter on a section line holding a complete call does: the lines from `from` to `to`
/// are replaced by `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    pub from: Position,
    pub to: Position,
    pub text: String,
}

/// Enter at the end of `lines[line]`, a `#section @name(args)` line of a console document: the
/// section's body (up to the next marker or response) becomes the call's expansion, its eager
/// calls `{{$!name(args)}}` replaced by their results (`scope`: the variable tree and the jq
/// functions). `None` when the line holds no call, or the body already is the expansion; an
/// error when the call (or an eager call) is wrong, and then nothing changes.
pub fn expand_call(
    services: &dyn Services,
    service: &str,
    lines: &[String],
    line: usize,
    regions: &[ResponseRegion],
    library: &Library,
    (root, defs): (&Root, &str),
) -> Option<Result<Expansion, String>> {
    let current = lines.get(line)?;
    let (section, rest) = document::marker(current)?;
    let check = section == "check";
    if !services.is_service(service)
        || !(check || services.markers(service).contains(&section))
        || regions
            .iter()
            .any(|region| (region.header..region.end).contains(&line))
    {
        return None;
    }
    let rest = rest.trim();
    let rest = if check {
        &rest[call_start(rest)?..]
    } else {
        rest
    };
    if !rest.starts_with('@') {
        return None;
    }
    // The call names a template of the section; the live calls in its body may also name
    // the global ones.
    let visible = templates::visible(library, service, &section);
    let own = templates::own(library, service, &section);
    let result = templates::parse_section_call(rest).and_then(|call| {
        if !own.iter().any(|template| template.name == call.name) {
            return Err(format!("Unknown template @{}", call.name));
        }
        templates::expand(
            &|name| {
                visible
                    .iter()
                    .find(|template| template.name == name)
                    .cloned()
            },
            &call,
            &Default::default(),
        )
    });
    let mut text = match result {
        Ok(text) => text,
        // Without parentheses it is text nobody asked to expand (`@file`, an address).
        Err(_) if !rest.contains('(') => return None,
        Err(error) => return Some(Err(error)),
    };
    // A check's call line is its header: the template's own header is dropped.
    if check && text.lines().next().is_some_and(is_check) {
        text = text
            .split_once('\n')
            .map_or("", |(_, body)| body)
            .to_owned();
    }
    if super::functions::has_eager(&text) {
        text = match super::functions::eager(&text, root, defs) {
            Ok(text) => text,
            Err(error) => return Some(Err(error)),
        };
    }
    let start = line + 1;
    let mut end = (start..lines.len())
        .find(|&at| {
            (if check {
                is_check(&lines[at])
            } else {
                document::marker(&lines[at]).is_some()
            }) || regions
                .iter()
                .any(|region| (region.header..region.end).contains(&at))
        })
        .unwrap_or(lines.len());
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    if lines[start..end].join("\n") == text {
        return None;
    }
    let edit = if end > start {
        Expansion {
            from: Position { line: start, ch: 0 },
            to: Position {
                line: end - 1,
                ch: utf16(&lines[end - 1]),
            },
            text,
        }
    } else {
        let at = Position {
            line,
            ch: utf16(current),
        };
        Expansion {
            from: at,
            to: at,
            text: format!("\n{text}"),
        }
    };
    Some(Ok(edit))
}

/// Enter at the end of `lines[line]` of a console document (outside the responses) holding
/// eager calls `{{$!name(args)}}`: the line with each replaced by its result. `None` when it
/// holds none; an error when one fails or its result holds a secret, and then nothing changes.
pub fn expand_eager(
    (lines, line): (&[String], usize),
    regions: &[ResponseRegion],
    (root, defs): (&Root, &str),
) -> Option<Result<Expansion, String>> {
    let current = lines.get(line)?;
    if !super::functions::has_eager(current)
        || regions
            .iter()
            .any(|region| (region.header..region.end).contains(&line))
    {
        return None;
    }
    Some(
        super::functions::eager(current, root, defs).map(|text| Expansion {
            from: Position { line, ch: 0 },
            to: Position {
                line,
                ch: utf16(current),
            },
            text,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_domain_uses_the_arguments_typed_before_it() {
        let parse = |text| refs::parse_lines(text).unwrap();
        let root = Root::new(vec![
            (
                "tree".into(),
                parse("a:\n  x:\n    1: p\n    2: q\nb:\n  x:\n    1: r\n  y:\n    1: s"),
            ),
            ("var".into(), parse("g: b")),
        ]);
        let header = "@t(g: {{.tree~}}, e: {{.tree.{{g}}~}}, n: {{.tree.{{g}}.{{e}}~}} = \"1\") D";
        let template = templates::parse(&format!(
            "{header}\n{{{{.tree.{{{{g}}}}.{{{{e}}}}.{{{{n}}}}}}}}"
        ))
        .unwrap()
        .remove(0);
        let offered = |args: &str| -> Vec<String> {
            argument_completion(&template, (&root, ""), (0, args), "", ")", utf16(args))
                .map(|completion| completion.candidates)
                .unwrap_or_default()
                .into_iter()
                .map(|candidate| {
                    assert_ne!(candidate.badge, Some('!'), "{:?}", candidate.label);
                    let text = candidate.text.trim_end_matches([',', ' ', ')']);
                    text.trim_matches('"').to_owned()
                })
                .collect()
        };
        assert_eq!(offered(""), ["a", "b"]);
        assert_eq!(offered("\"b\", "), ["x", "y"]);
        assert_eq!(offered("\"a\", "), ["x"]);
        assert_eq!(offered("\"a\", \"x\", "), ["1", "2"]);
        assert_eq!(offered("\"b\", \"x\", "), ["1"]);
        assert_eq!(offered(":g, \"y\", "), ["1"]);
        assert!(offered(", ").is_empty());
    }

    #[test]
    fn a_function_domain_runs_on_the_arguments_typed_before_it() {
        let parse = |text| refs::parse_lines(text).unwrap();
        let root = Root::new(vec![(
            "tree".into(),
            parse("alpha:\n  one: 1\n  two: 2\nbeta:\n  x: 3"),
        )]);
        let defs = "def sub(k): .tree[k] | keys; def bad: error(\"boom\");";
        let template = templates::parse("@t(a: {{.tree~}}, b: {{$sub(a)}}, c: {{$bad}}) D\nx")
            .unwrap()
            .remove(0);
        let offered = |args: &str| -> Vec<(String, Option<char>)> {
            argument_completion(&template, (&root, defs), (0, args), "", ")", utf16(args))
                .map(|completion| completion.candidates)
                .unwrap_or_default()
                .into_iter()
                .map(|candidate| (candidate.label.unwrap_or_default(), candidate.badge))
                .collect()
        };
        let plain = |items: &[&str]| -> Vec<(String, Option<char>)> {
            items.iter().map(|item| (item.to_string(), None)).collect()
        };
        assert_eq!(offered("\"alpha\", "), plain(&["one", "two"]));
        assert_eq!(offered("\"beta\", "), plain(&["x"]));
        assert!(offered(", ").is_empty());
        let error = offered("\"beta\", \"x\", ");
        assert_eq!(error[0].1, Some('!'));
        assert!(error[0].0.contains("boom"), "{error:?}");
    }
}
