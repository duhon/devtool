//! Splunk module: options-only parameters, endpoint completion, SPL coloring
//! and preview labels. UI-behaviour tests.

use crate::support::tokens;
use serde_json::{Value, json};
use services_ui::completion::suggest;
use services_ui::console::parse;
use services_ui::format::{check_response, history_snapshot};
use services_ui::syntax::{Language, State};

#[test]
fn rejects_removed_sections_instead_of_sending_them_as_spl() {
    for marker in ["from", "to", "app", "count", "preview", "offset"] {
        for spelling in [
            format!("#{marker}"),
            format!("  #{}  ", marker.to_uppercase()),
            format!("#{marker} @template"),
        ] {
            for options in [
                "",
                "#options\nFrom: -24h\nTo: now\nApp: search\nCount: 30\n",
            ] {
                let source = format!(
                    "#endpoint\nhttps://splunk.fixture\n{options}#search\nindex=main\n{spelling}\nvalue"
                );
                assert_eq!(
                    parse("splunk", &source).unwrap_err(),
                    format!("Unsupported Splunk section #{marker}; use #options.")
                );
            }
        }
    }
}

#[test]
fn reads_parameters_only_from_options() {
    assert_eq!(
        parse(
            "splunk",
            "#endpoint\nhttps://splunk.fixture\n#options\nFrom: -24h\nTo: -5m\nApp: search\nCount: 30\nPreview: true\nOffset: 10\n#search\nindex=main"
        )
        .unwrap(),
        json!({
            "baseUrl": "https://splunk.fixture", "headers": {}, "search": "index=main",
            "earliestTime": "-24h", "latestTime": "-5m", "app": "search", "count": "30",
            "preview":"true","offset":"10"
        })
    );
    assert_eq!(
        parse(
            "splunk",
            "#endpoint\nhttps://splunk.fixture\n#search\nindex=main"
        )
        .unwrap(),
        json!({"baseUrl": "https://splunk.fixture", "headers": {}, "search": "index=main"})
    );
}

#[test]
fn suggests_only_options_and_its_parameter_keys() {
    let hints = suggest("splunk", "", "#check Splunk\n#", 1, 1, &[], &Value::Null);
    assert_eq!(
        hints
            .candidates
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect::<Vec<_>>(),
        ["#endpoint", "#headers", "#options", "#search", "#check"]
    );
    let hints = suggest(
        "splunk",
        "",
        "#check Splunk\n#options\n",
        2,
        0,
        &[],
        &Value::Null,
    );
    assert_eq!(
        hints
            .candidates
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect::<Vec<_>>(),
        [
            "From: -24h",
            "To: now",
            "App: search",
            "Count: 30",
            "Preview: false",
            "Offset: 0",
            "Columns-Type: ",
            "Columns-Size: "
        ]
    );
}

#[test]
fn endpoint_only_offers_endpoints_not_headers_or_searches() {
    let hints = vec![
        ("Authorization: fixture".into(), "Header".into()),
        ("index=http".into(), "SPL".into()),
        ("https://splunk.example.test".into(), "Endpoint".into()),
    ];
    let r = suggest("splunk", "endpoint", "http", 0, 4, &hints, &Value::Null);
    assert!(!r.candidates.is_empty());
    assert!(r.candidates.iter().all(|c| c.text.contains("://")));
    assert!(
        r.candidates
            .iter()
            .any(|c| c.text == "https://splunk.example.test")
    );
}

#[test]
fn spl_comments_and_command_boundaries() {
    let mut state = State::default();
    let t = tokens(
        Language::Spl,
        "index=main | stats count by host",
        &mut state,
    );
    assert!(t.contains(&("index".into(), Some("attribute"))));
    assert!(t.contains(&("stats".into(), Some("keyword"))));
    tokens(Language::Spl, "```note", &mut state);
    assert!(state.fenced);
    assert_eq!(
        tokens(Language::Spl, "other```", &mut state)[0].1,
        Some("comment")
    );
    assert!(!state.fenced);
}

#[test]
fn preview_is_labeled_in_console_and_saved_history() {
    let result = json!({"preview":true,"columns":["n"],"rows":[{"n":31}]});
    let body = check_response("splunk", &result, false, "", "");
    assert!(body.starts_with(services_contracts::SPLUNK_PREVIEW_WARNING));
    assert!(body.contains("| 31 |"));
    let source = format!(
        "#check preview\n#endpoint\nhttps://fixture.invalid\n#options\nPreview: true\nOffset: 30\n#search\nindex=fixture\n\n#responce WARNING\n{result}"
    );
    let saved = history_snapshot("splunk", &source, "1:00:00 PM", true).unwrap();
    assert!(saved.ends_with(&body));
    assert!(saved.contains("· WARNING"));
    let mut final_result = result;
    final_result["preview"] = json!(false);
    assert!(!check_response("splunk", &final_result, false, "", "").contains("provisional"));
}

#[test]
fn options_use_literal_column_names_and_reject_css_sizes() {
    let splunk = parse(
        "splunk",
        "#endpoint\nhttps://fixture.invalid\n#options\nColumns-Type: message.timestamp=datetime message.message=json\nColumns-Size: message.message=100\n#search\nindex=fixture",
    )
    .unwrap();
    assert_eq!(
        splunk["columnsType"],
        "message.timestamp=datetime message.message=json"
    );
    assert_eq!(splunk["columnsSize"], "message.message=100");
    assert!(
        parse(
            "splunk",
            "#endpoint\nhttps://fixture.invalid\n#options\nColumns-Size: message=100px\n#search\nindex=fixture"
        )
        .is_err()
    );
}
