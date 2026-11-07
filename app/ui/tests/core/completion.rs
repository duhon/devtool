//! Service-agnostic Console completion: markers, separators, variables,
//! headers, JSON snippets and UTF-16 offsets. UI-behaviour tests.

use services_ui::completion::{prepare_insertion, suggest};
use services_ui::console::{self, fuzzy_match};
use services_ui::services;

#[test]
fn fuzzy_matching_accepts_ordered_subsequences_only() {
    assert!(fuzzy_match("Content-Type", "cty"));
    assert!(!fuzzy_match("Content-Type", "xyz"));
}

#[test]
fn empty_console_is_safe_and_offers_request() {
    assert!(console::request_at("", 0).is_none());
    for service in services::ids().iter().map(String::as_str) {
        for source in ["", "#"] {
            let hints = suggest(service, "", source, 0, source.len(), &[]);
            assert_eq!(
                hints
                    .candidates
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["#check"],
                "{service}: {source}"
            );
        }
    }
}

#[test]
fn newline_after_request_header_offers_unused_sections() {
    for service in services::ids().iter().map(String::as_str) {
        for source in ["#check\n", "#check my query\n", "#check my query\n\n"] {
            let hints = suggest(service, "", source, source.split('\n').count() - 1, 0, &[]);
            assert_eq!(
                hints
                    .candidates
                    .iter()
                    .map(|candidate| candidate.text.clone())
                    .collect::<Vec<_>>(),
                console::markers(service)
                    .iter()
                    .map(String::as_str)
                    .chain(std::iter::once("check"))
                    .map(|marker| format!("#{marker}"))
                    .collect::<Vec<_>>(),
                "{service}: {source}"
            );
        }
    }
    let source = "#check my query\n\n#endpoint\nmysql://db\n";
    let hints = suggest("mysql", "", source, 1, 0, &[]);
    assert_eq!(
        hints
            .candidates
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect::<Vec<_>>(),
        vec!["#options", "#query", "#check"]
    );
}

#[test]
fn hints_editor_offers_sections_missing_from_its_text() {
    for service in services::ids() {
        let markers = console::markers(&service);
        let Some(first) = markers.first() else {
            continue;
        };
        let texts = |source: &str| {
            let current = source.rsplit('\n').next().unwrap_or("");
            suggest(
                "hints",
                &service,
                source,
                source.split('\n').count() - 1,
                current.len(),
                &[],
            )
            .candidates
            .into_iter()
            .map(|candidate| candidate.text)
            .collect::<Vec<_>>()
        };
        let missing: Vec<_> = markers[1..].iter().map(|m| format!("#{m}")).collect();
        assert_eq!(texts(&format!("#{first}\nvalue\n#")), missing, "{service}");
        assert!(texts("##").is_empty(), "{service}");
        assert!(texts("value").is_empty(), "{service}");
    }
}

#[test]
fn non_service_editors_never_offer_console_or_service_syntax() {
    let hints = vec![
        ("#check".into(), String::new()),
        ("Accept: */*".into(), String::new()),
        ("GET".into(), String::new()),
        ("SELECT".into(), String::new()),
        ("query".into(), String::new()),
        ("{{prod:TOKEN}}".into(), "fixture".into()),
    ];
    for (service, field) in [("vars", "variables"), ("", "")] {
        for source in [
            "",
            "#",
            "Ac",
            "GET",
            "SEL",
            "qu",
            "NAME: ",
            "#check example\n",
            "#check example\n#headers\n",
        ] {
            let result = suggest(
                service,
                field,
                source,
                source.split('\n').count() - 1,
                source
                    .split('\n')
                    .next_back()
                    .unwrap()
                    .encode_utf16()
                    .count(),
                &hints,
            );
            assert!(result.candidates.is_empty(), "{service}: {source}");
        }
        let result = suggest(service, field, "NAME: {{prod:TO", 0, 15, &hints);
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].text, "{{prod:TOKEN}}");
    }
}

#[test]
fn marker_hints_offer_only_unused_sections_and_new_checks() {
    let source = "#check\n#endpoint\nmysql://db\n#";
    let r = suggest("mysql", "", source, 3, 1, &[]);
    assert!(r.candidates.iter().any(|c| c.text == "#query"));
    assert!(!r.candidates.iter().any(|c| c.text == "#endpoint"));
    assert!(r.candidates.iter().any(|c| c.text == "#check"));
}

#[test]
fn separator_hints_are_repeatable_and_scoped_to_each_request() {
    for service in services::ids().iter().map(String::as_str) {
        let filled = console::markers(service)
            .iter()
            .map(|marker| format!("#{marker}\nvalue\n"))
            .collect::<String>();
        let first = format!("#check first\n{filled}");
        let source = format!("{first}#");
        let line = source.split('\n').count() - 1;
        let result = suggest(service, "", &source, line, 1, &[]);
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            ["#check"],
            "{service}"
        );
        let source = format!("{first}#check second\n#\n#check third\n{filled}");
        let line = first.split('\n').count();
        let result = suggest(service, "", &source, line, 1, &[]);
        let expected = console::markers(service)
            .iter()
            .map(String::as_str)
            .chain(std::iter::once("check"))
            .map(|marker| format!("#{marker}"))
            .collect::<Vec<_>>();
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|candidate| candidate.text.clone())
                .collect::<Vec<_>>(),
            expected,
            "{service}"
        );
        let source = format!("{first}#check second\n#endpoint\nvalue\n#\n#check third\n{filled}");
        let line = first.split('\n').count() + 2;
        let result = suggest(service, "", &source, line, 1, &[]);
        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "#check")
        );
        assert!(
            !result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "#endpoint")
        );
        assert_eq!(result.candidates.len(), console::markers(service).len());
    }
}

#[test]
fn markers_advance_to_their_value_but_request_leaves_room_for_a_title() {
    assert_eq!(prepare_insertion("#check", true), ("#check ".into(), None));
    for service in services::ids().iter().map(String::as_str) {
        for marker in console::markers(service) {
            let marker = format!("#{marker}");
            assert_eq!(
                prepare_insertion(&marker, true),
                (format!("{marker}\n"), None)
            );
        }
    }
    assert_eq!(
        prepare_insertion("#endpoint", false),
        ("#endpoint".into(), None)
    );
}

#[test]
fn variable_placeholders_replace_the_existing_closing_braces() {
    let r = suggest(
        "rest",
        "body",
        "{{:TOK}}",
        0,
        6,
        &[("{{:TOKEN}}".into(), "value".into())],
    );
    assert_eq!(r.to, 8);
    assert_eq!(r.from, 0);
    assert_eq!(r.candidates[0].text, "{{:TOKEN}}");
}

#[test]
fn profiles_then_scoped_variables() {
    let hints = vec![
        ("{{prod:TOKEN}}".into(), "secret".into()),
        ("{{other:HOST}}".into(), "host".into()),
        ("{{:TOKEN}}".into(), "global".into()),
    ];
    for (service, service_field) in [
        ("mysql", "query"),
        ("rest", "endpoint"),
        ("vars", "variables"),
    ] {
        for field in [service_field, ""] {
            let profiles = suggest(service, field, "{{", 0, 2, &hints);
            assert!(profiles.candidates.iter().any(|c| c.text == "{{prod:"));
            assert!(profiles.candidates.iter().any(|c| c.text == "{{:"));
            assert!(!profiles.candidates.iter().any(|c| c.text.ends_with("}}")));
            let variables = suggest(service, field, "{{prod:TK", 0, 9, &hints);
            assert_eq!(variables.candidates.len(), 1);
            assert_eq!(variables.candidates[0].text, "{{prod:TOKEN}}");
        }
    }
}

#[test]
fn repeat_until_is_offered_under_a_repeat() {
    let options = |lines: &str| -> Vec<String> {
        let source = format!("#check a\n#endpoint\nGET https://a\n#options\n{lines}");
        let line = source.lines().count();
        suggest("rest", "", &format!("{source}\n"), line, 0, &[])
            .candidates
            .into_iter()
            .map(|candidate| candidate.text)
            .filter(|text| !text.starts_with('#'))
            .collect()
    };
    assert_eq!(options("X-Debug: 1"), ["Repeat: 5s"]);
    assert_eq!(options("Repeat: 1m"), ["Repeat-Until: changed"]);
}
