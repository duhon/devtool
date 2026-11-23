//! `{{…}}` references in the editors: completion (`{{.` walks the variable tree, `{{:` searches
//! every leaf, `{{@` calls a template) and the hover that shows what a reference resolves to.
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
                c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '*' | '~' | '@')
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
/// the cursor is in, for the templates a live call may name.
#[allow(clippy::too_many_arguments)]
pub fn complete(
    root: &Root,
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
                .and_then(|template| argument_completion(&template, root, site, suffix, ")}}", ch))
                .unwrap_or_default();
        } else {
            name_candidates(library, service, section, call, "{{@", "}}")
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

/// The byte ranges of the references in the parameters of a template definition `line`
/// that offer no options.
pub fn definition_problems(root: &Root, line: &str) -> Vec<(usize, usize)> {
    refs::scan(line)
        .into_iter()
        .filter(|found| in_params(line, found.start))
        .filter(|found| match &found.reference {
            refs::Ref::Dynamic(_) => matches!(offered(root, &found.reference), Ok(Err(_))),
            reference => {
                refs::eval(root, reference).is_none_or(|selection| list(&selection).is_err())
            }
        })
        .map(|found| (found.start, found.end))
        .collect()
}

/// The value (or the error) of the variable reference at column `ch` of `line`; in the
/// parameters of a template definition, its options.
pub fn hover(root: &Root, line: &str, ch: usize) -> Option<Hover> {
    for found in refs::scan(line) {
        let start = utf16(&line[..found.start]);
        let end = utf16(&line[..found.end]);
        if !(start..end).contains(&ch) {
            continue;
        }
        let reference = found.reference.to_string();
        if let refs::Ref::Dynamic(body) = &found.reference {
            let offset = found.start + 2;
            let inner = utf16(&line[..offset]);
            if let Some(mut hover) = ch.checked_sub(inner).and_then(|ch| hover(root, body, ch)) {
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
        let text = |needle: &str| hover(&root, line, at(needle)).unwrap().text;
        assert_eq!(text("{{:region}}"), "{{:region}} = eu");
        assert_eq!(text("{{.dev"), "{{.dev.region}} = us");
        assert!(text("{{:hosts}}").contains("expected a string, got a hash"));
        assert!(text("{{:nope}}").contains("Unknown variable"));
        assert!(text("{{:token}}").ends_with(MASK));
        assert_eq!(text("{{:auth}}"), format!("{{{{:auth}}}} = {MASK}"));
        assert!(hover(&root, line, 0).is_none());
        let nested = "{{:hosts.{{:region}}}}";
        let inner = hover(&root, nested, 10).unwrap();
        assert_eq!(
            (inner.start, inner.end, inner.text.as_str()),
            (9, 20, "{{:region}} = eu")
        );
        let outer = hover(&root, nested, 3).unwrap();
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
        let text = |needle: &str| hover(&root, line, at(needle)).unwrap().text;
        assert_eq!(text("{{:hosts}},"), "{{:hosts}} = x");
        assert_eq!(text("{{:region}}"), "{{:region}} = eu");
        assert!(text("{{:envs}}").contains("envs.qa is a hash"));
        let after = line.rfind("{{:hosts}}").unwrap() + 2;
        assert!(
            hover(&root, line, after)
                .unwrap()
                .text
                .contains("expected a string")
        );
        let envs = line.find("{{:envs}}").unwrap();
        assert_eq!(
            definition_problems(&root, line),
            vec![(envs, envs + "{{:envs}}".len())]
        );
        assert_eq!(
            definition_problems(&root, "@t(a: {{:nope}})"),
            vec![(6, 15)]
        );
        assert!(definition_problems(&root, "@t(a) {{:envs}}").is_empty());
        let line = "@t(a: {{:envs~}}, b: {{:envs.{{a}}~}}) d";
        assert!(definition_problems(&root, line).is_empty());
        let b = line.find("{{:envs.").unwrap() + 3;
        assert_eq!(
            hover(&root, line, b).unwrap().text,
            "{{:envs.{{a}}~}}: depends on a"
        );
    }
}
