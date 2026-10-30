//! History panel: saved snapshots keep typed result metadata and render
//! partial, plain and large responses intact. UI-behaviour tests.

use serde_json::json;
use services_ui::format::{MAX_RESPONSE_CHARS, history_document, history_snapshot, table};

#[test]
fn typed_history_keeps_result_metadata_and_exact_wrapped_text() {
    let result = json!({"columns":["payload"],"columnTypes":{"payload":"json"},"columnSizes":{"payload":8},
        "rows":[{"payload":"{\"n\":12,\"ok\":true}"}]});
    let source = format!(
        "#check saved\n#endpoint\nhttps://fixture.invalid\n#search\nindex=fixture\n\n#responce OK\n{result}"
    );
    let (text, regions) = history_document("splunk", &source, "1:00:00 PM", true).unwrap();
    assert!(text.ends_with(&table(&result, false)));
    assert_eq!(regions.len(), 1);
    assert_eq!(
        regions[0].table.as_ref().unwrap().types["payload"],
        services_contracts::columns::Kind::Json
    );
    assert!(
        text.lines()
            .nth(regions[0].header)
            .unwrap()
            .starts_with("#responce")
    );
    assert_eq!(regions[0].end, text.split('\n').count());
}

#[test]
fn saved_vertical_query_renders_its_grid_vertically() {
    let result = json!({"columns":["n"],"rows":[{"n":1}],"resultCount":1});
    for (query, vertical) in [("SELECT 1\\G", true), ("SELECT 1", false)] {
        let source = format!(
            "#check saved\n#endpoint\nmysql://root@127.0.0.1/db\n#query\n{query}\n\n#responce OK\n{result}"
        );
        let (text, _) = history_document("mysql", &source, "1:00:00 PM", true).unwrap();
        assert!(text.ends_with(&table(&result, vertical)), "{query}: {text}");
    }
}

#[test]
fn history_display_handles_partial_plain_and_large_responses_without_truncating() {
    let prefix = "#check example";
    let partial = format!("{prefix}\n\n#responce OK\n{{\"debug\":");
    assert_eq!(
        history_snapshot("graphql", &partial, "1:00:00 PM", false).unwrap(),
        format!("{prefix}\n#responce 1:00:00 PM · OK\n{{\"debug\":")
    );
    let plain = "line one\n\n#response error\nThis is part of the actual response.";
    let snapshot = format!("{prefix}\n\n#responce OK\n{plain}");
    assert!(
        history_snapshot("rest", &snapshot, "1:00:00 PM", true)
            .unwrap()
            .ends_with(plain)
    );
    let large = format!("{{\"value\":\"{}\"}}", "é".repeat(MAX_RESPONSE_CHARS));
    let snapshot = format!("{prefix}\n\n#responce OK\n{large}");
    let display = history_snapshot("rest", &snapshot, "1:00:00 PM", true).unwrap();
    assert!(display.ends_with(&large));
    assert!(!display.contains("response truncated"));
    for outcome in ["WARNING", "ERROR", "NOTICE", "Stopped"] {
        let snapshot = format!("{prefix}\n\n#responce {outcome}\n{{\"debug\":true}}");
        let display = history_snapshot("graphql", &snapshot, "1:00:00 PM", true).unwrap();
        assert!(display.contains(&format!("#responce 1:00:00 PM · {outcome}")));
        assert!(display.ends_with("{\n  \"debug\": true\n}"));
    }
}
