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

/// Splits a check; text before the first marker is the `leftover` section.
pub fn check(service: &str, text: &str, options: &Value) -> Check {
    let mut sections: Vec<Section> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if let Some((name, rest)) = marker(line) {
            sections.push(Section {
                name,
                text: rest.to_owned(),
                line: index as u32,
            });
            continue;
        }
        if sections.is_empty() {
            sections.push(Section {
                name: "leftover".into(),
                text: String::new(),
                line: index as u32,
            });
        }
        let section = sections.last_mut().expect("a section");
        if !section.text.is_empty() || section.line as usize != index {
            section.text.push('\n');
        }
        section.text.push_str(line);
    }
    for section in &mut sections {
        section.text = section.text.trim().to_owned();
    }
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
    fn text_before_a_marker_is_leftover() {
        let check = check("rest", "hello\n#Payload x", &Value::Null);
        assert_eq!(check.text_of("leftover"), "hello");
        assert_eq!(check.text_of("payload"), "x");
        assert_eq!(marker("#1abc"), None);
        assert_eq!(marker("#a:b"), None);
    }
}
