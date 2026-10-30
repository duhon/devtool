//! Syntax coloring of result tables (typed cells, wrapped JSON, borders).
//! UI-behaviour tests.

use crate::support::tokens;
use services_ui::syntax::{Language, State, table_line};
use services_ui::tables;

#[test]
fn typed_tables_color_cells_without_guessing_from_text_or_pipes() {
    let result = serde_json::json!({"columns":["n","flag","at","payload","text"],
        "columnTypes":{"n":"number","flag":"boolean","at":"datetime","payload":"json"},
        "rows":[{"n":"123.000","flag":true,"at":"2026-10-27","payload":"{\"x\":\"a|b\",\"v\":12}","text":"{\"v\":99}"}]});
    let mut state = State {
        table: tables::Metadata::from_result(&result),
        ..Default::default()
    };
    let table = tables::render(&result, false);
    let all: Vec<_> = table
        .lines()
        .flat_map(|line| tokens(Language::Table, line, &mut state))
        .collect();
    assert!(
        all.iter()
            .any(|(s, c)| s.contains("123.000") && *c == Some("number"))
    );
    assert!(
        all.iter()
            .any(|(s, c)| s.contains("true") && *c == Some("atom"))
    );
    assert!(
        all.iter()
            .any(|(s, c)| s.contains("2026-10-27") && *c == Some("string"))
    );
    assert!(all.contains(&("\"a|b\"".into(), Some("string"))));
    assert!(all.contains(&("12".into(), Some("number"))));
    assert!(
        all.iter()
            .any(|(s, c)| s.contains("{\"v\":99}") && c.is_none())
    );
    assert!(
        all.iter()
            .filter(|(_, c)| *c == Some("mysql-table-border"))
            .all(|(s, _)| !s.contains("a|b"))
    );
}

#[test]
fn wrapped_json_and_vertical_json_keep_flat_token_state() {
    for vertical in [false, true] {
        let result = serde_json::json!({"columns":["payload","text"],"columnTypes":{"payload":"json"},
            "columnSizes":{"payload":7},"rows":[{"payload":"{\"s\":\"abc def ghij\",\"v\":true}","text":"plain"}]});
        let mut state = State {
            table: tables::Metadata::from_result(&result),
            ..Default::default()
        };
        let text = tables::render(&result, vertical);
        let mut output = vec![];
        for line in text.lines() {
            output.extend(tokens(Language::Table, line, &mut state));
            state = serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
        }
        assert!(
            !output.iter().any(|(_, c)| *c == Some("error")),
            "{vertical}: {output:?}"
        );
        assert!(
            output
                .iter()
                .any(|(s, c)| s.contains("true") && *c == Some("atom")),
            "{vertical}: {output:?}"
        );
        assert!(
            !output
                .iter()
                .any(|(s, c)| s.contains("plain") && c.is_some()),
            "{vertical}: {output:?}"
        );
    }
}

#[test]
fn line_lexer_matches_tokens_and_uses_utf16_without_retaining_per_token_state() {
    let result = serde_json::json!({"columns":["j","n"],"columnTypes":{"j":"json","n":"number"},
        "columnSizes":{"j":9},"rows":[{"j":r#"{"s":"😀a\"b","v":[true,false,null]}"#,"n":42}]});
    for vertical in [false, true] {
        let mut reference = State {
            table: tables::Metadata::from_result(&result),
            ..Default::default()
        };
        let mut batched = reference.clone();
        for line in tables::render(&result, vertical).lines() {
            let expected = tokens(Language::Table, line, &mut reference);
            let spans = table_line(line, &mut batched);
            let mut start = 0;
            let actual: Vec<_> = spans
                .into_iter()
                .map(|(end, style)| {
                    let text =
                        String::from_utf16(&line.encode_utf16().collect::<Vec<_>>()[start..end])
                            .unwrap();
                    start = end;
                    (text, style)
                })
                .collect();
            assert_eq!(actual, expected);
            assert_eq!(start, line.encode_utf16().count());
        }
    }
}

#[test]
fn json_escapes_atoms_and_numbers_survive_every_narrow_wrap() {
    for width in 1..=16 {
        let result = serde_json::json!({"columns":["j"],"columnTypes":{"j":"json"},"columnSizes":{"j":width},
            "rows":[{"j":r#"{"s":"a\"b\\c | 😀","v":[true,false,null,-1.25e+10]}"#}]});
        let mut state = State {
            table: tables::Metadata::from_result(&result),
            ..Default::default()
        };
        let all: Vec<_> = tables::render(&result, false)
            .lines()
            .flat_map(|line| tokens(Language::Table, line, &mut state))
            .collect();
        assert!(
            !all.iter().any(|(_, c)| *c == Some("error")),
            "width {width}: {all:?}"
        );
    }
}

#[test]
fn table_values_are_not_colored_as_sql() {
    let t = tokens(Language::Table, "| SELECT | true |", &mut State::default());
    assert!(
        t.iter()
            .filter(|(_, s)| s.is_some())
            .all(|(_, s)| *s == Some("mysql-table-border"))
    );
}
