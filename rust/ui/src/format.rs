use serde_json::Value;
pub use services_contracts::response_outcome as outcome;
pub const MAX_RESPONSE_CHARS: usize = 300_000;
pub fn display(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}
pub fn table(result: &Value, vertical: bool) -> String {
    crate::tables::render(result, vertical)
}
pub fn console_response(service: &str, result: &Value, vertical: bool) -> String {
    if result["cancelled"] == true {
        return result["warning"]
            .as_str()
            .unwrap_or("Request stopped before dispatch.")
            .into();
    }
    if service == "mysql" || service == "splunk" {
        if service == "splunk" && result["preview"] == true {
            return format!(
                "{}\n\n{}",
                services_contracts::SPLUNK_PREVIEW_WARNING,
                table(result, vertical)
            );
        }
        return table(result, vertical);
    }
    let raw = result["body"].as_str().unwrap_or("").trim_end_matches('\n');
    let pretty = if raw.chars().count() <= MAX_RESPONSE_CHARS {
        pretty_body(service, raw)
    } else {
        raw.into()
    };
    let count = pretty.chars().count();
    if count > MAX_RESPONSE_CHARS {
        format!(
            "{}\n\n… response truncated ({} more characters omitted) — too large to display inline without freezing the editor.",
            pretty.chars().take(MAX_RESPONSE_CHARS).collect::<String>(),
            count - MAX_RESPONSE_CHARS
        )
    } else {
        pretty
    }
}
fn pretty_body(service: &str, raw: &str) -> String {
    if ["rest", "graphql", "grpc"].contains(&service) {
        serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_else(|| raw.into())
    } else {
        raw.into()
    }
}
pub fn check_response(
    service: &str,
    result: &Value,
    vertical: bool,
    stdout: &str,
    stderr: &str,
) -> String {
    if service == "terminal" {
        let mut body = format!("{stdout}{stderr}{}", result["error"].as_str().unwrap_or(""));
        if let Some(warning) = result["warning"].as_str() {
            if !body.is_empty() && !body.ends_with('\n') {
                body.push('\n');
            }
            body.push_str("Warning: ");
            body.push_str(warning);
        }
        body
    } else if service != "rest" && result.get("error").is_some() {
        result["error"]
            .as_str()
            .or(result["message"].as_str())
            .unwrap_or("Unknown request error")
            .into()
    } else {
        console_response(service, result, vertical)
    }
}
fn terminal_history_body(raw: &str, complete: bool) -> Result<String, String> {
    let mut stdout = String::new();
    let mut stderr = String::new();
    let result_text = if let Some(output) = raw.strip_prefix("#terminal-output\n") {
        let (events, result) = match output.split_once("\n\n") {
            Some((events, result)) => (events, Some(result)),
            None if !complete => (output, None),
            None => return Err("Saved Terminal output has no completion result".into()),
        };
        for line in events.split_inclusive('\n') {
            if !line.ends_with('\n') && result.is_none() && !complete {
                break;
            }
            let event: Value = serde_json::from_str(line)
                .map_err(|error| format!("Invalid saved Terminal output: {error}"))?;
            let data = event["data"]
                .as_str()
                .ok_or("Saved Terminal output has no text")?;
            match event["type"].as_str() {
                Some("stdout") => stdout.push_str(data),
                Some("stderr") => stderr.push_str(data),
                _ => return Err("Saved Terminal output has an unknown stream".into()),
            }
        }
        result
    } else {
        Some(raw)
    };
    let result = if complete {
        serde_json::from_str(result_text.ok_or("Saved Terminal result is missing")?)
            .map_err(|error| format!("Invalid saved Terminal result: {error}"))?
    } else {
        Value::Null
    };
    Ok(check_response("terminal", &result, false, &stdout, &stderr))
}
pub fn history_snapshot(
    service: &str,
    snapshot: &str,
    time: &str,
    complete: bool,
) -> Result<String, String> {
    history_document(service, snapshot, time, complete).map(|(text, _)| text)
}
pub fn history_document(
    service: &str,
    snapshot: &str,
    time: &str,
    complete: bool,
) -> Result<(String, Vec<crate::document::ResponseRegion>), String> {
    let headers = [
        ("\n\n#response success\n", "OK"),
        ("\n\n#response error\n", "ERROR"),
        ("\n\n#response stopped\n", "Stopped"),
        ("\n\n#responce OK\n", "OK"),
        ("\n\n#responce WARNING\n", "WARNING"),
        ("\n\n#responce ERROR\n", "ERROR"),
        ("\n\n#responce NOTICE\n", "NOTICE"),
        ("\n\n#responce Stopped\n", "Stopped"),
    ];
    let Some((offset, marker, outcome)) = headers
        .into_iter()
        .filter_map(|(marker, outcome)| {
            snapshot
                .find(marker)
                .map(|offset| (offset, marker, outcome))
        })
        .min_by_key(|(offset, _, _)| *offset)
    else {
        return Ok((snapshot.into(), vec![]));
    };
    let request = snapshot[..offset].trim_end_matches('\n');
    let raw = &snapshot[offset + marker.len()..];
    let mut table = None;
    let body = if service == "terminal" {
        terminal_history_body(raw, complete)?
    } else if ["mysql", "splunk"].contains(&service) {
        if complete {
            let result: Value = serde_json::from_str(raw)
                .map_err(|error| format!("Invalid saved {service} result: {error}"))?;
            if result["columns"].is_array() {
                table = Some(crate::tables::Metadata::from_result(&result));
            }
            let vertical = if service == "mysql" {
                let sections = crate::console::sections(service, request);
                let query = sections
                    .get("query")
                    .ok_or("Saved MySQL request has no query")?;
                crate::console::parse_sql(query, "")
                    .map_err(|error| format!("Invalid saved MySQL query: {error}"))?["vertical"]
                    == true
            } else {
                false
            };
            check_response(service, &result, vertical, "", "")
        } else {
            String::new()
        }
    } else if complete && raw.chars().count() <= MAX_RESPONSE_CHARS {
        pretty_body(service, raw.trim_end_matches('\n'))
    } else {
        raw.into()
    };
    let text = format!("{request}\n#responce {time} · {outcome}\n{body}");
    let regions = table
        .map(|table| crate::document::ResponseRegion {
            request: request
                .lines()
                .position(crate::console::is_check)
                .unwrap_or(0),
            header: request.lines().count(),
            end: text.split('\n').count(),
            table: Some(table),
        })
        .into_iter()
        .collect();
    Ok((text, regions))
}
pub fn duration(ms: f64) -> String {
    if ms < 1000.0 {
        format!("{ms:.0}ms")
    } else {
        let (value, unit) = if ms < 60000.0 {
            (ms / 1000.0, "s")
        } else {
            (ms / 60000.0, "min")
        };
        format!(
            "{}{unit}",
            format!("{value:.1}")
                .trim_end_matches('0')
                .trim_end_matches('.')
        )
    }
}
pub fn status(service: &str, result: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(ms) = result["durationMs"].as_f64() {
        parts.push(duration(ms));
    }
    parts.push(outcome(service, result).into());
    if ["mysql", "splunk"].contains(&service) {
        let n = result["resultCount"]
            .as_u64()
            .or_else(|| result["rows"].as_array().map(|a| a.len() as u64));
        if let Some(n) = n {
            parts.push(format!("{n} {}", if n == 1 { "row" } else { "rows" }));
        }
    }
    parts.join(" · ")
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn mysql_formatting() {
        let r = json!({"columns":["id","name"],"rows":[{"id":1,"name":null}]});
        assert_eq!(
            table(&r, false),
            "+----+------+\n| id | name |\n+----+------+\n| 1  | NULL |\n+----+------+"
        );
        assert!(table(&r, true).contains("  id: 1"));
        assert_eq!(table(&json!({}), false), "Empty set");
    }
    #[test]
    fn cap_does_not_split_unicode() {
        let r = json!({"body":"é".repeat(MAX_RESPONSE_CHARS+4)});
        let out = console_response("rest", &r, false);
        assert!(out.contains("4 more characters"));
        assert_eq!(
            out.chars().take(MAX_RESPONSE_CHARS).collect::<String>(),
            "é".repeat(MAX_RESPONSE_CHARS)
        );
    }
    #[test]
    fn status_codes() {
        assert_eq!(outcome("grpc", &json!({"status":0})), "OK");
        assert_eq!(outcome("rest", &json!({"status":404})), "WARNING");
        assert_eq!(duration(1500.0), "1.5s");
        assert_eq!(duration(60000.0), "1min");
    }
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
    fn splunk_preview_is_labeled_in_console_and_saved_history() {
        let result = json!({"preview":true,"columns":["n"],"rows":[{"n":31}]});
        let body = check_response("splunk", &result, false, "", "");
        assert!(body.starts_with(services_contracts::SPLUNK_PREVIEW_WARNING));
        assert!(body.contains("| 31 |"));
        let source = format!(
            "#check preview\n#endpoint\nhttps://fixture.invalid\n#options\nPreview: true\nOffset: 30\n#search\nindex=fixture\n\n#responce WARNING\n{result}"
        );
        let saved = history_snapshot("splunk", &source, "1:00:00 PM", true).unwrap();
        assert!(saved.ends_with(&body));
        assert!(saved.contains("· WARNING"));
        let mut final_result = result;
        final_result["preview"] = json!(false);
        assert!(!check_response("splunk", &final_result, false, "", "").contains("provisional"));
    }
    #[test]
    fn book_history_matches_the_visible_check_without_changing_stored_body() {
        let body = r#"{
  "id": 2,
  "title": "Example Book Title",
  "description": "Sample description of the book.",
  "pageCount": 320,
  "excerpt": "Sample excerpt from the book.",
  "publishDate": "2024-01-15T00:00:00Z"
}"#;
        let request = format!(
            "#check test1\n#endpoint\nPUT https://fakerestapi.azurewebsites.net/api/v1/Books/2\n#headers\nContent-Type: application/json\nAccept: application/json\n#body\n{body}"
        );
        let raw = serde_json::from_str::<Value>(body).unwrap().to_string();
        for header in ["#response success", "#responce OK"] {
            let snapshot = format!("{request}\n\n\n{header}\n{raw}");
            assert_eq!(
                history_snapshot("rest", &snapshot, "5:47:15 PM", true).unwrap(),
                format!("{request}\n#responce 5:47:15 PM · OK\n{body}")
            );
            assert_eq!(console_response("rest", &json!({"body":raw}), false), body);
            assert!(snapshot.ends_with(&raw));
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
    #[test]
    fn mysql_history_matches_the_check_table_and_vertical_mode() {
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
    fn terminal_history_matches_joined_console_output_without_event_or_exit_metadata() {
        let request = "#check indexer status\n#endpoint\ndocker exec fixture bash -c\n#command\nbin/magento indexer:status";
        let first = "+----+-------+\n| ID | Title |\n";
        let last = "+----+-------+\n| 1  | Ready |\n+----+-------+\n";
        let warning = "warning: é\n";
        let events = [
            json!({"type":"stdout","data":first}),
            json!({"type":"stderr","data":warning}),
            json!({"type":"stdout","data":last}),
        ]
        .map(|event| event.to_string())
        .join("\n");
        let result = json!({"code":0,"signal":null,"durationMs":3645,"cancelled":false});
        let incomplete = json!({"code":0,"outputIncomplete":true,"warning":"Detached processes may still be running."});
        let incomplete_snapshot =
            format!("{request}\n\n#responce WARNING\n#terminal-output\n{events}\n\n{incomplete}");
        assert_eq!(
            history_snapshot("terminal", &incomplete_snapshot, "6:00:14 PM", true).unwrap(),
            format!(
                "{request}\n#responce 6:00:14 PM · WARNING\n{first}{last}{warning}Warning: Detached processes may still be running."
            )
        );
        assert_eq!(
            check_response("terminal", &incomplete, false, "output", ""),
            "output\nWarning: Detached processes may still be running."
        );
        let snapshot = format!("{request}\n\n#responce OK\n#terminal-output\n{events}\n\n{result}");
        let display = history_snapshot("terminal", &snapshot, "6:00:14 PM", true).unwrap();
        let stdout = format!("{first}{last}");
        assert_eq!(
            display,
            format!("{request}\n#responce 6:00:14 PM · OK\n{stdout}{warning}")
        );
        assert!(display.ends_with(&check_response(
            "terminal", &result, false, &stdout, warning
        )));
        assert!(!display.contains("#terminal-output"));
        assert!(!display.contains("\"type\""));
        assert!(!display.contains("durationMs"));
        assert!(snapshot.contains("#terminal-output"));
        let partial = format!(
            "{request}\n\n#responce OK\n#terminal-output\n{}\n{{\"type\":\"stdout\",\"data\":",
            json!({"type":"stdout","data":first})
        );
        assert!(
            history_snapshot("terminal", &partial, "6:00:14 PM", false)
                .unwrap()
                .ends_with(first)
        );
        assert!(history_snapshot("terminal", &partial, "6:00:14 PM", true).is_err());
        let empty = format!("{request}\n\n#responce OK\n{result}");
        assert!(
            history_snapshot("terminal", &empty, "6:00:14 PM", true)
                .unwrap()
                .ends_with(" · OK\n")
        );
        let failed = format!(
            "{request}\n\n#responce ERROR\n{{\"error\":\"could not start\",\"debug\":{{}}}}"
        );
        assert!(
            history_snapshot("terminal", &failed, "6:00:14 PM", true)
                .unwrap()
                .ends_with("could not start")
        );
    }
}
