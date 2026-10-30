//! MySQL module: SQL completion, `\G` vertical mode, column options and
//! result/history formatting. UI-behaviour tests.

use serde_json::{Value, json};
use services_module_mysql_ui::parse_sql;
use services_ui::completion;
use services_ui::completion::suggest;
use services_ui::console::parse;
use services_ui::format::{check_response, history_snapshot, table};

#[test]
fn backslash_g_requests_vertical_results() {
    for sql in ["SELECT 1 \\G", "SELECT 1; \\G", "SELECT 1 \\g;"] {
        let p = parse_sql(sql, "mysql://db").unwrap();
        assert_eq!(p["sql"], "SELECT 1");
        assert_eq!(p["vertical"], true);
    }
}

#[test]
fn alias_columns_resolve_to_their_table() {
    let d = json!({"tables":[{"name":"users","columns":[{"name":"id"},{"name":"email"}]},{"name":"orders","columns":[{"name":"total"}]}]});
    let s = "SELECT * FROM users u WHERE u.em";
    let r = suggest("mysql", "query", s, 0, s.len(), &[], &d);
    assert_eq!(r.candidates[0].text, "email");
}

#[test]
fn alias_can_be_declared_after_cursor() {
    let d = json!({"tables":[{"name":"users","columns":[{"name":"email"}]},{"name":"orders","columns":[{"name":"total"}]}]});
    let source = "SELECT u.em\nFROM users u";
    let r = suggest("mysql", "query", source, 0, 11, &[], &d);
    assert_eq!(r.candidates[0].text, "email");
    assert_eq!(r.candidates.len(), 1);
}

fn schema() -> Value {
    json!({"tables":[
        {"name":"users","columns":["id","email","status"]},
        {"name":"orders","columns":["user_id","total"]},
        {"name":"UserArchive","columns":["Email"]}
    ]})
}

fn at(source: &str, line: usize, ch: usize, field: &str, schema: &Value) -> Vec<String> {
    completion::suggest("mysql", field, source, line, ch, &[], schema)
        .candidates
        .into_iter()
        .map(|candidate| candidate.text)
        .collect()
}

fn complete(source: &str) -> Vec<String> {
    let lines: Vec<_> = source.split('\n').collect();
    at(
        source,
        lines.len() - 1,
        lines.last().unwrap().encode_utf16().count(),
        "query",
        &schema(),
    )
}

#[test]
fn query_start_does_not_drown_commands_in_discovered_columns() {
    let schema = json!({"tables":[{"name":"huge","columns":(0..150).map(|i|format!("field_{i}")).collect::<Vec<_>>()}]});
    for (source, line, field) in [
        ("", 0, "query"),
        ("#check test\n#endpoint\nmysql://fixture\n#query\n", 4, ""),
    ] {
        let items = at(source, line, 0, field, &schema);
        assert!(items.contains(&"SELECT".into()));
        assert!(items.len() < 100);
        assert!(
            items
                .iter()
                .all(|item| !item.starts_with("field_") && item != "huge")
        );
    }
}

#[test]
fn joined_table_columns_are_not_buried_by_a_large_first_table() {
    let schema = json!({"tables":[
        {"name":"users","columns":(0..150).map(|i|format!("field_{i}")).collect::<Vec<_>>()},
        {"name":"orders","columns":["user_id","total"]}
    ]});
    let source = "SELECT * FROM users u JOIN orders o ON ";
    let items = at(source, 0, source.len(), "query", &schema);
    assert_eq!(items.len(), 100);
    assert!(items.contains(&"u.field_0".into()));
    assert!(items.contains(&"o.user_id".into()));
    assert!(items.contains(&"o.total".into()));
}

#[test]
fn select_star_from_table_join_sequence_is_contextual() {
    let projection = complete("SELECT ");
    assert_eq!(projection[0], "*");
    assert!(projection.contains(&"email".into()));
    assert!(!projection.contains(&"users".into()) && !projection.contains(&"FROM".into()));
    assert_eq!(complete("SELECT * "), vec!["FROM"]);
    assert_eq!(complete("SELECT *\n"), vec!["FROM"]);
    assert_eq!(
        complete("SELECT * FROM "),
        vec!["users", "orders", "UserArchive"]
    );
    let after_table = complete("SELECT * FROM users ");
    for expected in [
        "JOIN",
        "LEFT JOIN",
        "WHERE",
        "GROUP BY",
        "ORDER BY",
        "LIMIT",
    ] {
        assert!(after_table.contains(&expected.into()), "missing {expected}");
    }
    for excluded in ["users", "orders", "email", "SELECT", "SET", "VALUES"] {
        assert!(
            !after_table.contains(&excluded.into()),
            "unexpected {excluded}"
        );
    }
    assert_eq!(
        complete("SELECT * FROM users LEFT "),
        vec!["JOIN", "OUTER JOIN"]
    );
    assert_eq!(
        complete("SELECT * FROM users LEFT JOIN "),
        vec!["users", "orders", "UserArchive"]
    );
    assert!(complete("SELECT * FROM users u LEFT JOIN orders o ").contains(&"ON".into()));
    let on = complete("SELECT * FROM users u JOIN orders o ON ");
    assert!(on.contains(&"u.id".into()) && on.contains(&"o.user_id".into()));
    assert!(!on.contains(&"UserArchive.Email".into()));
    assert_eq!(
        complete("SELECT * FROM users u JOIN orders o ON o.us"),
        vec!["user_id"]
    );
    assert!(
        complete("SELECT * FROM users u JOIN orders o ON u.id = o.user_id ")
            .contains(&"WHERE".into())
    );
}

#[test]
fn uppercase_intent_filters_identifiers_but_not_explicit_qualifiers_or_quotes() {
    assert_eq!(complete("S"), vec!["SELECT", "SHOW"]);
    let lower = complete("SELECT s");
    assert!(lower.contains(&"status".into()));
    let upper = complete("SELECT S");
    assert!(upper.contains(&"SUM(".into()));
    assert!(!upper.contains(&"status".into()));
    assert_eq!(complete("SELECT * FROM U"), Vec::<String>::new());
    assert_eq!(
        complete("SELECT * FROM `U"),
        vec!["`users`", "`UserArchive`"]
    );
    assert_eq!(
        complete("SELECT * FROM `"),
        vec!["`users`", "`orders`", "`UserArchive`"]
    );
    assert_eq!(complete("SELECT * FROM users U WHERE U.St"), vec!["status"]);
    assert_eq!(
        complete("SELECT * FROM users u JOIN orders o ON `em"),
        vec!["`u`.`email`"]
    );
    assert_eq!(
        complete("SELECT * FROM users u JOIN orders o ON u.`em"),
        vec!["`email`"]
    );
}

#[test]
fn semicolon_finishes_statement_without_misreading_literals_or_comments() {
    for source in [
        "SELECT * FROM users;",
        "SELECT 1;\n  ",
        "SELECT 1; -- finished",
        "SELECT 1; /* done */ ",
    ] {
        assert!(complete(source).is_empty(), "{source}");
    }
    assert!(complete("SELECT 1;\nS").contains(&"SELECT".into()));
    assert!(complete("SELECT ';' ").contains(&"FROM".into()));
    assert!(complete("SELECT '-- FROM orders; JOIN ghost' ").contains(&"FROM".into()));
    assert!(complete("SELECT 1 /* ; */ ").contains(&"FROM".into()));
    for source in [
        "SELECT 'unfinished;",
        "SELECT \"unfinished",
        "SELECT 1 -- FROM",
        "SELECT /* unfinished",
    ] {
        assert!(complete(source).is_empty(), "{source}");
    }
    assert!(complete("-- SELECT * FROM ghost\n").contains(&"SELECT".into()));
    assert_eq!(
        complete("SELECT /* FROM ghost */ * FROM "),
        vec!["users", "orders", "UserArchive"]
    );
}

#[test]
fn references_stay_in_current_statement_and_query_scope() {
    assert_eq!(
        complete("SELECT * FROM orders; SELECT * FROM users u WHERE u.em"),
        vec!["email"]
    );
    let source = "SELECT u.em\nFROM `users` AS u";
    assert_eq!(at(source, 0, 11, "query", &schema()), vec!["email"]);
    let nested = "SELECT (SELECT u.email FROM users u), (SELECT o.us\nFROM orders o)";
    assert_eq!(
        at(
            nested,
            0,
            nested.split('\n').next().unwrap().encode_utf16().count(),
            "query",
            &schema()
        ),
        vec!["user_id"]
    );
    assert_eq!(
        complete("SELECT * FROM app.users u WHERE u.em"),
        vec!["email"]
    );
    assert_eq!(
        complete("SELECT * FROM app."),
        vec!["users", "orders", "UserArchive"]
    );
    let union = complete("SELECT * FROM orders o UNION ALL SELECT * FROM users u WHERE ");
    assert!(union.contains(&"email".into()));
    assert!(!union.contains(&"total".into()) && !union.contains(&"o.total".into()));
}

#[test]
fn grouping_ordering_conditions_and_functions_follow_their_positions() {
    assert_eq!(complete("SELECT * FROM users GROUP "), vec!["BY"]);
    assert_eq!(complete("SELECT * FROM users ORDER "), vec!["BY"]);
    let group = complete("SELECT * FROM users GROUP BY ");
    assert!(group.contains(&"email".into()) && !group.contains(&"orders".into()));
    let order = complete("SELECT * FROM users ORDER BY email ");
    assert!(order.contains(&"ASC".into()) && order.contains(&"DESC".into()));
    assert_eq!(
        complete("SELECT * FROM users LIMIT 10 "),
        vec![",", "OFFSET"]
    );
    assert!(complete("SELECT COUNT(").contains(&"email".into()));
    assert!(complete("SELECT LEFT(").contains(&"email".into()));
    assert_eq!(
        complete("SELECT COUNT(*) "),
        vec![",", "FROM", "AS", "+", "-", "*", "/"]
    );
    assert!(complete("SELECT * FROM users WHERE id = ").contains(&"email".into()));
    assert!(!complete("SELECT DISTINCT ").contains(&"DISTINCT".into()));
    assert_eq!(complete("SELECT email AS mail "), vec![",", "FROM"]);
}

#[test]
fn dml_table_positions_do_not_regress() {
    assert_eq!(complete("UPDATE "), vec!["users", "orders", "UserArchive"]);
    let update = complete("UPDATE users ");
    assert!(update.contains(&"SET".into()) && !update.contains(&"VALUES".into()));
    assert_eq!(complete("UPDATE users SET "), vec!["id", "email", "status"]);
    assert_eq!(complete("INSERT "), vec!["INTO"]);
    assert_eq!(
        complete("INSERT INTO "),
        vec!["users", "orders", "UserArchive"]
    );
    assert_eq!(complete("DELETE "), vec!["FROM"]);
    assert_eq!(
        complete("DELETE FROM "),
        vec!["users", "orders", "UserArchive"]
    );
}

#[test]
fn completion_offsets_use_utf16_with_unicode_identifiers() {
    let source = "SELECT * FROM `пользователи` п WHERE п.им";
    let schema = json!({"tables":[{"name":"пользователи","columns":["имя"]}]});
    let result = completion::suggest(
        "mysql",
        "query",
        source,
        0,
        source.encode_utf16().count(),
        &[],
        &schema,
    );
    assert_eq!(result.candidates[0].text, "имя");
    assert_eq!(
        result.from,
        "SELECT * FROM `пользователи` п WHERE п."
            .encode_utf16()
            .count()
    );
}

#[test]
fn results_render_as_mysql_tables_and_vertical_rows() {
    let r = json!({"columns":["id","name"],"rows":[{"id":1,"name":null}]});
    assert_eq!(
        table(&r, false),
        "+----+------+\n| id | name |\n+----+------+\n| 1  | NULL |\n+----+------+"
    );
    assert!(table(&r, true).contains("  id: 1"));
    assert_eq!(table(&json!({}), false), "Empty set");
}

#[test]
fn history_matches_the_check_table_and_vertical_mode() {
    let result = json!({
        "columns":["entity_id","category_id","product_id","position"],
        "rows":[{"entity_id":1,"category_id":3,"product_id":1,"position":0}],
        "durationMs":3
    });
    let expected = "+-----------+-------------+------------+----------+\n| entity_id | category_id | product_id | position |\n+-----------+-------------+------------+----------+\n| 1         | 3           | 1          | 0        |\n+-----------+-------------+------------+----------+";
    for suffix in [";", "\\G", "\\g;"] {
        let request = format!(
            "#check 1\n#endpoint\nmysql://[redacted]@localhost:3306/magento\n#query\nSELECT * FROM m2_catalog_category_product{suffix}"
        );
        let snapshot = format!("{request}\n\n#responce OK\n{result}");
        let display = history_snapshot("mysql", &snapshot, "6:02:01 PM", true).unwrap();
        let body = if suffix == ";" {
            expected.into()
        } else {
            check_response("mysql", &result, true, "", "")
        };
        assert_eq!(
            display,
            format!("{request}\n#responce 6:02:01 PM · OK\n{body}")
        );
        assert!(snapshot.ends_with(&result.to_string()));
        assert!(!display.contains("durationMs"));
    }
    let request = "#check empty\n#endpoint\nmysql://localhost/database\n#query\nSELECT 1;";
    let snapshot = format!("{request}\n\n#responce OK\n{{\"columns\":[],\"rows\":[]}}");
    assert!(
        history_snapshot("mysql", &snapshot, "6:02:01 PM", true)
            .unwrap()
            .ends_with("Empty set")
    );
    assert!(
        history_snapshot("mysql", &snapshot, "6:02:01 PM", false)
            .unwrap()
            .ends_with(" · OK\n")
    );
    assert!(
        history_snapshot(
            "mysql",
            &format!("{request}\n\n#responce OK\ninvalid"),
            "6:02:01 PM",
            true
        )
        .is_err()
    );
    let request = "#check splunk\n#endpoint\nhttps://splunk.fixture\n#search\nsearch *";
    let snapshot = format!("{request}\n\n#responce OK\n{result}");
    assert!(
        history_snapshot("splunk", &snapshot, "6:02:01 PM", true)
            .unwrap()
            .ends_with(expected)
    );
}

#[test]
fn options_use_literal_column_names_and_reject_unknown_types() {
    let mysql = parse(
        "mysql",
        "#endpoint\nmysql://fixture/db\n#options\nColumns-Type: feed_data=json enabled=boolean\n#query\nselect * from fixture\\G",
    )
    .unwrap();
    assert_eq!(mysql["columnsType"], "feed_data=json enabled=boolean");
    assert_eq!(mysql["vertical"], true);
    assert_eq!(mysql["sql"], "select * from fixture");
    assert!(
        parse(
            "mysql",
            "#endpoint\nmysql://fixture/db\n#options\nColumns-Type: x=xml\n#query\nselect 1"
        )
        .is_err()
    );
}
