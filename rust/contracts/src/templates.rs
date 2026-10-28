use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type Library = BTreeMap<String, BTreeMap<String, String>>;

pub fn sections(service: &str) -> &'static [&'static str] {
    match service {
        "mysql" => &["check", "endpoint", "options", "query"],
        "rest" => &["check", "endpoint", "headers", "body"],
        "graphql" => &["check", "endpoint", "headers", "variables", "query"],
        "grpc" => &["check", "endpoint", "headers", "proto", "message"],
        "splunk" => &["check", "endpoint", "headers", "options", "search"],
        "terminal" => &["check", "endpoint", "command", "env"],
        _ => &[],
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Template {
    pub name: String,
    pub description: String,
    pub body: String,
}

pub fn parse(text: &str) -> Result<Vec<Template>, String> {
    if text.len() > 256 * 1024 || text.contains('\0') {
        return Err("Templates must be at most 256 KiB per section and cannot contain NUL.".into());
    }
    let mut result: Vec<Template> = Vec::new();
    let mut names = BTreeSet::new();
    for (index, line) in text.lines().enumerate() {
        if let Some(header) = line.strip_prefix('@').filter(|line| !line.starts_with('@')) {
            let end = header.find(char::is_whitespace).unwrap_or(header.len());
            let name = &header[..end];
            if name.is_empty()
                || name.chars().count() > 80
                || !name
                    .chars()
                    .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.'))
            {
                return Err(format!(
                    "Line {}: expected @name followed by an optional description. Use @@ for a literal leading @ in a body.",
                    index + 1
                ));
            }
            if !names.insert(name.to_owned()) {
                return Err(format!("Line {}: duplicate template @{name}.", index + 1));
            }
            result.push(Template {
                name: name.into(),
                description: header[end..].trim().into(),
                body: String::new(),
            });
        } else if let Some(template) = result.last_mut() {
            template.body.push_str(if line.starts_with("@@") {
                &line[1..]
            } else {
                line
            });
            template.body.push('\n');
        } else if !line.trim().is_empty() {
            return Err(format!(
                "Line {}: start a template with @name description.",
                index + 1
            ));
        }
    }
    for template in &mut result {
        let lines: Vec<_> = template.body.lines().collect();
        let start = lines.iter().position(|line| !line.trim().is_empty());
        let end = lines.iter().rposition(|line| !line.trim().is_empty());
        template.body = match (start, end) {
            (Some(start), Some(end)) => lines[start..=end].join("\n"),
            _ => return Err(format!("Template @{} has no content.", template.name)),
        };
    }
    Ok(result)
}

pub fn validate(library: &Library) -> Result<(), String> {
    for (service, entries) in library {
        if sections(service).is_empty() {
            return Err(format!("Unknown template service: {service}"));
        }
        for (section, text) in entries {
            if !sections(service).contains(&section.as_str()) {
                return Err(format!("Unknown {service} template section: {section}"));
            }
            parse(text)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn splunk_only_accepts_canonical_template_sections() {
        assert_eq!(
            sections("splunk"),
            &["check", "endpoint", "headers", "options", "search"]
        );
        for section in ["from", "to", "app", "count"] {
            let library = BTreeMap::from([(
                "splunk".into(),
                BTreeMap::from([(section.into(), "@old Existing template\nvalue".into())]),
            )]);
            assert_eq!(
                validate(&library).unwrap_err(),
                format!("Unknown splunk template section: {section}")
            );
        }
    }
    #[test]
    fn parses_literal_multiline_bodies_without_headers_or_descriptions() {
        let entries = parse("@my_create_book comment here\n{\n  \"id\": 123\n}\n\n@другой second\n  SELECT 1;\n\n@@counter\n").unwrap();
        assert_eq!(entries[0].name, "my_create_book");
        assert_eq!(entries[0].description, "comment here");
        assert_eq!(entries[0].body, "{\n  \"id\": 123\n}");
        assert_eq!(entries[1].body, "  SELECT 1;\n\n@counter");
    }
    #[test]
    fn rejects_invalid_definitions_but_does_not_validate_body_languages() {
        for text in ["@", "@a", "body\n@a\nx", "@a\none\n@a\ntwo", "@bad/name\nx"] {
            assert!(parse(text).is_err(), "{text}");
        }
        assert!(parse("@a\n{\"trailing\":1,}\n").is_ok());
        assert!(parse("").unwrap().is_empty());
    }
}
