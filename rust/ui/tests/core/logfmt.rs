//! Log line parsing for response tables (logfmt, Java-style nesting, JSON).
//! UI-behaviour tests.

use serde_json::json;
use services_ui::logfmt::parse;

#[test]
fn plain_text_is_left_alone_and_key_values_are_typed() {
    assert!(parse("ordinary text a=b").is_none());
    assert_eq!(
        parse("INFO id=3 ok=true").unwrap(),
        json!({"text":"INFO","id":3,"ok":true})
    );
}

#[test]
fn java_style_maps_and_json_values_become_nested_objects() {
    assert_eq!(
        parse("context={storeViewCode=default,categoryId=[2,3],nested={a: 1}}").unwrap(),
        json!({"context":{"storeViewCode":"default","categoryId":[2,3],"nested":{"a":1}}})
    );
    assert_eq!(
        parse("a=1 metadata={\"x\":\"y\"}").unwrap()["metadata"],
        json!({"x":"y"})
    );
}

#[test]
fn anonymous_maps_become_context_and_unbalanced_maps_are_rejected() {
    let parsed = parse("id=1 type=event {time={\"$java\":2026-09-26T04:12:36Z}}").unwrap();
    assert_eq!(parsed["context"]["time"], "2026-09-26T04:12:36Z");
    assert!(parse("a={broken").is_none());
}
