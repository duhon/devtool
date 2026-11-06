//! Variables panel (profiles): editing, hints, reference resolution and hover.
//! UI-behaviour tests.

use serde_json::json;
use services_ui::profiles::{format, hints, hover_at, parse, parse_checked, resolve_text};

#[test]
fn checked_editor_does_not_silently_drop_invalid_lines() {
    assert_eq!(
        parse_checked("# note\nURL: https://a:443\nEMPTY:").unwrap(),
        parse("# note\nURL: https://a:443\nEMPTY:")
    );
    for text in ["broken", ": value", "X: one\nX: two"] {
        assert!(parse_checked(text).is_err(), "{text}");
    }
}

#[test]
fn editing_keeps_comments_and_colons_in_values() {
    let s = "# note\nURL: https://a:443\nX: one";
    assert_eq!(format(&parse(s)), s);
}

#[test]
fn hints_follow_profile_order_and_env() {
    let state = json!({"order":["b","a"],"profiles":{"a":{"X":"a"},"b":{"X":"b","#comment":""}},"env":{"HOME":"/home"}});
    let h = hints(&state);
    assert!(h.contains(&("{{:X}}".into(), "b".into())));
    assert!(!h.iter().any(|(k, _)| k.contains("#")));
    assert!(h.contains(&("{{env:HOME}}".into(), "/home".into())));
}

#[test]
fn reference_resolution_uses_known_values_and_rejects_unresolved_templates() {
    let hints = hints(&json!({
        "order":["var"],
        "profiles":{"var":{"HOST":"fixture.invalid","NESTED":"{{other:HOST}}","EMPTY":""}},
        "env":{"ROOT":"/rest/default"}
    }));
    assert_eq!(
        resolve_text("http://{{var:HOST}}{{env:ROOT}}/{{:HOST}}", &hints).unwrap(),
        "http://fixture.invalid/rest/default/fixture.invalid"
    );
    assert_eq!(resolve_text("{{var:EMPTY}}", &hints).unwrap(), "");
    assert_eq!(resolve_text("literal", &hints).unwrap(), "literal");
    for text in ["{{var:HOST", "{{missing:HOST}}", "{{var:NESTED}}"] {
        assert!(resolve_text(text, &hints).is_err(), "{text}");
    }
}

#[test]
fn hover_matches_the_whole_reference_at_utf16_positions() {
    let state = json!({
        "order": ["default", "second"],
        "profiles": {
            "default": {"TOKEN": "first", "REF": "{{second:TOKEN}}"},
            "second": {"TOKEN": "final"}
        },
        "resolvedProfiles": {
            "default": {"TOKEN": "first", "REF": "final"},
            "second": {"TOKEN": "final"}
        },
        "env": {"PROBE": "companion"}
    });
    let hints = hints(&state);
    let line = "VALUE: \u{1f642} {{second:TOKEN}}{{default:REF}} {{:TOKEN}} {{env:PROBE}}.";
    for (key, expected) in [
        ("{{second:TOKEN}}", "final"),
        ("{{default:REF}}", "final"),
        ("{{:TOKEN}}", "first"),
        ("{{env:PROBE}}", "companion"),
    ] {
        let start = line[..line.find(key).unwrap()].encode_utf16().count();
        let end = start + key.encode_utf16().count();
        for ch in start..end {
            let hover = hover_at(line, ch, &hints).unwrap();
            assert_eq!((hover.start, hover.end), (start, end));
            assert_eq!((hover.key, hover.value), (key, expected));
        }
    }
    assert!(hover_at(line, 0, &hints).is_none());
    assert!(hover_at(line, line.encode_utf16().count() - 1, &hints).is_none());
    assert!(hover_at("{{unknown:TOKEN}}", 5, &hints).is_none());
    assert!(hover_at("{{second:TOKEN", 5, &hints).is_none());
}
