//! `Name: value` lines: headers, and option lists with the same shape.

use super::{item, placeholder, run, token};
use crate::{Item, Token};
use std::collections::BTreeSet;

/// Tokens of a `Name: value` line: `attribute` name, `string` value; a `#` line is a
/// `comment`.
pub fn tokens(line: &str) -> Vec<Token> {
    if line.trim_start().starts_with('#') {
        return vec![token(0, line.len(), "comment")];
    }
    let mut out = Vec::new();
    let mut value = false;
    let mut pos = 0;
    while pos < line.len() {
        if let Some(end) = placeholder(line, pos) {
            pos = end;
            continue;
        }
        let rest = &line[pos..];
        if rest.starts_with(char::is_whitespace) {
            pos = run(line, pos, char::is_whitespace);
            continue;
        }
        if rest.starts_with(':') && !value {
            value = true;
            pos += 1;
            continue;
        }
        let stop = rest.find("{{").unwrap_or(rest.len());
        let end = if value {
            stop
        } else {
            rest.find(':').unwrap_or(stop).min(stop)
        };
        out.push(token(
            pos,
            pos + end,
            if value { "string" } else { "attribute" },
        ));
        pos += end;
    }
    out
}

/// The `Name: value` pairs of a section in order; blank, `#` and nameless lines are skipped.
pub fn pairs(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| !name.trim().is_empty())
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

/// The lowercased name of a `Name: value` line.
pub fn name(line: &str) -> String {
    line.split(':')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// Lowercased names the lines already use.
pub fn used<'a>(lines: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    lines
        .into_iter()
        .filter(|line| line.contains(':'))
        .map(name)
        .collect()
}

/// Header items from `Name: value` hints, then `extra` names (`Name: `) not among the
/// hints, all without the `used` names.
pub fn items(hints: &[&str], extra: &[(&str, &str)], used: &BTreeSet<String>) -> Vec<Item> {
    let mut known: BTreeSet<_> = hints.iter().map(|hint| name(hint)).collect();
    hints
        .iter()
        .map(|hint| item(*hint, "Header", "header"))
        .chain(
            extra
                .iter()
                .filter(|(header, _)| known.insert(header.to_ascii_lowercase()))
                .map(|(header, detail)| item(format!("{header}: "), detail, "header")),
        )
        .filter(|item| !used.contains(&name(&item.insert)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::styled;

    fn tokens_of(line: &str) -> Vec<(String, String)> {
        styled(line, &tokens(line))
    }

    #[test]
    fn names_values_and_comments() {
        let pairs = |items: &[(&str, &str)]| {
            items
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            tokens_of("Content-Type: application/json"),
            pairs(&[
                ("Content-Type", "attribute"),
                ("application/json", "string")
            ])
        );
        assert_eq!(
            tokens_of("  # Accept: */*"),
            pairs(&[("  # Accept: */*", "comment")])
        );
        assert_eq!(
            tokens_of("Authorization: Bearer {{prod:TOKEN}} x"),
            pairs(&[
                ("Authorization", "attribute"),
                ("Bearer ", "string"),
                ("x", "string")
            ])
        );
        assert_eq!(
            tokens_of("X-{{a}}: v:w"),
            pairs(&[("X-", "attribute"), ("v:w", "string")])
        );
        assert_eq!(tokens_of(""), vec![]);
    }

    #[test]
    fn pairs_keep_order_and_skip_comments() {
        assert_eq!(
            pairs("B: 2\n# A: 0\n\nA: 1:2\n: none\nbad"),
            [("B".into(), "2".into()), ("A".into(), "1:2".into())]
        );
    }

    #[test]
    fn items_skip_used_names_and_known_extras() {
        let used = used(["accept: x", "garbage"]);
        let items = items(
            &["Accept: */*", "Cookie:"],
            &[("cookie", "OpenAPI header"), ("X-Trace", "OpenAPI header")],
            &used,
        );
        assert_eq!(
            items.iter().map(|i| i.insert.as_str()).collect::<Vec<_>>(),
            ["Cookie:", "X-Trace: "]
        );
        assert_eq!(items[1].detail, "OpenAPI header");
    }
}
