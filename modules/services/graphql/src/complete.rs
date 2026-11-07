//! Completion in a GraphQL check: section markers, endpoint schemes, headers, JSON in
//! `#variables` and the query from the introspected schema.

use crate::language;
use serde_json::Value;
use services_sdk::lang::{self, finish, headers, item, json, marker};
use services_sdk::{Check, Completions, Item};

pub const SECTIONS: [&str; 4] = ["endpoint", "headers", "variables", "query"];

pub const HEADERS: [&str; 9] = [
    "Accept: */*",
    "Accept-Encoding: gzip, deflate, br",
    "Authorization: ****** application/json",
    "Cookie:",
    "Host:",
    "User-Agent:",
    "X-Api-Key:",
    "X-Forwarded-For:",
    "X-Requested-With: XMLHttpRequest",
];

const URLS: [&str; 2] = ["http://", "https://"];

/// The lines of the check: their byte start and text.
struct Lines<'a>(Vec<(usize, &'a str)>);

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        let mut start = 0;
        Self(
            text.split('\n')
                .map(|line| {
                    let at = start;
                    start += line.len() + 1;
                    (at, line)
                })
                .collect(),
        )
    }
    fn text(&self, line: usize) -> &'a str {
        self.0.get(line).map_or("", |(_, text)| text)
    }
    /// The byte where `line` starts; the end of `text` past the last line.
    fn start(&self, line: usize, text: &str) -> usize {
        self.0.get(line).map_or(text.len(), |(at, _)| *at)
    }
}

pub fn complete(schema: Option<&Value>, check: &Check, pos: usize) -> Completions {
    let text = check.text.as_str();
    let mut pos = pos.min(text.len());
    while !text.is_char_boundary(pos) {
        pos -= 1;
    }
    let none = || finish(pos, pos, "", vec![]);
    let lines = Lines::new(text);
    let line = lines
        .0
        .iter()
        .rposition(|(start, _)| *start <= pos)
        .unwrap_or(0);
    let (line_start, full) = lines.0[line];
    let before = &full[..pos - line_start];
    // `{{…}}` variables are the App's.
    if before
        .rfind("{{")
        .is_some_and(|start| !before[start..].contains("}}"))
    {
        return none();
    }
    let markers: Vec<_> = (0..lines.0.len())
        .filter(|i| *i != line)
        .filter_map(|i| Some((i, marker(lines.text(i))?.0)))
        .collect();
    let section = markers.iter().rev().find(|(i, _)| *i < line);
    if markers
        .iter()
        .any(|(i, name)| *i < line && matches!(name.as_str(), "responce" | "response"))
    {
        return none();
    }
    let unused = || -> Vec<Item> {
        SECTIONS
            .iter()
            .filter(|name| !markers.iter().any(|(_, used)| used == *name))
            .map(|name| item(format!("#{name}\n"), "Section", "section"))
            .collect()
    };
    if before.trim_start().starts_with('#') || (before.trim().is_empty() && section.is_none()) {
        if before.trim().is_empty() && !full.trim().is_empty() {
            return none();
        }
        let offset = before.len() - before.trim_start().len();
        let end = full[offset..]
            .find(char::is_whitespace)
            .map_or(full.len(), |length| offset + length);
        return finish(
            line_start + offset,
            line_start + end,
            before.trim(),
            unused(),
        );
    }
    let (body_start, name) = match section {
        Some((at, name)) => (at + 1, name.as_str()),
        None => (0, "query"),
    };
    let section_end = (line + 1..lines.0.len())
        .find(|i| markers.iter().any(|(at, _)| at == i))
        .unwrap_or(lines.0.len());
    let start = lines.start(body_start, text);
    let end = lines.start(section_end, text).saturating_sub(1).max(pos);
    match name {
        "endpoint" => {
            if before.trim().is_empty()
                && (body_start..line).any(|i| !lines.text(i).trim().is_empty())
            {
                return finish(line_start, pos, "", unused());
            }
            let value = before.trim_start();
            if value.contains("://") {
                return none();
            }
            let urls = URLS.iter().map(|url| item(*url, "", "url")).collect();
            finish(pos - value.len(), pos, value, urls)
        }
        "headers" => {
            let used = headers::used(
                (body_start..section_end)
                    .filter(|i| *i != line)
                    .map(|i| lines.text(i)),
            );
            let mut result = finish(
                line_start,
                pos,
                before.trim(),
                headers::items(&HEADERS, &[], &used),
            );
            if before.trim().is_empty() && !used.is_empty() {
                let mut items = finish(line_start, pos, "", unused()).items;
                items.append(&mut result.items);
                items.truncate(lang::MAX_ITEMS);
                result.items = items;
            }
            result
        }
        "variables" => finish(
            pos,
            pos,
            "",
            json::templates(&text[start..pos], before, &text[pos..end]),
        ),
        "query" => query(
            schema.unwrap_or(&Value::Null),
            check.text_of("variables"),
            &text[start..pos],
            before,
            &text[pos..end],
            pos,
            &full[before.len()..],
            unused,
        ),
        _ => none(),
    }
}

/// An insert as the editor takes it: the cursor (`$0`) inside `()`, `""` and `[]`.
fn insert(text: &str) -> (String, bool) {
    if let Some(name) = text.strip_suffix("()")
        && name
            .trim()
            .trim_start_matches('@')
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
    {
        return (format!("{name}($0)"), true);
    }
    for pair in ["\"\"", "[]"] {
        if let Some(head) = text.strip_suffix(pair) {
            return (format!("{head}{}$0{}", &pair[..1], &pair[1..]), false);
        }
    }
    let closing = matches!(text, ")" | "]" | "}");
    (text.to_owned(), closing)
}

/// Completion in the query: `body` is the section up to the cursor, `after` the rest of
/// it, `suffix` the rest of the cursor's line.
#[allow(clippy::too_many_arguments)]
fn query(
    schema: &Value,
    variables: &str,
    body: &str,
    before: &str,
    after: &str,
    pos: usize,
    suffix: &str,
    unused: impl Fn() -> Vec<Item>,
) -> Completions {
    let (fragment, candidates, _, _) = language::suggest(
        body,
        before,
        after,
        schema,
        &Value::String(variables.to_owned()),
    );
    let mut from = pos - fragment.len().min(before.len());
    let mut to = pos;
    if before.trim().is_empty() && language::operation_complete(body, schema) {
        return finish(pos - before.len(), pos, "", unused());
    }
    // Filtered as `finish` does, so the adjustments below see only what is offered.
    let mut seen = std::collections::BTreeSet::new();
    let mut candidates: Vec<_> = candidates
        .into_iter()
        .filter(|(text, _)| {
            text.trim() != fragment.trim()
                && lang::fuzzy_match(text, &fragment)
                && seen.insert(text.clone())
        })
        .collect();
    let name_end = if matches!(fragment.as_str(), "." | "..") {
        suffix.bytes().take_while(|c| *c == b'.').count()
    } else {
        suffix
            .bytes()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == b'_')
            .count()
    };
    if !fragment.is_empty() {
        to = pos + name_end;
    }
    if candidates.iter().any(|(text, _)| text == "... on ") {
        let dots = suffix.bytes().take_while(|byte| *byte == b'.').count();
        if let Some(rest) = suffix[dots..].trim_start().strip_prefix("on")
            && !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
        {
            candidates.retain(|(text, _)| text == "... on ");
            let remaining = rest.trim_start();
            to = pos + suffix.len() - remaining.len();
        }
    }
    if candidates
        .iter()
        .all(|(text, _)| text.starts_with('$') || text.ends_with(": "))
        && candidates.iter().any(|(text, _)| text.ends_with(": "))
    {
        to = pos + name_end;
        if suffix[name_end..].trim_start().starts_with(':') {
            for (text, _) in &mut candidates {
                if let Some(name) = text.strip_suffix(": ") {
                    *text = name.to_owned();
                }
            }
            candidates.retain(|(text, _)| *text != fragment);
        }
    }
    // A closing bracket the cursor is already before is not typed again.
    candidates.retain(|(text, _)| {
        !(matches!(text.as_str(), ")" | "]" | "}") && suffix.starts_with(text.as_str()))
    });
    let items = candidates
        .into_iter()
        .map(|(text, detail)| {
            let (insert, next) = insert(&text);
            let label = if text == "... on " {
                "... on".to_owned()
            } else {
                lang::label(&insert)
            };
            Item {
                label,
                insert,
                detail,
                kind: String::new(),
                next,
            }
        })
        .collect();
    if from > pos {
        from = pos;
    }
    finish(from, to, &fragment, items)
}
