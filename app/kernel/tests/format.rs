//! Response formatting shared by every service: size cap and status labels.
//! UI-behaviour tests.

use serde_json::json;
use services_kernel::requests::format::{
    MAX_RESPONSE_CHARS, check_response, console_response, outcome,
};

#[test]
fn long_responses_are_capped_without_splitting_unicode() {
    let r = json!({"body":"é".repeat(MAX_RESPONSE_CHARS+4)});
    let out = console_response(&r);
    assert!(out.contains("4 more characters"));
    assert_eq!(
        out.chars().take(MAX_RESPONSE_CHARS).collect::<String>(),
        "é".repeat(MAX_RESPONSE_CHARS)
    );
}

#[test]
fn outcomes_use_readable_labels() {
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
}

#[test]
fn a_stopped_run_keeps_what_it_showed_and_an_error_replaces_the_body() {
    let stopped = json!({"cancelled":true,"warning":"Stopped.","body":"partial"});
    assert_eq!(console_response(&stopped), "partial\nWarning: Stopped.");
    assert_eq!(
        console_response(&json!({"cancelled":true})),
        "Request stopped before dispatch."
    );
    assert_eq!(
        check_response(&json!({"error":"offline","body":"x"}), "", ""),
        "offline"
    );
    assert_eq!(
        check_response(&serde_json::Value::Null, "so far", ""),
        "so far"
    );
}
