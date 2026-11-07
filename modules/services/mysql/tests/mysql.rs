//! MySQL module behaviour: requests, `\G`, column options, result tables, SQL completion
//! against a discovered schema, discovery through the `mysql` driver and highlighting.

use serde_json::{Value, json};
use services_module_mysql::{DISCOVERY_SQL, Mysql, parse_sql, render};
use services_sdk::lang::marker;
use services_sdk::{Check, Cmd, DriverCall, Event, Failure, Module, Section};

const RUN: &str = "console:mysql:1";

/// Runs `check` with its variables left as they are: the driver call the run makes, or the
/// error it finished with.
fn run(mysql: &mut Mysql, check: &Check) -> Result<DriverCall, String> {
    let cmds = mysql.update(Event::Run {
        tag: RUN.into(),
        check: check.clone(),
    });
    let failed = |cmd: &Cmd| -> Result<DriverCall, String> {
        assert_eq!(cmd.name, "run.result");
        Err(cmd.args()["error"].as_str().unwrap().to_owned())
    };
    if cmds[0].name != "vars.resolve" {
        return failed(&cmds[0]);
    }
    let text = cmds[0].args()["text"].clone();
    let cmds = mysql.update(Event::Reply {
        tag: cmds[0].tag.clone(),
        result: Ok(text),
    });
    if cmds[0].name != "driver.call" {
        return failed(&cmds[0]);
    }
    let args = cmds[0].args();
    Ok(DriverCall::new(
        args["driver"].as_str().unwrap(),
        &args["request"],
    ))
}

/// A check as the Kernel builds it: sections split at marker lines.
fn check(text: &str) -> Check {
    let mut sections: Vec<Section> = vec![];
    let mut body: Vec<&str> = vec![];
    let mut current: Option<(String, String, u32)> = Some(("leftover".into(), String::new(), 0));
    let mut close = |current: Option<(String, String, u32)>, body: &mut Vec<&str>| {
        if let Some((name, head, line)) = current {
            let mut text = head;
            if !body.is_empty() {
                text = format!("{text}\n{}", body.join("\n"));
            }
            body.clear();
            let text = text.trim().to_owned();
            if name != "leftover" || !text.is_empty() {
                sections.push(Section { name, text, line });
            }
        }
    };
    for (i, line) in text.split('\n').enumerate() {
        if let Some((name, rest)) = marker(line) {
            close(current.take(), &mut body);
            let head = if name == "endpoint" {
                rest.to_owned()
            } else {
                String::new()
            };
            current = Some((name, head, i as u32));
        } else {
            body.push(line);
        }
    }
    close(current, &mut body);
    Check {
        columns: Vec::new(),
        service: "mysql".into(),
        text: text.into(),
        sections,
        options: vec![],
    }
}

fn schema() -> Value {
    json!({"tables":[
        {"name":"users","columns":["id","email","status"]},
        {"name":"orders","columns":["user_id","total"]},
        {"name":"UserArchive","columns":["Email"]}
    ]})
}

const HEAD: &str = "#endpoint\nmysql://db\n#query\n";

/// Completion labels at the end of `query` with `schema`.
fn suggest(query: &str, schema: &Value) -> Vec<String> {
    let text = format!("{HEAD}{query}");
    services_module_mysql::complete(&check(&text), text.len(), schema)
        .items
        .into_iter()
        .map(|item| item.label)
        .collect()
}

fn complete(query: &str) -> Vec<String> {
    suggest(query, &schema())
}

/// Completion at the `|` in a query: the replaced range start within the query and labels.
fn complete_at(query: &str, schema: &Value) -> (usize, Vec<String>) {
    let text = format!("{HEAD}{query}");
    let pos = text.find('|').unwrap();
    let text = text.replacen('|', "", 1);
    let result = services_module_mysql::complete(&check(&text), pos, schema);
    (
        result.from as usize - HEAD.len(),
        result.items.into_iter().map(|item| item.label).collect(),
    )
}

fn build(text: &str) -> Result<Value, String> {
    run(&mut Mysql::default(), &check(text)).map(|call| call.request())
}

fn column(name: &str, kind: &str) -> Value {
    json!({"name": name, "type": kind, "length": 11, "charset": 33})
}

/// The text and outcome of an answer rendered for `request`.
fn rendered(request: &Value, response: &Value) -> (String, String) {
    let rendered = render(Some(request), response).unwrap();
    assert_eq!(rendered.language, "mysql");
    (rendered.text, rendered.outcome)
}

#[test]
fn backslash_g_requests_vertical_results() {
    for sql in ["SELECT 1 \\G", "SELECT 1; \\G", "SELECT 1 \\g;"] {
        let query = parse_sql(sql).unwrap();
        assert_eq!(query.sql, "SELECT 1");
        assert!(query.vertical);
    }
    assert!(!parse_sql("SELECT 1;").unwrap().vertical);
    assert_eq!(parse_sql("  ").unwrap_err(), "SQL query is required");
}

#[test]
fn checks_become_mysql_driver_calls() {
    let call = run(
        &mut Mysql::default(),
        &check("#endpoint\nmysql://{{store:USER}}@db/shop\n#query\nSELECT * FROM t\\G"),
    )
    .unwrap();
    assert_eq!(call.driver, "mysql");
    assert!(!call.stream);
    let request: Value = serde_json::from_slice(&call.request).unwrap();
    assert_eq!(
        request,
        json!({"url":"mysql://{{store:USER}}@db/shop","sql":"SELECT * FROM t","console":{"vertical":true}})
    );
    assert_eq!(
        build("#endpoint\nhttp://db\n#query\nSELECT 1").unwrap_err(),
        "MySQL request needs a #endpoint block (a mysql:// connection string)"
    );
    assert_eq!(
        build("#endpoint\nmysql://db\n#query\n").unwrap_err(),
        "SQL query is required"
    );
}

#[test]
fn options_use_literal_column_names_and_reject_unknown_types() {
    let request = build(
        "#endpoint\nmysql://fixture/db\n#options\nColumns-Type: feed_data=json enabled=boolean\n#query\nselect * from fixture\\G",
    )
    .unwrap();
    assert_eq!(
        request["console"],
        json!({"vertical":true,"columnsType":"feed_data=json enabled=boolean"})
    );
    assert_eq!(request["sql"], "select * from fixture");
    assert!(
        build("#endpoint\nmysql://fixture/db\n#options\nColumns-Type: x=xml\n#query\nselect 1")
            .is_err()
    );
    let response = json!({"sets":[{"columns":[column("feed_data","blob"),column("enabled","tiny")],"rows":[["{\"a\":1}",1]]}]});
    let (text, _) = rendered(
        &json!({"console":{"vertical":true,"columnsType":"feed_data=json"}}),
        &response,
    );
    assert!(text.contains("feed_data: {"), "{text}");
    assert!(text.contains("\"a\": 1"), "{text}");
    let missing = render(
        Some(&json!({"console":{"columnsType":"missing=json"}})),
        &response,
    );
    assert!(missing.unwrap_err().contains("missing"));
}

#[test]
fn render_reports_the_server_column_types() {
    let response = json!({"sets":[{"columns":[column("id","long"),column("doc","json"),column("at","datetime"),column("name","var_string")],"rows":[[1,"{}","2024-01-01 00:00:00","x"]]}]});
    let rendered = render(Some(&json!({"console":{}})), &response).unwrap();
    let kinds: Vec<(String, String)> = rendered
        .columns
        .into_iter()
        .map(|column| (column.name, column.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("id", "number"),
            ("doc", "json"),
            ("at", "datetime"),
            ("name", "text")
        ]
        .map(|(name, kind)| (name.to_owned(), kind.to_owned()))
    );
}

#[test]
fn results_render_as_mysql_tables_and_vertical_rows() {
    let response = json!({"sets":[{"columns":[column("id","long"),column("name","var_string")],"rows":[[1,null]]}],"threadId":7});
    let (text, outcome) = rendered(&json!({"console":{"vertical":false}}), &response);
    assert_eq!(
        text,
        "+----+------+\n| id | name |\n+----+------+\n| 1  | NULL |\n+----+------+"
    );
    assert_eq!(outcome, "OK");
    let (text, _) = rendered(&json!({"console":{"vertical":true}}), &response);
    assert!(text.contains("id: 1"), "{text}");
    let (text, _) = rendered(&json!({}), &json!({"sets":[]}));
    assert_eq!(text, "Empty set");
}

#[test]
fn statements_without_rows_show_affected_rows_and_every_set_prints() {
    let response = json!({"sets":[
        {"affectedRows":3,"info":"Rows matched: 3"},
        {"columns":[column("n","longlong")],"rows":[[1]]}
    ]});
    let (text, _) = rendered(&json!({}), &response);
    assert_eq!(
        text,
        "+--------------+\n| affectedRows |\n+--------------+\n| 3            |\n+--------------+\n\n+---+\n| n |\n+---+\n| 1 |\n+---+"
    );
}

#[test]
fn json_binary_and_server_text_cells_print() {
    let response = json!({"sets":[{"columns":[
        column("doc","json"),
        {"name":"raw","type":"blob","charset":63},
        column("at","datetime"),
        column("price","newdecimal")
    ],"rows":[["{\"b\": [1, 2]}",{"base64":"AQI="},"2024-01-02 03:04:05","12.50"]]}]});
    let (text, _) = rendered(&json!({}), &response);
    assert!(text.contains("2024-01-02 03:04:05"), "{text}");
    assert!(text.contains("12.50"), "{text}");
    assert!(text.contains("\"Buffer\""), "{text}");
    assert!(text.contains("[1,2]"), "{text}");
}

#[test]
fn server_errors_follow_earlier_sets_and_fail_the_check() {
    let response = json!({"sets":[{"columns":[column("n","long")],"rows":[[1]]}],
        "error":{"code":1064,"state":"42000","message":"You have an error"}});
    let (text, outcome) = rendered(&json!({}), &response);
    assert!(
        text.ends_with("+---+\n\nERROR 1064 (42000): You have an error"),
        "{text}"
    );
    assert_eq!(outcome, "ERROR");
    let (text, _) = rendered(
        &json!({}),
        &json!({"error":{"code":1045,"message":"Access denied"}}),
    );
    assert_eq!(text, "ERROR 1045: Access denied");
}

#[test]
fn runs_render_their_call_with_the_row_count_and_format() {
    let mut mysql = Mysql::default();
    let finish = |mysql: &mut Mysql, response: Value| {
        mysql.update(Event::Reply {
            tag: format!("{RUN}:call"),
            result: Ok(response),
        })
    };
    run(
        &mut mysql,
        &check("#endpoint\nmysql://db\n#query\nSELECT 1\\G"),
    )
    .unwrap();
    let response = json!({"sets":[{"columns":[column("1","longlong")],"rows":[[1]]}]});
    let cmds = finish(&mut mysql, response.clone());
    assert_eq!(
        (cmds[0].name.as_str(), cmds[0].args()["text"].clone()),
        ("status.set", json!("1 row"))
    );
    let text = cmds[1].args()["body"].as_str().unwrap().to_owned();
    assert!(
        text.starts_with("*************************** 1. row"),
        "{text}"
    );
    assert!(text.ends_with("1: 1"), "{text}");
    run(
        &mut mysql,
        &check("#endpoint\nmysql://db\n#options\nFormat: json\n#query\nSELECT 1"),
    )
    .unwrap();
    let cmds = finish(&mut mysql, response);
    assert_eq!(cmds[1].args()["body"], "[\n  {\n    \"1\": 1\n  }\n]");
    assert_eq!(
        build("#endpoint\nmysql://db\n#options\nFormat: grid\n#query\nSELECT 1").unwrap_err(),
        "Format takes table, vertical or json, not \"grid\""
    );
}

#[test]
fn alias_columns_resolve_to_their_table() {
    let schema = json!({"tables":[{"name":"users","columns":[{"name":"id"},{"name":"email"}]},{"name":"orders","columns":[{"name":"total"}]}]});
    assert_eq!(
        suggest("SELECT * FROM users u WHERE u.em", &schema)[0],
        "email"
    );
    let (_, items) = complete_at("SELECT u.em|\nFROM users u", &schema);
    assert_eq!(items, vec!["email"]);
}

#[test]
fn query_start_does_not_drown_commands_in_discovered_columns() {
    let schema = json!({"tables":[{"name":"huge","columns":(0..150).map(|i|format!("field_{i}")).collect::<Vec<_>>()}]});
    let items = suggest("", &schema);
    assert!(items.contains(&"SELECT".into()));
    assert!(items.len() < 100);
    assert!(
        items
            .iter()
            .all(|item| !item.starts_with("field_") && item != "huge")
    );
}

#[test]
fn joined_table_columns_are_not_buried_by_a_large_first_table() {
    let schema = json!({"tables":[
        {"name":"users","columns":(0..150).map(|i|format!("field_{i}")).collect::<Vec<_>>()},
        {"name":"orders","columns":["user_id","total"]}
    ]});
    let items = suggest("SELECT * FROM users u JOIN orders o ON ", &schema);
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
    assert!(complete("SELECT * FROM users u LEFT JOIN orders o ").contains(&"ON".into()));
    let on = complete("SELECT * FROM users u JOIN orders o ON ");
    assert!(on.contains(&"u.id".into()) && on.contains(&"o.user_id".into()));
    assert!(!on.contains(&"UserArchive.Email".into()));
    assert_eq!(
        complete("SELECT * FROM users u JOIN orders o ON o.us"),
        vec!["user_id"]
    );
}

#[test]
fn uppercase_intent_filters_identifiers_but_not_explicit_qualifiers_or_quotes() {
    assert_eq!(complete("S"), vec!["SELECT", "SHOW"]);
    assert!(complete("SELECT s").contains(&"status".into()));
    let upper = complete("SELECT S");
    assert!(upper.contains(&"SUM(".into()));
    assert!(!upper.contains(&"status".into()));
    assert_eq!(
        complete("SELECT * FROM `U"),
        vec!["`users`", "`UserArchive`"]
    );
    assert_eq!(complete("SELECT * FROM users U WHERE U.St"), vec!["status"]);
    assert_eq!(
        complete("SELECT * FROM users u JOIN orders o ON `em"),
        vec!["`u`.`email`"]
    );
}

#[test]
fn semicolon_finishes_statement_without_misreading_literals_or_comments() {
    for source in [
        "SELECT * FROM users;",
        "SELECT 1; -- finished",
        "SELECT 'unfinished;",
        "SELECT 1 -- FROM",
    ] {
        assert!(complete(source).is_empty(), "{source}");
    }
    assert!(complete("SELECT 1;\nS").contains(&"SELECT".into()));
    assert!(complete("SELECT ';' ").contains(&"FROM".into()));
    assert_eq!(
        complete("SELECT /* FROM ghost */ * FROM "),
        vec!["users", "orders", "UserArchive"]
    );
}

#[test]
fn dml_and_grouping_positions_do_not_regress() {
    assert_eq!(complete("UPDATE "), vec!["users", "orders", "UserArchive"]);
    assert_eq!(complete("UPDATE users SET "), vec!["id", "email", "status"]);
    assert_eq!(complete("INSERT "), vec!["INTO"]);
    assert_eq!(complete("DELETE "), vec!["FROM"]);
    assert_eq!(complete("SELECT * FROM users GROUP "), vec!["BY"]);
}

#[test]
fn completion_offsets_are_bytes_with_unicode_identifiers() {
    let schema = json!({"tables":[{"name":"пользователи","columns":["имя"]}]});
    let query = "SELECT * FROM `пользователи` п WHERE п.им|";
    let (from, items) = complete_at(query, &schema);
    assert_eq!(items, vec!["имя"]);
    assert_eq!(from, query.find("им|").unwrap());
}

#[test]
fn markers_scheme_and_options_complete_outside_the_query() {
    let inserts = |text: &str| -> Vec<String> {
        let pos = text.find('|').unwrap();
        let text = text.replacen('|', "", 1);
        Mysql::default()
            .complete(&check(&text), pos as u32)
            .items
            .into_iter()
            .map(|item| item.insert)
            .collect()
    };
    assert_eq!(inserts("#endpoint\nmy|"), vec!["mysql://"]);
    assert_eq!(
        inserts("#endpoint\nmysql://db\n#options\nCol|"),
        vec!["Columns-Type: "]
    );
    assert!(inserts("#endpoint\nmysql://db\n#query\nSELECT 1\n#responce OK\n|").is_empty());
    assert!(!inserts("#endpoint\nmysql://db\n#|").is_empty());
}

fn statuses(cmds: &[Cmd]) -> Vec<String> {
    cmds.iter()
        .filter(|cmd| cmd.name == "status.set")
        .map(|cmd| cmd.args()["text"].as_str().unwrap().to_owned())
        .collect()
}

fn driver_call(cmds: &[Cmd]) -> (String, Value) {
    let cmd = cmds
        .iter()
        .find(|cmd| cmd.name == "driver.call")
        .expect("a driver call");
    let args = cmd.args();
    assert_eq!(args["driver"], "mysql");
    (cmd.tag.clone(), args["request"].clone())
}

fn discovery_rows() -> Value {
    let columns: Vec<_> = ["tableName", "columnName", "columnType", "columnKey"]
        .iter()
        .map(|name| column(name, "var_string"))
        .collect();
    json!({"sets":[{"columns":columns,"rows":[
        ["users","id","int","PRI"],["users","email","varchar(255)",""],["orders","total","decimal(10,2)",""]
    ]}]})
}

#[test]
fn hints_discover_the_schema_once_and_feed_completion() {
    let mut mysql = Mysql::default();
    let text = "#endpoint\nmysql://root@db/shop\n#query\nSELECT * FROM ";
    let cmds = mysql.update(Event::Hints {
        check: check(text),
        pos: text.len() as u32,
    });
    assert_eq!(statuses(&cmds), vec!["Discovery…"]);
    let (tag, request) = driver_call(&cmds);
    assert_eq!(request["url"], "mysql://root@db/shop");
    assert_eq!(request["sql"], DISCOVERY_SQL);
    let done = mysql.update(Event::Reply {
        tag,
        result: Ok(discovery_rows()),
    });
    assert_eq!(statuses(&done), vec!["Discovery · OK"]);
    let items: Vec<_> = mysql
        .complete(&check(text), text.len() as u32)
        .items
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert_eq!(items, vec!["users", "orders"]);
    assert!(
        mysql
            .update(Event::Hints {
                check: check(text),
                pos: 0
            })
            .is_empty()
    );
    assert_eq!(
        statuses(&mysql.update(Event::Discover(check(text)))),
        vec!["Discovery…"]
    );
}

#[test]
fn discovery_resolves_variables_through_the_app_and_reports_failures() {
    let mut mysql = Mysql::default();
    let text = "#endpoint\nmysql://{{store:DSN}}\n#query\nSELECT 1";
    let cmds = mysql.update(Event::Document(check(text)));
    let resolve = cmds.iter().find(|cmd| cmd.name == "vars.resolve").unwrap();
    assert!(
        mysql
            .update(Event::Reply {
                tag: resolve.tag.clone(),
                result: Ok(json!("mysql://u@db/x")),
            })
            .is_empty()
    );
    let cmds = mysql.update(Event::Discover(check(text)));
    let resolve = cmds.iter().find(|cmd| cmd.name == "vars.resolve").unwrap();
    let cmds = mysql.update(Event::Reply {
        tag: resolve.tag.clone(),
        result: Ok(json!("mysql://u@db/x")),
    });
    let (tag, request) = driver_call(&cmds);
    assert_eq!(request["url"], "mysql://u@db/x");
    let failed = mysql.update(Event::Reply {
        tag,
        result: Err(Failure::new("connect ECONNREFUSED")),
    });
    assert_eq!(
        statuses(&failed),
        vec!["Discovery · ERROR · connect ECONNREFUSED"]
    );
    let invalid = mysql.update(Event::Discover(check("#endpoint\nmysql://\n#query\nx")));
    assert_eq!(
        statuses(&invalid),
        vec!["Discovery · WARNING · Invalid or incomplete service address for discovery"]
    );
    assert!(
        mysql
            .update(Event::Reply {
                tag: "discover:99".into(),
                result: Ok(json!({}))
            })
            .is_empty()
    );
}

fn styles(lines: &[&str]) -> Vec<Vec<(String, String)>> {
    let lines: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
    Mysql::default()
        .highlight(&[], &lines, &[])
        .iter()
        .zip(&lines)
        .map(|(line, text)| {
            line.tokens
                .iter()
                .map(|t| {
                    let start = t.start as usize;
                    (
                        text[start..start + t.len as usize].to_owned(),
                        t.kind.clone(),
                    )
                })
                .collect()
        })
        .collect()
}

#[test]
fn checks_highlight_sql_and_result_tables() {
    let styled = styles(&[
        "#query",
        "SELECT /* a",
        "b */ 'x' # c",
        "#responce 6:02:01 PM · OK",
        "+----+",
        "| id |",
        "+----+",
        "| 1  |",
        "+----+",
    ]);
    let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
    assert_eq!(styled[0], vec![pair("#query", "comment")]);
    assert_eq!(
        styled[1],
        vec![pair("SELECT", "keyword"), pair("/* a", "comment")]
    );
    assert!(styled[2].contains(&pair("b */", "comment")));
    assert!(styled[2].contains(&pair("'x'", "string")));
    assert!(
        styled[4]
            .iter()
            .all(|(_, kind)| kind == "mysql-table-border")
    );
    assert!(styled[7].contains(&pair("1", "number")));
}
