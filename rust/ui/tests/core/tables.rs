//! Result tables: plain copyable text, JSON cells and width limits.
//! UI-behaviour tests.

use serde_json::{Value, json};
use services_contracts::columns::Kind;
use services_ui::tables::{cell, render};

#[test]
fn ordinary_table_remains_plain_copyable_text() {
    assert_eq!(
        render(
            &json!({"columns":["id","name"],"rows":[{"id":1,"name":"abc"},{"id":2,"name":"x"}]}),
            false
        ),
        "+----+------+\n| id | name |\n+----+------+\n| 1  | abc  |\n| 2  | x    |\n+----+------+"
    );
}

#[test]
fn json_is_compact_in_tables_and_pretty_only_when_typed_and_vertical() {
    let mut result = json!({"columns":["feed"],"rows":[{"feed":"{\n \"a\": [1,true]\n}"}],"columnTypes":{"feed":"json"}});
    let horizontal = render(&result, false);
    assert!(horizontal.contains("| {\"a\":[1,true]} |"), "{horizontal}");
    let vertical = render(&result, true);
    assert!(
        vertical.contains(
            "feed: {\n        \"a\": [\n          1,\n          true\n        ]\n      }"
        ),
        "{vertical}"
    );
    result["columnTypes"]["feed"] = json!("text");
    assert!(render(&result, true).ends_with("feed: {\n \"a\": [1,true]\n}"));
    assert!(render(&result, false).contains(r#"{\n "a": [1,true]\n}"#));
    assert_eq!(cell(&json!("not JSON"), Kind::Json, true), "not JSON");
    assert_eq!(cell(&Value::Null, Kind::Json, true), "NULL");
    assert_eq!(cell(&json!("null"), Kind::Json, true), "null");
}

#[test]
fn character_limits_wrap_headers_and_unicode_without_truncating_data() {
    let result = json!({"columns":["message.message","n"],"columnSizes":{"message.message":3},
        "rows":[{"message.message":"аб😀вгде","n":12},{"message.message":"xy","n":34}]});
    let table = render(&result, false);
    assert_eq!(
        table,
        "+-----+----+\n| mes | n  |\n| sag |    |\n| e.m |    |\n| ess |    |\n| age |    |\n+-----+----+\n| аб😀 | 12 |\n| вгд |    |\n| е   |    |\n+-----+----+\n| xy  | 34 |\n+-----+----+"
    );
    assert!(table.lines().all(|line| line.chars().count() == 12));
    let narrow = json!({"columns":["x"],"columnSizes":{"x":1},"rows":[{"x":"a\nb\tc"}]});
    assert_eq!(
        render(&narrow, false)
            .lines()
            .filter(|l| l.starts_with("| "))
            .count(),
        8
    );
}
