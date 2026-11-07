//! Languages services share: tokenizers for the editor and completion helpers.
//!
//! Token kinds are the editor's CSS classes (`property`, `keyword`, `def`, `string`,
//! `number`, `atom`, `variable`, `operator`, `attribute`, `comment`, `rest-path-param`,
//! `string property`). `{{…}}` placeholders are left unstyled: the App overlays them.
//! Completion inserts may hold `$0`, where the App puts the cursor.

pub mod check;
pub mod curl;
pub mod endpoint;
pub mod headers;
pub mod json;
pub mod resolve;
pub mod rest;
pub mod shell;
pub mod sql;
pub mod table;

use crate::{Completions, Item, Token};
use std::collections::BTreeSet;

/// The most items a completion offers.
pub const MAX_ITEMS: usize = 100;

/// A section marker line (`#name …`): its lowercased name and the trimmed rest of the line.
/// `# text` and `#1` are not markers.
pub fn marker(line: &str) -> Option<(String, &str)> {
    let rest = line.trim_start().strip_prefix('#')?;
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    let (name, after) = rest.split_at(end);
    let starts = name.starts_with(|c: char| c.is_ascii_alphabetic());
    (starts && (after.is_empty() || after.starts_with(char::is_whitespace)))
        .then(|| (name.to_ascii_lowercase(), after.trim()))
}

/// The query's characters appear in the candidate in order, ignoring case.
pub fn fuzzy_match(candidate: &str, query: &str) -> bool {
    let mut chars = candidate.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .all(|q| chars.by_ref().any(|c| c == q))
}

/// The byte where the word before the cursor starts (letters, digits, `_` and `-`).
pub fn token_start(before: &str) -> usize {
    before
        .char_indices()
        .rev()
        .find(|(_, c)| !c.is_alphanumeric() && *c != '_' && *c != '-')
        .map_or(0, |(i, c)| i + c.len_utf8())
}

/// The end of a `{{…}}` placeholder at byte `pos`: after its `}}`, or after `{{` when open.
pub fn placeholder(text: &str, pos: usize) -> Option<usize> {
    let rest = text[pos..].strip_prefix("{{")?;
    Some(rest.find("}}").map_or(pos + 2, |end| pos + 2 + end + 2))
}

/// A one-line label for an insert: without `$0`, lines trimmed and joined.
pub fn label(insert: &str) -> String {
    let text = insert.replace("$0", "");
    if !text.contains('\n') {
        return text;
    }
    let mut out = String::new();
    for line in text
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if !out.is_empty() && !out.ends_with(['{', '[']) && !line.starts_with(['}', ']']) {
            out.push(' ');
        }
        out.push_str(line);
    }
    out
}

/// An item labelled after its insert.
pub fn item(insert: impl Into<String>, detail: &str, kind: &str) -> Item {
    let insert = insert.into();
    Item {
        label: label(&insert),
        insert,
        detail: detail.into(),
        kind: kind.into(),
        next: false,
    }
}

/// Completions of `from..to`: the items whose label fuzzy-matches `query`, without
/// duplicates or the query itself, at most [`MAX_ITEMS`].
pub fn finish(from: usize, to: usize, query: &str, items: Vec<Item>) -> Completions {
    let mut seen = BTreeSet::new();
    Completions {
        from: from as u32,
        to: to as u32,
        items: items
            .into_iter()
            .filter(|item| {
                item.label.trim() != query.trim()
                    && fuzzy_match(&item.label, query)
                    && seen.insert(item.insert.clone())
            })
            .take(MAX_ITEMS)
            .collect(),
    }
}

pub(crate) fn token(start: usize, end: usize, kind: &str) -> Token {
    Token {
        start: start as u32,
        len: (end - start) as u32,
        kind: kind.into(),
    }
}

/// The end of the run of characters from `pos` that satisfy `f`.
pub(crate) fn run(text: &str, pos: usize, f: impl Fn(char) -> bool) -> usize {
    text[pos..].find(|c| !f(c)).map_or(text.len(), |n| pos + n)
}

#[cfg(test)]
pub(crate) fn styled(line: &str, tokens: &[Token]) -> Vec<(String, String)> {
    tokens
        .iter()
        .map(|t| {
            let start = t.start as usize;
            (
                line[start..start + t.len as usize].to_owned(),
                t.kind.clone(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_need_a_name_that_starts_with_a_letter() {
        assert_eq!(
            marker("  #Endpoint GET x"),
            Some(("endpoint".into(), "GET x"))
        );
        assert_eq!(marker("#responce 5:47 · OK").unwrap().0, "responce");
        for line in ["# a comment", "#1abc", "#a:b", "text", "#"] {
            assert_eq!(marker(line), None, "{line}");
        }
    }

    #[test]
    fn fuzzy_matching_accepts_ordered_subsequences_only() {
        assert!(fuzzy_match("Content-Type", "cty"));
        assert!(!fuzzy_match("Content-Type", "xyz"));
    }

    #[test]
    fn labels_are_one_line() {
        assert_eq!(label("{\n    $0\n  }"), "{}");
        assert_eq!(label("\"a\": {\n  $0\n}"), "\"a\": {}");
        assert_eq!(
            label("{\n  \"a\": \"$0\",\n  \"b\": 0\n}"),
            "{\"a\": \"\", \"b\": 0}"
        );
        assert_eq!(label("GET "), "GET ");
    }

    #[test]
    fn placeholders_end_after_their_braces() {
        assert_eq!(placeholder("a{{x:y}}b", 1), Some(8));
        assert_eq!(placeholder("{{open", 0), Some(2));
        assert_eq!(placeholder("{x}", 0), None);
    }

    #[test]
    fn finish_filters_dedups_and_drops_the_typed_text() {
        let items = vec![
            item("Accept: */*", "", ""),
            item("Accept: */*", "", ""),
            item("Cookie:", "", ""),
            item("acc", "", ""),
        ];
        let result = finish(0, 3, "acc", items);
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].insert, "Accept: */*");
    }
}
