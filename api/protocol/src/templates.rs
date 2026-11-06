use crate::manifest::Manifest;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type Library = BTreeMap<String, BTreeMap<String, String>>;

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

/// Whether `service` has a template section `section` (`check` or a console section).
pub fn has_section(manifests: &[Manifest], service: &str, section: &str) -> bool {
    manifests
        .iter()
        .find(|manifest| manifest.id == service)
        .is_some_and(|manifest| manifest.template_sections().any(|name| name == section))
}

/// Checks every template text. Sections of services or sections no loaded module has are
/// kept as they are: a module may be removed, fail to load or come back in another version.
pub fn validate(library: &Library) -> Result<(), String> {
    for entries in library.values() {
        for text in entries.values() {
            parse(text)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_manifest_sections_are_template_sections() {
        let manifests = [Manifest {
            id: "splunk".into(),
            console: crate::manifest::Console {
                sections: ["endpoint", "headers", "options", "search"]
                    .map(|name| crate::manifest::Section {
                        name: name.into(),
                        ..Default::default()
                    })
                    .into(),
                ..Default::default()
            },
            ..Default::default()
        }];
        for section in ["check", "endpoint", "headers", "options", "search"] {
            assert!(has_section(&manifests, "splunk", section));
        }
        for section in ["from", "to", "app", "count"] {
            assert!(!has_section(&manifests, "splunk", section));
        }
        assert!(!has_section(&manifests, "redis", "check"));
    }
    #[test]
    fn templates_of_unloaded_services_and_sections_are_kept_but_still_checked() {
        let library = BTreeMap::from([
            (
                "splunk".into(),
                BTreeMap::from([("from".into(), "@old Existing template\nvalue".into())]),
            ),
            (
                "redis".into(),
                BTreeMap::from([("check".into(), "@x\nGET".into())]),
            ),
        ]);
        assert_eq!(validate(&library), Ok(()));
        let broken = BTreeMap::from([(
            "redis".into(),
            BTreeMap::from([("check".into(), "x".into())]),
        )]);
        assert!(validate(&broken).unwrap_err().starts_with("Line 1"));
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
