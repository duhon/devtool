//! Templates panel: inserting template bodies and checks from `@` and section
//! markers. UI-behaviour tests.

use services_kernel::editor::Completion;
use services_shell::language::Modules;
use std::collections::BTreeMap;

fn suggest(
    service: &str,
    field: &str,
    source: &str,
    line: usize,
    ch: usize,
    templates: &BTreeMap<String, String>,
) -> Option<Completion> {
    services_kernel::editor::templates::suggest(
        &Modules, service, field, source, line, ch, templates,
    )
}

#[test]
fn inserts_only_payload_and_normalizes_inline_sections() {
    services_shell::link();
    let templates = BTreeMap::from([(
        "payload".into(),
        "@my_create_book comment\n{\n  \"id\": 123\n}\n@my_create_book2 second\n{\"id\":1,}".into(),
    )]);
    let completion = suggest("artemis", "", "#check test\n#payload @", 1, 10, &templates).unwrap();
    assert_eq!((completion.from, completion.to), (8, 10));
    assert_eq!(completion.candidates[0].text, "\n{\n  \"id\": 123\n}");
    assert_eq!(
        completion.candidates[0].label.as_deref(),
        Some("@my_create_book")
    );
    assert!(suggest("artemis", "", "#payload\n@m", 1, 2, &templates).is_none());
    assert!(suggest("artemis", "", "#metadata\n@", 1, 1, &templates).is_none());
    assert!(suggest("artemis", "", "#payload\nmail@", 1, 5, &templates).is_none());
}

#[test]
fn full_checks_and_unicode_are_literal() {
    services_shell::link();
    let templates = BTreeMap::from([(
        "check".into(),
        "@готово full\n#check new\n#target\n/books\n#payload\n{}".into(),
    )]);
    let completion = suggest("artemis", "", "#check @гот", 0, 11, &templates).unwrap();
    assert_eq!(completion.from, 0);
    assert_eq!(
        completion.candidates[0].text,
        "#check new\n#target\n/books\n#payload\n{}"
    );
    assert!(completion.candidates[0].literal);
    assert!(suggest("artemis", "", "@", 0, 1, &templates).is_none());
}

#[test]
fn check_header_uses_description_without_copying_template_definition() {
    services_shell::link();
    let body = r#"#target
https://fakerestapi.azurewebsites.net/api/v1/Books/123
#action
PUT
#metadata
Content-Type: application/json
Accept: application/json
#payload
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
    let completion = suggest("artemis", "", source, 0, source.len(), &templates).unwrap();
    assert_eq!((completion.from, completion.to), (0, source.len()));
    assert_eq!(completion.candidates[0].text, format!("#check mmm\n{body}"));
    assert_eq!(completion.candidates[0].label.as_deref(), Some("@test1"));
    assert_eq!(completion.candidates[0].detail, "mmm");
    assert!(suggest("artemis", "", "@test1", 0, 6, &templates).is_none());
}

#[test]
fn check_names_fall_back_to_template_name_and_keep_explicit_headers() {
    services_shell::link();
    for (definition, expected) in [
        ("@test1\n#target\n/books", "#check test1\n#target\n/books"),
        (
            "@test1 Create a book\n#target\n/books",
            "#check Create a book\n#target\n/books",
        ),
        (
            "@test1 mmm\n  #CHECK Existing\n#target\n/books",
            "  #CHECK Existing\n#target\n/books",
        ),
        (
            "@test1 mmm\n#request Existing\n#target\n/books",
            "#request Existing\n#target\n/books",
        ),
        (
            "@test1 mmm\n#checker\n#target\n/books",
            "#check mmm\n#checker\n#target\n/books",
        ),
    ] {
        let templates = BTreeMap::from([("check".into(), definition.into())]);
        let completion = suggest("artemis", "", "#check @", 0, 8, &templates).unwrap();
        assert_eq!(completion.candidates[0].text, expected, "{definition}");
    }
}

#[test]
fn templates_are_only_suggested_after_a_section_marker_on_the_same_line() {
    services_shell::link();
    let templates = BTreeMap::from([("payload".into(), "@example Body\n{}".into())]);
    for source in [
        "@",
        "#payload\n@",
        "#payload\n@exa",
        "#payload\nemail@",
        "#payload\ntext @",
        "#payload name @",
        "#unknown @",
    ] {
        let line = source.lines().count() - 1;
        let ch = source.lines().last().unwrap().encode_utf16().count();
        assert!(
            suggest("artemis", "", source, line, ch, &templates).is_none(),
            "{source}"
        );
    }
    assert!(suggest("artemis", "payload", "#payload @", 0, 10, &templates).is_none());
    let completion = suggest("artemis", "", "  #payload \t@", 0, 13, &templates).unwrap();
    assert_eq!(completion.candidates[0].text, "\n{}");
    assert_eq!((completion.from, completion.to), (10, 13));
}

#[test]
fn space_after_section_marker_offers_templates_without_an_at_sign() {
    services_shell::link();
    let templates = BTreeMap::from([
        ("payload".into(), "@example Body\n{}".into()),
        (
            "check".into(),
            "@create Create book\n#target\n/books\n#action\nPUT\n#payload\n{}".into(),
        ),
    ]);
    let completion = suggest("artemis", "", "#payload ", 0, 9, &templates).unwrap();
    assert!(completion.automatic);
    assert_eq!((completion.from, completion.to), (8, 9));
    assert_eq!(completion.candidates[0].text, "\n{}");
    assert_eq!(completion.candidates[0].label.as_deref(), Some("@example"));
    assert_eq!(
        suggest("artemis", "", "#check ", 0, 7, &templates)
            .unwrap()
            .candidates[0]
            .text,
        "#check Create book\n#target\n/books\n#action\nPUT\n#payload\n{}"
    );
    let existing = suggest("artemis", "", "#payload @example", 0, 9, &templates).unwrap();
    assert_eq!((existing.from, existing.to), (8, 17));
    for source in [
        "#payload",
        "#payload\n ",
        "#payload\ntext ",
        "#payload text ",
        "#unknown ",
    ] {
        let line = source.lines().count() - 1;
        let ch = source.lines().last().unwrap().encode_utf16().count();
        assert!(
            suggest("artemis", "", source, line, ch, &templates).is_none(),
            "{source}"
        );
    }
}
