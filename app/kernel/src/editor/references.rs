//! `{{…}}` references in the editors: completion (`{{.` walks the variable tree, `{{:` searches
//! every leaf, `{{@` calls a template, `{{$` a jq function) and the hover that shows what a
//! reference resolves to.
use super::templates::{argument_completion, built, name_candidates};
use super::{Candidate, Completion, fuzzy_match};
use services_contracts::templates::{self, Library};
use services_sdk::refs::{self, Root};
use std::collections::BTreeMap;

const MASK: &str = "••••••";
const SHOWN: usize = 60;

fn shorten(value: &str) -> String {
    let line = value.lines().next().unwrap_or("");
    if line.chars().count() > SHOWN || value.contains('\n') {
        format!("{}…", line.chars().take(SHOWN).collect::<String>())
    } else {
        line.to_owned()
    }
}

fn shown(secret: bool, value: &str) -> String {
    if secret { MASK.into() } else { shorten(value) }
}

fn candidate(text: String, label: Option<String>, detail: String, more: bool) -> Candidate {
    Candidate {
        literal: !text.contains("$0"),
        text,
        label,
        detail,
        badge: None,
        continue_hint: more,
        expand: false,
        tab: None,
    }
}

fn utf16(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Where the open `{{` at byte `start` of `full` ends for a completion at `ch`: through the
/// closing `}}` when the text up to it is a plain reference.
fn reference_end(full: &str, before: &str, ch: usize) -> usize {
    let suffix = &full[before.len()..];
    match suffix.find("}}") {
        Some(n)
            if suffix[..n].chars().all(|c| {
                c.is_alphanumeric()
                    || matches!(c, '_' | '-' | '.' | ':' | '*' | '~' | '@' | '$' | '!')
            }) =>
        {
            ch + utf16(&suffix[..n + 2])
        }
        _ => ch,
    }
}

/// The paths `{{.` offers at `fragment` (after the dot): the children of the node typed so far.
fn tree(root: &Root, fragment: &str) -> Vec<Candidate> {
    let (parent, partial) = fragment.rsplit_once('.').unwrap_or(("", fragment));
    let mut out = Vec::new();
    for child in refs::children_at(root, parent) {
        if !partial.is_empty() && !fuzzy_match(&child.key, partial) {
            continue;
        }
        let path = if parent.is_empty() {
            child.key.clone()
        } else {
            format!("{parent}.{}", child.key)
        };
        out.push(match child.value {
            Some(value) => {
                let secret = path.split('.').any(refs::sensitive);
                candidate(
                    format!("{{{{.{path}}}}}"),
                    Some(child.key),
                    shown(secret, &value),
                    false,
                )
            }
            None => candidate(
                format!("{{{{.{path}.$0}}}}"),
                Some(format!("{}.", child.key)),
                "hash".into(),
                true,
            ),
        });
    }
    out
}

/// Every leaf whose path (inside its panel) contains `query`'s characters in order. The first
/// panel with a path wins the relative form; the panels it shadows insert their absolute one.
fn search(root: &Root, query: &str) -> Vec<Candidate> {
    let mut seen = std::collections::BTreeSet::new();
    let mut ranked: Vec<(usize, Candidate)> = Vec::new();
    for leaf in refs::leaves(root) {
        let won = seen.insert(leaf.path.clone());
        if !fuzzy_match(&leaf.path, query) {
            continue;
        }
        let lower = leaf.path.to_lowercase();
        let query = query.to_lowercase();
        let rank = if lower.starts_with(&query) {
            0
        } else if lower.contains(&query) {
            1
        } else {
            2
        };
        let secret = leaf.path.split('.').any(refs::sensitive);
        let value = shown(secret, &leaf.value);
        let (text, detail) = if won {
            (
                format!("{{{{:{}}}}}", leaf.path),
                format!("{value} — {}", leaf.panel),
            )
        } else {
            (
                format!("{{{{.{}.{}}}}}", leaf.panel, leaf.path),
                format!("{value} — {} (shadowed)", leaf.panel),
            )
        };
        let rank = rank + if won { 0 } else { 3 };
        ranked.push((
            rank,
            candidate(text, Some(leaf.path.clone()), detail, false),
        ));
    }
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, candidate)| candidate).collect()
}

/// What the open `{{` at byte `start` of `before` completes at `ch`; `section` is the section
/// the cursor is in, for the templates a live call may name; `defs` are the jq functions a
/// `{{$` call may name.
#[allow(clippy::too_many_arguments)]
pub fn complete(
    (root, defs): (&Root, &str),
    library: &Library,
    service: &str,
    section: &str,
    (full, before): (&str, &str),
    (start, ch): (usize, usize),
    calls: bool,
) -> Completion {
    let from = utf16(&before[..start]);
    let fragment = &before[start + 2..];
    let to = reference_end(full, before, ch);
    let candidates = if let Some(call) = fragment.strip_prefix('@') {
        if !calls {
            Vec::new()
        } else if let Some((name, args)) = super::templates::open_call(fragment) {
            let site = (utf16(&before[..start + 2 + 1 + name.len() + 1]), args);
            let suffix = &full[before.len()..];
            let template = services_contracts::templates::visible(library, service, section)
                .into_iter()
                .find(|template| template.name == name);
            return template
                .and_then(|template| {
                    argument_completion(&template, (root, defs), site, suffix, ")}}", ch)
                })
                .unwrap_or_default();
        } else {
            name_candidates(library, service, section, call, "{{@", "}}")
        }
    } else if let Some(call) = fragment.strip_prefix('$') {
        let (open, call) = match call.strip_prefix('!') {
            Some(call) => ("{{$!", call),
            None => ("{{$", call),
        };
        let name_end = call.find(|c: char| !refs_word(c)).unwrap_or(call.len());
        match call[name_end..].strip_prefix('(') {
            // Inside the arguments: what the current one completes.
            Some(args) => {
                let col = utf16(&before[..before.len() - args.len()]);
                return function_argument(
                    (root, defs),
                    (library, service, section, calls),
                    &call[..name_end],
                    (col, args),
                    ch,
                )
                .unwrap_or_default();
            }
            None => super::functions::name_candidates(defs, call, open),
        }
    } else if let Some(path) = fragment.strip_prefix('.') {
        tree(root, path)
    } else if let Some(query) = fragment.strip_prefix(':') {
        search(root, query)
    } else if fragment.is_empty() {
        let mut starters = vec![
            candidate(
                "{{.".into(),
                Some("{{.".into()),
                "Walk the variable tree".into(),
                true,
            ),
            candidate(
                "{{:".into(),
                Some("{{:".into()),
                "Search every variable".into(),
                true,
            ),
        ];
        if calls {
            starters.push(candidate(
                "{{@".into(),
                Some("{{@".into()),
                "Call a template".into(),
                true,
            ));
        }
        if !services_jq::names(defs).is_empty() {
            starters.push(candidate(
                "{{$".into(),
                Some("{{$".into()),
                "Call a jq function".into(),
                true,
            ));
            starters.push(candidate(
                "{{$!".into(),
                Some("{{$!".into()),
                "Insert a jq function's result on Enter".into(),
                true,
            ));
        }
        starters
    } else {
        Vec::new()
    };
    let mut candidates = candidates;
    candidates.truncate(100);
    Completion {
        from,
        to,
        candidates,
        comma: None,
        automatic: true,
    }
}

/// A candidate of a reference without its braces: what an argument of a call writes.
fn bare(mut candidate: Candidate) -> Candidate {
    if let Some(text) = candidate.text.strip_prefix("{{") {
        candidate.text = text.strip_suffix("}}").unwrap_or(text).to_owned();
    }
    candidate
}

/// The variables an argument `partial` (`.path` or `:query`) completes, without braces.
pub(super) fn variable_argument(root: &Root, partial: &str) -> Option<Vec<Candidate>> {
    let candidates = if let Some(path) = partial.strip_prefix('.') {
        tree(root, path)
    } else if let Some(query) = partial.strip_prefix(':') {
        search(root, query)
    } else {
        return None;
    };
    Some(candidates.into_iter().map(bare).collect())
}

/// What the current argument of the call of the jq function `name` completes, `args` being
/// the text after its `(` up to the cursor, from UTF-16 column `col`: a variable for `.` and
/// `:`, a function for `$` and a template for `@` (their own arguments in turn), the kinds of
/// argument when nothing is typed; the signature, its current parameter marked, otherwise.
fn function_argument(
    (root, defs): (&Root, &str),
    (library, service, section, calls): (&Library, &str, &str, bool),
    name: &str,
    (col, args): (usize, &str),
    ch: usize,
) -> Option<Completion> {
    let (index, start) = super::functions::argument_at(args)?;
    let typed = &args[start..];
    let partial = typed.trim_start();
    let from = col + utf16(&args[..start]) + utf16(&typed[..typed.len() - partial.len()]);
    let signature = super::functions::signature_hint(defs, name, index);
    let found = |candidates: Vec<Candidate>| {
        let mut candidates = candidates;
        candidates.truncate(100);
        Completion {
            from,
            to: ch,
            candidates,
            comma: None,
            automatic: true,
        }
    };
    if let Some(candidates) = variable_argument(root, partial) {
        return Some(found(candidates));
    }
    if let Some(call) = partial.strip_prefix('$') {
        let name_end = call.find(|c: char| !refs_word(c)).unwrap_or(call.len());
        return Some(match call[name_end..].strip_prefix('(') {
            Some(inner) => {
                let col = from + utf16(&partial[..partial.len() - inner.len()]);
                return function_argument(
                    (root, defs),
                    (library, service, section, calls),
                    &call[..name_end],
                    (col, inner),
                    ch,
                );
            }
            None => found(
                super::functions::name_candidates(defs, call, "$")
                    .into_iter()
                    .map(|mut candidate| {
                        if let Some(text) = candidate.text.strip_suffix("}}") {
                            candidate.text = text.to_owned();
                        }
                        candidate
                    })
                    .collect(),
            ),
        });
    }
    if let Some(call) = partial.strip_prefix('@').filter(|_| calls) {
        if let Some((template, inner)) = super::templates::open_call(partial) {
            let col = from + utf16(&partial[..partial.len() - inner.len()]);
            let template = templates::visible(library, service, section)
                .into_iter()
                .find(|known| known.name == template)?;
            return argument_completion(&template, (root, defs), (col, inner), "", ")", ch);
        }
        return Some(found(name_candidates(
            library, service, section, call, "@", "",
        )));
    }
    if partial.is_empty() {
        let mut candidates: Vec<Candidate> = signature
            .map(|signature| candidate(String::new(), Some(signature), "jq function".into(), false))
            .into_iter()
            .collect();
        let mut kinds = vec![
            (".", "Walk the variable tree"),
            (":", "Search every variable"),
            ("$", "Call a jq function"),
            ("\"", "Text"),
        ];
        if calls {
            kinds.insert(3, ("@", "Call a template"));
        }
        candidates.extend(kinds.into_iter().map(|(kind, detail)| {
            candidate(kind.into(), Some(kind.into()), detail.into(), kind != "\"")
        }));
        return Some(found(candidates));
    }
    let signature = signature?;
    Some(Completion {
        from: ch,
        to: ch,
        candidates: vec![candidate(
            String::new(),
            Some(signature),
            "jq function".into(),
            false,
        )],
        comma: None,
        automatic: true,
    })
}

/// A reference under the cursor: its UTF-16 columns and what it resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hover {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// Whether byte `at` of `line` is inside the parameters of a template definition
/// (`@name(…)` at the start of the line), where a reference is a list of options.
fn in_params(line: &str, at: usize) -> bool {
    let Some(rest) = line.strip_prefix('@') else {
        return false;
    };
    let name = rest.find(|c: char| !refs_word(c)).unwrap_or(rest.len());
    if name == 0 || !rest[name..].starts_with('(') {
        return false;
    }
    let open = 1 + name;
    if at <= open {
        return false;
    }
    let spans = refs::scan(line);
    let (mut quoted, mut escaped) = (false, false);
    let mut index = open + 1;
    while index < at {
        if let Some(span) = spans.iter().find(|span| span.start == index) {
            index = span.end;
            continue;
        }
        let c = line.as_bytes()[index];
        match c {
            _ if escaped => escaped = false,
            b'\\' if quoted => escaped = true,
            b'"' => quoted = !quoted,
            b')' if !quoted => return false,
            _ => {}
        }
        index += 1;
    }
    true
}

fn refs_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// The options a selection offers, shown `a | b`, or why it offers none.
fn list(selection: &refs::Selection) -> Result<String, String> {
    if selection.items.is_empty() {
        return Err("unknown variable".into());
    }
    if selection.secret() {
        return Ok(MASK.into());
    }
    let items = refs::as_list(selection)?;
    Ok(items
        .iter()
        .map(|item| item.value.as_str())
        .collect::<Vec<_>>()
        .join(" | "))
}

/// What a nested reference in template parameters offers: its options shown `a | b` or the
/// error, or the parameters it depends on.
fn offered(root: &Root, reference: &refs::Ref) -> Result<Result<String, String>, Vec<String>> {
    templates::fill_domain(&reference.to_string(), &BTreeMap::new())?;
    Ok(built(root, reference).and_then(|reference| {
        refs::eval(root, &reference)
            .ok_or_else(|| "unknown variable".to_owned())
            .and_then(|selection| list(&selection))
    }))
}

/// What a function call domain offers, shown `a | b`, or the error, or the parameters it
/// depends on.
fn function_offered(
    root: &Root,
    defs: &str,
    reference: &refs::Ref,
) -> Result<Result<String, String>, Vec<String>> {
    let text = templates::fill_domain(&reference.to_string(), &BTreeMap::new())?;
    Ok(super::functions::options(&text, root, defs).map(|items| {
        items
            .iter()
            .map(|item| item.value.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    }))
}

/// The byte ranges of the references in the parameters of a template definition `line`
/// that offer no options; `defs` are the jq functions a `{{$name(…)}}` domain may name.
pub fn definition_problems(root: &Root, defs: &str, line: &str) -> Vec<(usize, usize)> {
    refs::scan(line)
        .into_iter()
        .filter(|found| in_params(line, found.start))
        .filter(|found| match &found.reference {
            refs::Ref::Dynamic(_) => matches!(offered(root, &found.reference), Ok(Err(_))),
            refs::Ref::Function { .. } => {
                matches!(function_offered(root, defs, &found.reference), Ok(Err(_)))
            }
            reference => {
                refs::eval(root, reference).is_none_or(|selection| list(&selection).is_err())
            }
        })
        .map(|found| (found.start, found.end))
        .collect()
}

/// The byte ranges of the function calls in `line` that name no function of `defs`.
pub fn unknown_calls(defs: &str, line: &str) -> Vec<(usize, usize)> {
    if !line.contains("{{$") {
        return Vec::new();
    }
    let known = services_jq::names(defs);
    refs::scan(line)
        .into_iter()
        .filter(|found| {
            matches!(&found.reference, refs::Ref::Function { name, .. }
                if !known.iter().any(|(known, _)| known == name))
        })
        .map(|found| (found.start, found.end))
        .collect()
}

/// The value (or the error) of the variable reference or function call at column `ch` of
/// `line`; in the parameters of a template definition, its options. `defs` are the jq
/// functions a call may name.
pub fn hover(root: &Root, defs: &str, line: &str, ch: usize) -> Option<Hover> {
    for found in refs::scan(line) {
        let start = utf16(&line[..found.start]);
        let end = utf16(&line[..found.end]);
        if !(start..end).contains(&ch) {
            continue;
        }
        let reference = found.reference.to_string();
        if let refs::Ref::Function { .. } = &found.reference {
            let transform = line
                .split_once(':')
                .is_some_and(|(key, _)| key.trim().eq_ignore_ascii_case("transform"));
            let text = if transform {
                // A `Transform` option runs on the response, which is not known here.
                format!("{reference}: applies to the response (`.` is the driver's answer)")
            } else if in_params(line, found.start) {
                match function_offered(root, defs, &found.reference) {
                    Err(names) => format!("{reference}: depends on {}", names.join(", ")),
                    Ok(Ok(values)) => format!("{reference} = {values}"),
                    Ok(Err(error)) => error,
                }
            } else {
                match super::functions::resolve(&reference, root, defs) {
                    Ok(resolved) if !resolved.secrets.is_empty() => format!("{reference} = {MASK}"),
                    Ok(resolved) => format!("{reference} = {}", shorten(&resolved.text)),
                    Err(error) => error,
                }
            };
            return Some(Hover { start, end, text });
        }
        if let refs::Ref::Dynamic(body) = &found.reference {
            let offset = found.start + 2;
            let inner = utf16(&line[..offset]);
            if let Some(mut hover) = ch
                .checked_sub(inner)
                .and_then(|ch| hover(root, defs, body, ch))
            {
                hover.start += inner;
                hover.end += inner;
                return Some(hover);
            }
            if in_params(line, found.start) {
                let text = match offered(root, &found.reference) {
                    Err(names) => format!("{reference}: depends on {}", names.join(", ")),
                    Ok(Ok(values)) => format!("{reference} = {values}"),
                    Ok(Err(error)) => format!("{reference}: {error}"),
                };
                return Some(Hover { start, end, text });
            }
            let text = match refs::resolve(&reference, root, false, &mut |_, _| Err(String::new()))
            {
                Ok(resolved) if !resolved.secrets.is_empty() => format!("{reference} = {MASK}"),
                Ok(resolved) => format!("{reference} = {}", resolved.text),
                Err(error) => error,
            };
            return Some(Hover { start, end, text });
        }
        let selection = refs::eval(root, &found.reference)?;
        if in_params(line, found.start) {
            let text = match list(&selection) {
                Ok(values) => format!("{reference} = {values}"),
                Err(error) => format!("{reference}: {error}"),
            };
            return Some(Hover { start, end, text });
        }
        let text = match refs::resolve(&reference, root, false, &mut |_, _| Err(String::new())) {
            Ok(resolved) if selection.secret() || !resolved.secrets.is_empty() => {
                format!("{reference} = {MASK}")
            }
            Ok(resolved) => format!("{reference} = {}", resolved.text),
            Err(error) => error,
        };
        return Some(Hover { start, end, text });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_of_unknown_functions_are_found() {
        let line = r#"x {{$!k8s("a")}} {{$env()}} {{:token}}"#;
        assert_eq!(unknown_calls("def env: 1;", line), [(2, 16)]);
        assert!(unknown_calls("def env: 1; def k8s(a): a;", line).is_empty());
    }

    fn root() -> Root {
        let parse = |text| refs::parse_lines(text).unwrap();
        Root::new(vec![
            (
                "var".into(),
                parse(
                    "region: eu\ntoken: abc\nhosts:\n  a: x\nauth: Bearer {{:token}}\nenvs:\n  qa:\n    1: c",
                ),
            ),
            ("dev".into(), parse("region: us")),
        ])
    }

    #[test]
    fn hover_shows_the_value_the_error_or_a_mask_at_utf16_columns() {
        let root = root();
        let line = "😀 {{:region}} {{.dev.region}} {{:hosts}} {{:nope}} {{:token}} {{:auth}}";
        let at = |needle: &str| line[..line.find(needle).unwrap()].encode_utf16().count() + 2;
        let text = |needle: &str| hover(&root, "", line, at(needle)).unwrap().text;
        assert_eq!(text("{{:region}}"), "{{:region}} = eu");
        assert_eq!(text("{{.dev"), "{{.dev.region}} = us");
        assert!(text("{{:hosts}}").contains("expected a string, got a hash"));
        assert!(text("{{:nope}}").contains("Unknown variable"));
        assert!(text("{{:token}}").ends_with(MASK));
        assert_eq!(text("{{:auth}}"), format!("{{{{:auth}}}} = {MASK}"));
        assert!(hover(&root, "", line, 0).is_none());
        let nested = "{{:hosts.{{:region}}}}";
        let inner = hover(&root, "", nested, 10).unwrap();
        assert_eq!(
            (inner.start, inner.end, inner.text.as_str()),
            (9, 20, "{{:region}} = eu")
        );
        let outer = hover(&root, "", nested, 3).unwrap();
        assert_eq!((outer.start, outer.end), (0, nested.len()));
        assert!(
            outer.text.contains("Unknown variable {{:hosts.eu}}"),
            "{}",
            outer.text
        );
    }

    #[test]
    fn hover_in_template_parameters_shows_the_options() {
        let root = root();
        let line = "@t(a: {{:hosts}}, b: \"x)\" | {{:region}}, c: {{:envs}}) d {{:hosts}}";
        let at = |needle: &str| line[..line.find(needle).unwrap()].encode_utf16().count() + 2;
        let text = |needle: &str| hover(&root, "", line, at(needle)).unwrap().text;
        assert_eq!(text("{{:hosts}},"), "{{:hosts}} = x");
        assert_eq!(text("{{:region}}"), "{{:region}} = eu");
        assert!(text("{{:envs}}").contains("envs.qa is a hash"));
        let after = line.rfind("{{:hosts}}").unwrap() + 2;
        assert!(
            hover(&root, "", line, after)
                .unwrap()
                .text
                .contains("expected a string")
        );
        let envs = line.find("{{:envs}}").unwrap();
        assert_eq!(
            definition_problems(&root, "", line),
            vec![(envs, envs + "{{:envs}}".len())]
        );
        assert_eq!(
            definition_problems(&root, "", "@t(a: {{:nope}})"),
            vec![(6, 15)]
        );
        assert!(definition_problems(&root, "", "@t(a) {{:envs}}").is_empty());
        let line = "@t(a: {{:envs~}}, b: {{:envs.{{a}}~}}) d";
        assert!(definition_problems(&root, "", line).is_empty());
        let b = line.find("{{:envs.").unwrap() + 3;
        assert_eq!(
            hover(&root, "", line, b).unwrap().text,
            "{{:envs.{{a}}~}}: depends on a"
        );
    }

    #[test]
    fn functions_hover_complete_and_flag_unknown_names() {
        let root = root();
        let defs = "def sub(k): .var[k] | keys; def first: .var.hosts.a; def tok: .var.token;";
        let text = |line: &str| hover(&root, defs, line, 3).unwrap().text;
        assert_eq!(text(r#"{{$sub("hosts")}}"#), r#"{{$sub("hosts")}} = ["a"]"#);
        assert_eq!(text("{{$first}} tail"), "{{$first()}} = x");
        assert!(text("{{$nope()}}").contains("Unknown function $nope"));
        assert!(text(r#"{{$sub("region")}}"#).contains("cannot"));
        assert_eq!(text("{{$tok}}"), format!("{{{{$tok()}}}} = {MASK}"));
        assert_eq!(
            hover(&root, defs, "Transform: {{$first}}", 12)
                .unwrap()
                .text,
            "{{$first()}}: applies to the response (`.` is the driver's answer)"
        );
        let line = "@t(a, b: {{$sub(a)}}, c: {{$sub(\"envs\")}}, d: {{$nope}}) d";
        let at = |needle: &str| line.find(needle).unwrap();
        assert_eq!(
            hover(&root, defs, line, at("{{$sub(a)") + 3).unwrap().text,
            "{{$sub(a)}}: depends on a"
        );
        assert_eq!(
            hover(&root, defs, line, at("{{$sub(\"envs") + 3)
                .unwrap()
                .text,
            r#"{{$sub("envs")}} = qa"#
        );
        let nope = at("{{$nope");
        assert_eq!(
            definition_problems(&root, defs, line),
            vec![(nope, nope + "{{$nope}}".len())]
        );
        let complete_at = |typed: &str| {
            complete(
                (&root, defs),
                &Library::new(),
                "alpha",
                "body",
                (typed, typed),
                (0, utf16(typed)),
                true,
            )
            .candidates
            .into_iter()
            .map(|candidate| candidate.text)
            .collect::<Vec<_>>()
        };
        assert_eq!(complete_at("{{$"), ["{{$sub(", "{{$first}}", "{{$tok}}"]);
        assert_eq!(complete_at("{{$fi"), ["{{$first}}"]);
        assert!(complete_at("{{").contains(&"{{$".to_owned()));
        let hint = |typed: &str| {
            complete(
                (&root, defs),
                &Library::new(),
                "alpha",
                "body",
                (typed, typed),
                (0, utf16(typed)),
                true,
            )
            .candidates
            .into_iter()
            .map(|candidate| (candidate.text, candidate.label.unwrap_or_default()))
            .collect::<Vec<_>>()
        };
        let kinds = [".", ":", "$", "@", "\""].map(|kind| (kind.to_owned(), kind.to_owned()));
        let signature = (String::new(), "sub(«k»)".to_owned());
        assert_eq!(
            hint("{{$sub("),
            [vec![signature.clone()], kinds.to_vec()].concat()
        );
        assert!(complete_at("{{$sub(.var.ho").contains(&".var.hosts.$0".to_owned()));
        assert!(complete_at("{{$sub( :reg").contains(&":region".to_owned()));
        assert_eq!(complete_at("{{$sub($fi"), ["$first"]);
        assert!(complete_at("{{$sub($sub(:reg").contains(&":region".to_owned()));
        assert_eq!(
            hint("{{$sub($sub(\"a\"), ")[0].1,
            "sub(k) — takes 1 argument(s)",
            "a closed nested call is one argument"
        );
        assert_eq!(
            hint("{{$!sub(\"a, b"),
            [(String::new(), "sub(«k»)".to_owned())]
        );
        assert!(hint("{{$sub(\"a\")").is_empty());
        assert_eq!(hint("{{$nope("), kinds.to_vec());
        assert_eq!(
            complete_at("{{$!f"),
            ["{{$!first}}"],
            "an eager call offers the names too"
        );
        assert!(complete_at("{{").contains(&"{{$!".to_owned()));
        assert_eq!(text("{{$!first}}"), "{{$!first()}} = x");
        assert_eq!(text("{{$!tok}}"), format!("{{{{$!tok()}}}} = {MASK}"));
    }
}
