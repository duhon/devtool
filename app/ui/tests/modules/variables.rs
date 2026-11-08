//! Hover over variable references in the console editor.
//! UI-behaviour tests.

use services_ui::profiles::hover_at;

#[test]
fn hover_matches_the_whole_reference_at_utf16_positions() {
    let hints: Vec<(String, String)> = [
        ("{{second:TOKEN}}", "final"),
        ("{{default:REF}}", "final"),
        ("{{:TOKEN}}", "first"),
        ("{{env:PROBE}}", "companion"),
    ]
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .into();
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
