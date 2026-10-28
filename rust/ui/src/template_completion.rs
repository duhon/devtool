use crate::{
    completion::{Candidate, Completion},
    console,
};
use std::collections::BTreeMap;

pub fn suggest(
    service: &str,
    field: &str,
    source: &str,
    line: usize,
    ch: usize,
    templates: &BTreeMap<String, String>,
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
    let reference = before.rfind('@');
    let (prefix, query) = match reference {
        Some(start) => (&before[..start], &before[start + 1..]),
        None if before.ends_with(char::is_whitespace) => (before.as_str(), ""),
        None => return None,
    };
    if !query
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return None;
    }
    let section = prefix.trim().strip_prefix('#')?;
    if !services_contracts::templates::sections(service).contains(&section) {
        return None;
    }
    let entries = services_contracts::templates::parse(templates.get(section)?).ok()?;
    let candidates: Vec<_> = entries
        .into_iter()
        .filter(|entry| console::fuzzy_match(&entry.name, query))
        .take(100)
        .map(|entry| {
            let text = if section == "check"
                && !entry.body.lines().next().is_some_and(console::is_check)
            {
                let title = if entry.description.is_empty() {
                    &entry.name
                } else {
                    &entry.description
                };
                format!("#check {title}\n{}", entry.body)
            } else if section != "check" {
                format!("\n{}", entry.body)
            } else {
                entry.body
            };
            Candidate {
                text,
                label: Some(format!("@{}", entry.name)),
                detail: entry.description,
                literal: true,
                continue_hint: false,
            }
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let from = if section == "check" {
        0
    } else {
        prefix.trim_end().encode_utf16().count()
    };
    let after = &current[before.len()..];
    let (after, reference_length) = match after.strip_prefix('@') {
        Some(suffix) if reference.is_none() => (suffix, 1),
        _ => (after, 0),
    };
    let suffix: String = after
        .chars()
        .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .collect();
    Some(Completion {
        from,
        to: ch + reference_length + suffix.encode_utf16().count(),
        candidates,
        comma: None,
        automatic: true,
        automatic_directives_only: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inserts_only_body_and_normalizes_inline_sections() {
        let templates = BTreeMap::from([(
            "body".into(),
            "@my_create_book comment\n{\n  \"id\": 123\n}\n@my_create_book2 second\n{\"id\":1,}"
                .into(),
        )]);
        let completion = suggest("rest", "", "#check test\n#body @", 1, 7, &templates).unwrap();
        assert_eq!((completion.from, completion.to), (5, 7));
        assert_eq!(completion.candidates[0].text, "\n{\n  \"id\": 123\n}");
        assert_eq!(
            completion.candidates[0].label.as_deref(),
            Some("@my_create_book")
        );
        assert!(suggest("rest", "", "#body\n@m", 1, 2, &templates).is_none());
        assert!(suggest("rest", "", "#headers\n@", 1, 1, &templates).is_none());
        assert!(suggest("rest", "", "#body\nmail@", 1, 5, &templates).is_none());
    }
    #[test]
    fn full_checks_and_unicode_are_literal() {
        let templates = BTreeMap::from([(
            "check".into(),
            "@готово full\n#check new\n#endpoint\nGET /books\n#body\n{}".into(),
        )]);
        let completion = suggest("rest", "", "#check @гот", 0, 11, &templates).unwrap();
        assert_eq!(completion.from, 0);
        assert_eq!(
            completion.candidates[0].text,
            "#check new\n#endpoint\nGET /books\n#body\n{}"
        );
        assert!(completion.candidates[0].literal);
        assert!(suggest("rest", "", "@", 0, 1, &templates).is_none());
    }

    #[test]
    fn check_header_uses_description_without_copying_template_definition() {
        let body = r#"#endpoint
PUT https://fakerestapi.azurewebsites.net/api/v1/Books/123
#headers
Content-Type: application/json
Accept: application/json
#body
{
  "id": 123,
  "title": "Example Book Title",
  "description": "Sample description of the book.",
  "pageCount": 320,
  "excerpt": "Sample excerpt from the book.",
  "publishDate": "2024-01-15T00:00:00Z"
}"#;
        let templates = BTreeMap::from([("check".into(), format!("@test1 mmm\n{body}"))]);
        let source = "#check @test1";
        let completion = suggest("rest", "", source, 0, source.len(), &templates).unwrap();
        assert_eq!((completion.from, completion.to), (0, source.len()));
        assert_eq!(completion.candidates[0].text, format!("#check mmm\n{body}"));
        assert_eq!(completion.candidates[0].label.as_deref(), Some("@test1"));
        assert_eq!(completion.candidates[0].detail, "mmm");
        assert!(suggest("rest", "", "@test1", 0, 6, &templates).is_none());
    }

    #[test]
    fn check_names_fall_back_to_template_name_and_keep_explicit_headers() {
        for (definition, expected) in [
            (
                "@test1\n#endpoint\nGET /books",
                "#check test1\n#endpoint\nGET /books",
            ),
            (
                "@test1 Create a book\n#endpoint\nGET /books",
                "#check Create a book\n#endpoint\nGET /books",
            ),
            (
                "@test1 mmm\n  #CHECK Existing\n#endpoint\nGET /books",
                "  #CHECK Existing\n#endpoint\nGET /books",
            ),
            (
                "@test1 mmm\n#request Existing\n#endpoint\nGET /books",
                "#request Existing\n#endpoint\nGET /books",
            ),
            (
                "@test1 mmm\n#checker\n#endpoint\nGET /books",
                "#check mmm\n#checker\n#endpoint\nGET /books",
            ),
        ] {
            let templates = BTreeMap::from([("check".into(), definition.into())]);
            let completion = suggest("rest", "", "#check @", 0, 8, &templates).unwrap();
            assert_eq!(completion.candidates[0].text, expected, "{definition}");
        }
    }

    #[test]
    fn templates_are_only_suggested_after_a_section_marker_on_the_same_line() {
        let templates = BTreeMap::from([("body".into(), "@example Body\n{}".into())]);
        for source in [
            "@",
            "#body\n@",
            "#body\n@exa",
            "#body\nemail@",
            "#body\ntext @",
            "#body name @",
            "#unknown @",
        ] {
            let line = source.lines().count() - 1;
            let ch = source.lines().last().unwrap().encode_utf16().count();
            assert!(
                suggest("rest", "", source, line, ch, &templates).is_none(),
                "{source}"
            );
        }
        assert!(suggest("rest", "body", "#body @", 0, 7, &templates).is_none());
        let completion = suggest("rest", "", "  #body \t@", 0, 10, &templates).unwrap();
        assert_eq!(completion.candidates[0].text, "\n{}");
        assert_eq!((completion.from, completion.to), (7, 10));
    }

    #[test]
    fn space_after_section_marker_offers_templates_without_an_at_sign() {
        let templates = BTreeMap::from([
            ("body".into(), "@example Body\n{}".into()),
            (
                "check".into(),
                "@create Create book\n#endpoint\nPUT /books\n#body\n{}".into(),
            ),
        ]);
        let completion = suggest("rest", "", "#body ", 0, 6, &templates).unwrap();
        assert!(completion.automatic);
        assert_eq!((completion.from, completion.to), (5, 6));
        assert_eq!(completion.candidates[0].text, "\n{}");
        assert_eq!(completion.candidates[0].label.as_deref(), Some("@example"));
        assert_eq!(
            suggest("rest", "", "#check ", 0, 7, &templates)
                .unwrap()
                .candidates[0]
                .text,
            "#check Create book\n#endpoint\nPUT /books\n#body\n{}"
        );
        let existing = suggest("rest", "", "#body @example", 0, 6, &templates).unwrap();
        assert_eq!((existing.from, existing.to), (5, 14));
        for source in [
            "#body",
            "#body\n ",
            "#body\ntext ",
            "#body text ",
            "#unknown ",
        ] {
            let line = source.lines().count() - 1;
            let ch = source.lines().last().unwrap().encode_utf16().count();
            assert!(
                suggest("rest", "", source, line, ch, &templates).is_none(),
                "{source}"
            );
        }
    }
}
