//! Response formatting shared by every service: size cap and status labels.
//! UI-behaviour tests.

use serde_json::json;
use services_ui::console::Format;
use services_ui::format::{
    MAX_RESPONSE_CHARS, check_response, console_response, duration, formatted_response, outcome,
};

#[test]
fn format_overrides_the_table_layout_but_never_hides_an_error() {
    let rows = json!({"columns":["id"],"rows":[{"id":1}]});
    let shown = |vertical, format| formatted_response("mysql", &rows, vertical, format, "", "");
    assert_eq!(
        shown(true, Some(Format::Table)),
        check_response("mysql", &rows, false, "", "")
    );
    assert_eq!(
        shown(false, Some(Format::Vertical)),
        check_response("mysql", &rows, true, "", "")
    );
    assert_eq!(
        shown(true, None),
        check_response("mysql", &rows, true, "", "")
    );
    assert_eq!(
        shown(false, Some(Format::Json)),
        "[\n  {\n    \"id\": 1\n  }\n]"
    );
    let failed = json!({"columns":["id"],"rows":[],"error":"denied"});
    assert_eq!(
        formatted_response("mysql", &failed, false, Some(Format::Json), "", ""),
        "denied"
    );
}

#[test]
fn long_responses_are_capped_without_splitting_unicode() {
    let r = json!({"body":"é".repeat(MAX_RESPONSE_CHARS+4)});
    let out = console_response("rest", &r, false);
    assert!(out.contains("4 more characters"));
    assert_eq!(
        out.chars().take(MAX_RESPONSE_CHARS).collect::<String>(),
        "é".repeat(MAX_RESPONSE_CHARS)
    );
}

#[test]
fn outcomes_and_durations_use_readable_labels() {
    // The module's own verdict wins over the generic reading of the answer.
    assert_eq!(outcome(&json!({"status":0,"outcome":"OK"})), "OK");
    assert_eq!(outcome(&json!({"status":404})), "WARNING");
    assert_eq!(
        outcome(&json!({"status":200,"outcome":"WARNING"})),
        "WARNING"
    );
    assert_eq!(outcome(&json!({"status":503})), "ERROR");
    assert_eq!(outcome(&json!({"rows":[]})), "NOTICE");
    // Runs the console ends itself carry no module verdict.
    assert_eq!(outcome(&json!({"error":"offline","outcome":"OK"})), "ERROR");
    assert_eq!(
        outcome(&json!({"cancelled":true,"outcome":"OK"})),
        "Stopped"
    );
    assert_eq!(duration(1500.0), "1.5s");
    assert_eq!(duration(60000.0), "1min");
}
