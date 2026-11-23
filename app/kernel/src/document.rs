//! A service document: the console's checks with their response regions and identities,
//! its edit history, and a check as modules see it — its text split at `#name` marker lines.
//! The Kernel knows no section; the service module reads the ones it understands.

pub mod checks;
mod console;
pub mod identity;

pub use console::*;
use serde_json::Value;
use services_sdk::{Check, Section};

/// The section name a marker line starts, and the text after the marker on that line.
pub fn marker(line: &str) -> Option<(String, &str)> {
    let rest = line.trim_start().strip_prefix('#')?;
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    let name = &rest[..end];
    let after = &rest[end..];
    let starts = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
    (starts && (after.is_empty() || after.starts_with(char::is_whitespace)))
        .then(|| (name.to_ascii_lowercase(), after.trim()))
}

/// The value on a marker line. `\@` stands for a value that starts with `@`; a template call
/// (`@name(args)`) is metadata that only names what generated the body (a bare `@name` only when
/// a body follows, as it may be plain text), so the section's content is its body.
fn value(rest: &str, body: &[&str]) -> String {
    if let Some(escaped) = rest.strip_prefix("\\@") {
        return format!("@{escaped}");
    }
    let has_body = body.iter().any(|line| !line.trim().is_empty());
    let call = rest.starts_with('@')
        && services_contracts::templates::parse_section_call(rest).is_ok()
        && (rest.contains('(') || has_body);
    if call { String::new() } else { rest.to_owned() }
}

/// Splits a check; text before the first marker is the `leftover` section.
pub fn check(service: &str, text: &str, options: &Value) -> Check {
    let mut parts: Vec<(String, String, u32, Vec<&str>)> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if let Some((name, rest)) = marker(line) {
            parts.push((name, rest.to_owned(), index as u32, Vec::new()));
        } else if let Some(last) = parts.last_mut() {
            last.3.push(line);
        } else {
            parts.push(("leftover".into(), String::new(), index as u32, vec![line]));
        }
    }
    let sections = parts
        .into_iter()
        .map(|(name, rest, line, body)| {
            let first = value(&rest, &body);
            let text = std::iter::once(first.as_str())
                .chain(body.iter().copied())
                .collect::<Vec<_>>()
                .join("\n");
            Section {
                name,
                text: text.trim().to_owned(),
                line,
            }
        })
        .collect();
    Check {
        service: service.into(),
        text: text.into(),
        sections,
        options: serde_json::to_vec(options).unwrap_or_default(),
        columns: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn splits_at_marker_lines_only() {
        let check = check(
            "rest",
            "#target https://x/a\n#metadata\nAccept: */*\n# a comment\n#payload\n{\"a\":#1}\n",
            &json!({"Repeat": "5s"}),
        );
        let names: Vec<(&str, &str)> = check
            .sections
            .iter()
            .map(|s| (s.name.as_str(), s.text.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("target", "https://x/a"),
                ("metadata", "Accept: */*\n# a comment"),
                ("payload", "{\"a\":#1}")
            ]
        );
        assert_eq!(check.sections[2].line, 4);
        assert_eq!(check.options()["Repeat"], "5s");
    }

    #[test]
    fn a_template_call_on_a_marker_line_is_metadata_and_the_body_is_the_content() {
        let text = |source: &str| -> String {
            check("rest", source, &Value::Null)
                .text_of("target")
                .to_owned()
        };
        assert_eq!(text("#target @url(\"a\", :x)\nhttps://a/b"), "https://a/b");
        assert_eq!(text("#target @url(\"a\")"), "");
        assert_eq!(text("#target @order\nhttps://a"), "https://a");
        // Without parentheses and body it may be text; `\@` always is.
        assert_eq!(text("#target @file"), "@file");
        assert_eq!(text("#target \\@url(\"a\")\nx"), "@url(\"a\")\nx");
        assert_eq!(text("#target @@url(\"a\")\nx"), "@@url(\"a\")\nx");
        assert_eq!(text("#target @url(\"a\"\nx"), "@url(\"a\"\nx");
    }

    #[test]
    fn text_before_a_marker_is_leftover() {
        let check = check("rest", "hello\n#Payload x", &Value::Null);
        assert_eq!(check.text_of("leftover"), "hello");
        assert_eq!(check.text_of("payload"), "x");
        assert_eq!(marker("#1abc"), None);
        assert_eq!(marker("#a:b"), None);
    }
}
