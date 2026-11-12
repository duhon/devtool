//! A driver answer as the console shows it: the documents of a cursor as a table, anything
//! else as the reply document, a server error after what was read before it.

use crate::action::View;
use crate::value::{as_f64, iso_date};
use serde_json::{Map, Value, json};
use services_sdk::lang::table::{self, Column, Format, Kind, Options};
use services_sdk::mongodb::{Request, Response};
use services_sdk::{ColumnType, Rendered};

/// Documents whose fields make the columns, and the columns shown.
const SAMPLE: usize = 100;
const MAX_COLUMNS: usize = 40;

/// How a run's answer prints.
#[derive(Clone, Debug, Default)]
pub struct Display {
    pub view: Option<View>,
    pub format: Option<Format>,
    pub columns_type: Option<String>,
}

/// An Extended JSON value as plain JSON: numbers are numbers, `$date` an ISO string.
pub fn relaxed(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(relaxed).collect()),
        Value::Object(map) => {
            if let Some((key, inner)) = single(map) {
                match (key, inner) {
                    ("$numberInt" | "$numberLong", Value::String(text)) => {
                        if let Ok(n) = text.parse::<i64>() {
                            return json!(n);
                        }
                    }
                    ("$numberDouble", Value::String(text)) => {
                        if let Some(n) = text.parse::<f64>().ok().filter(|n| n.is_finite()) {
                            return json!(n);
                        }
                    }
                    ("$date", Value::Object(millis)) => {
                        if let Some(ms) = millis.get("$numberLong").and_then(Value::as_str)
                            && let Ok(ms) = ms.parse::<i64>()
                            && (0..253_402_300_800_000).contains(&ms)
                        {
                            return json!({"$date": iso_date(ms)});
                        }
                    }
                    _ => {}
                }
            }
            Value::Object(map.iter().map(|(k, v)| (k.clone(), relaxed(v))).collect())
        }
        other => other.clone(),
    }
}

fn single(map: &Map<String, Value>) -> Option<(&str, &Value)> {
    (map.len() == 1)
        .then(|| map.iter().next().map(|(k, v)| (k.as_str(), v)))
        .flatten()
}

fn uuid(base64: &str) -> Option<String> {
    let bytes = services_sdk::driver::decode(base64)
        .ok()
        .filter(|b| b.len() == 16)?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

/// A field value as a cell: its kind and value; `None` for a kind-neutral `null`.
fn cell(value: &Value) -> (Option<Kind>, Value) {
    let text = |text: String| (Some(Kind::Text), Value::String(text));
    match value {
        Value::Null => (None, Value::Null),
        Value::Bool(_) => (Some(Kind::Boolean), value.clone()),
        Value::Number(_) => (Some(Kind::Number), value.clone()),
        Value::String(_) => (Some(Kind::Text), value.clone()),
        Value::Array(_) => (Some(Kind::Json), relaxed(value)),
        Value::Object(map) => match single(map) {
            Some(("$numberInt" | "$numberLong" | "$numberDouble", _)) => match relaxed(value) {
                Value::Number(n) => (Some(Kind::Number), Value::Number(n)),
                _ => (
                    Some(Kind::Number),
                    json!(as_f64(value).map(|n| n.to_string()).unwrap_or_default()),
                ),
            },
            Some(("$numberDecimal", Value::String(n))) => (Some(Kind::Number), json!(n)),
            Some(("$oid", Value::String(id))) => text(format!("ObjectId(\"{id}\")")),
            Some(("$date", date)) => {
                let iso = match relaxed(value)["$date"].as_str() {
                    Some(iso) => iso.to_owned(),
                    None => date.to_string(),
                };
                (Some(Kind::Datetime), json!(format!("ISODate(\"{iso}\")")))
            }
            Some(("$regularExpression", re)) => text(format!(
                "/{}/{}",
                re["pattern"].as_str().unwrap_or(""),
                re["options"].as_str().unwrap_or("")
            )),
            Some(("$timestamp", t)) => text(format!("Timestamp({}, {})", t["t"], t["i"])),
            Some(("$minKey", _)) => text("MinKey()".into()),
            Some(("$maxKey", _)) => text("MaxKey()".into()),
            Some(("$undefined", _)) => (None, Value::Null),
            Some(("$binary", binary)) => {
                let data = binary["base64"].as_str().unwrap_or("");
                match (binary["subType"].as_str(), uuid(data)) {
                    (Some("04"), Some(id)) => text(format!("UUID(\"{id}\")")),
                    (kind, _) => text(format!("BinData({}, \"{data}\")", kind.unwrap_or("00"))),
                }
            }
            _ => (Some(Kind::Json), relaxed(value)),
        },
    }
}

/// The columns and rows of documents: the union of the top-level fields of the first
/// documents; `hidden` counts the fields past [`MAX_COLUMNS`].
pub fn documents_table(documents: &[Value]) -> (Vec<Column>, Vec<Vec<Value>>, usize) {
    let mut names: Vec<String> = Vec::new();
    for document in documents.iter().take(SAMPLE) {
        if let Value::Object(map) = document {
            for name in map.keys() {
                if !names.contains(name) {
                    names.push(name.clone());
                }
            }
        }
    }
    let hidden = names.len().saturating_sub(MAX_COLUMNS);
    names.truncate(MAX_COLUMNS);
    let mut kinds: Vec<Option<Option<Kind>>> = vec![None; names.len()];
    let mut rows = Vec::new();
    for document in documents {
        let mut row = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let (kind, value) = match document.get(name) {
                Some(field) => cell(field),
                None => (None, Value::Null),
            };
            if let Some(kind) = kind {
                kinds[i] = Some(match kinds[i] {
                    None => Some(kind),
                    Some(Some(known)) if known == kind => Some(kind),
                    Some(_) => None,
                });
            }
            row.push(value);
        }
        rows.push(row);
    }
    let columns = names
        .into_iter()
        .zip(kinds)
        .map(|(name, kind)| Column::new(name, kind.flatten().unwrap_or(Kind::Text)))
        .collect();
    (columns, rows, hidden)
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("JSON values serialize")
}

fn error_line(code: i32, name: &str, message: &str) -> String {
    if name.is_empty() {
        format!("ERROR {code}: {message}")
    } else {
        format!("ERROR {code} ({name}): {message}")
    }
}

/// The answer of `request` as text with its column types, and the run's status.
pub fn render(
    display: &Display,
    request: &Request,
    response: &Response,
) -> Result<(Rendered, String), String> {
    let options = Options::parse(display.columns_type.as_deref(), None)?;
    let view = display.view.unwrap_or(View::Auto);
    let mut columns_out: Vec<ColumnType> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    let mut failed = response.error.is_some();
    let documents: Option<Vec<Value>> = match view {
        View::Rows(field) => Some(
            response
                .reply
                .as_ref()
                .and_then(|reply| reply[field].as_array().cloned())
                .unwrap_or_default(),
        ),
        _ if request.is_cursor() => Some(response.documents.clone()),
        _ => None,
    };
    let mut status = String::new();
    match documents {
        Some(documents) => {
            let (mut columns, mut rows, hidden) = documents_table(&documents);
            if view == View::Count && documents.is_empty() {
                columns = vec![Column::new("n", Kind::Number)];
                rows = vec![vec![json!(0)]];
            }
            let known: Vec<_> = columns.iter().map(|column| column.name.clone()).collect();
            if let Some(name) = options.types.keys().find(|name| !known.contains(name)) {
                return Err(format!(
                    "Column options refer to an unknown result column: {name}"
                ));
            }
            options.apply(&mut columns)?;
            let mut text = match display.format {
                Some(Format::Json) => {
                    pretty(&Value::Array(documents.iter().map(relaxed).collect()))
                }
                format => table::show(&columns, &rows, format, false),
            };
            if hidden > 0 {
                text.push_str(&format!(
                    "\n{hidden} more fields not shown; use Format: json to see whole documents"
                ));
            }
            if response.truncated {
                text.push_str(&format!(
                    "\n{} documents shown; the rest were dropped (raise Max-Documents)",
                    response.count
                ));
            }
            parts.push(text);
            columns_out = table::types(&columns);
            let n = documents.len();
            status = format!("{n} {}", if n == 1 { "document" } else { "documents" });
        }
        None => {
            let reply = response.reply.as_ref().map(relaxed).unwrap_or(Value::Null);
            failed |=
                reply.get("writeErrors").is_some() || reply.get("writeConcernError").is_some();
            parts.push(pretty(&reply));
        }
    }
    if let Some(error) = &response.error {
        parts.push(error_line(error.code, &error.code_name, &error.message));
    }
    if response.timing.total_us > 0 {
        let ms = response.timing.total_us as f64 / 1000.0;
        let time = if ms < 10.0 {
            format!("{ms:.1} ms")
        } else {
            format!("{ms:.0} ms")
        };
        status = if status.is_empty() {
            time
        } else {
            format!("{status} · {time}")
        };
    }
    Ok((
        Rendered {
            text: parts.join("\n\n"),
            language: "mongodb".into(),
            outcome: if failed { "ERROR" } else { "OK" }.into(),
            columns: columns_out,
        },
        status,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docs() -> Vec<Value> {
        vec![
            json!({"_id": {"$oid": "65f0c0ffee0000000000000a"}, "name": "Ann", "age": {"$numberInt": "30"},
                "born": {"$date": {"$numberLong": "946684800000"}}, "tags": ["a", "b"], "vip": true}),
            json!({"_id": {"$oid": "65f0c0ffee0000000000000b"}, "name": "Bob", "age": {"$numberDouble": "41.5"},
                "address": {"city": "Paris"}}),
        ]
    }

    fn answer(documents: Vec<Value>) -> Response {
        Response {
            count: documents.len() as u64,
            documents,
            ..Response::default()
        }
    }

    fn find() -> Request {
        Request::new("mongodb://h/app", "app", json!({"find": "users"}))
    }

    #[test]
    fn documents_become_a_table_with_typed_columns() {
        let (shown, status) = render(&Display::default(), &find(), &answer(docs())).unwrap();
        assert_eq!(status, "2 documents");
        assert!(
            shown
                .text
                .contains("ObjectId(\"65f0c0ffee0000000000000a\")"),
            "{}",
            shown.text
        );
        assert!(shown.text.contains("ISODate(\"2000-01-01T00:00:00.000Z\")"));
        assert!(shown.text.contains("[\"a\",\"b\"]"));
        assert!(shown.text.contains("{\"city\":\"Paris\"}"));
        assert!(shown.text.contains("41.5"));
        let kinds: Vec<_> = shown
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.kind.as_str()))
            .collect();
        assert!(kinds.contains(&("age", "number")), "{kinds:?}");
        assert!(kinds.contains(&("born", "datetime")));
        assert!(kinds.contains(&("tags", "json")));
        assert_eq!(shown.outcome, "OK");
    }

    #[test]
    fn json_format_shows_whole_relaxed_documents() {
        let display = Display {
            format: Some(Format::Json),
            ..Display::default()
        };
        let (shown, _) = render(&display, &find(), &answer(docs())).unwrap();
        let parsed: Value = serde_json::from_str(&shown.text).unwrap();
        assert_eq!(parsed[0]["age"], json!(30));
        assert_eq!(
            parsed[0]["born"],
            json!({"$date": "2000-01-01T00:00:00.000Z"})
        );
        assert_eq!(parsed[1]["address"]["city"], "Paris");
    }

    #[test]
    fn an_empty_cursor_is_an_empty_set_and_a_count_is_zero() {
        let (shown, _) = render(&Display::default(), &find(), &answer(vec![])).unwrap();
        assert_eq!(shown.text, "Empty set");
        let display = Display {
            view: Some(View::Count),
            ..Display::default()
        };
        let (shown, _) = render(&display, &find(), &answer(vec![])).unwrap();
        assert!(shown.text.contains("| 0"), "{}", shown.text);
    }

    #[test]
    fn truncation_and_server_errors_are_shown_after_the_documents() {
        let mut response = answer(docs());
        response.truncated = true;
        response.error = Some(services_sdk::mongodb::ServerError {
            code: 43,
            code_name: "CursorNotFound".into(),
            message: "cursor id 1 not found".into(),
        });
        let (shown, _) = render(&Display::default(), &find(), &response).unwrap();
        assert!(
            shown
                .text
                .contains("2 documents shown; the rest were dropped")
        );
        assert!(
            shown
                .text
                .ends_with("ERROR 43 (CursorNotFound): cursor id 1 not found")
        );
        assert!(shown.text.contains("Ann"));
        assert_eq!(shown.outcome, "ERROR");
    }

    #[test]
    fn other_commands_show_the_reply_and_write_errors_fail() {
        let request = Request::new("mongodb://h/app", "app", json!({"insert": "u"}));
        let reply = json!({"n": {"$numberInt": "1"}, "ok": {"$numberDouble": "1.0"}});
        let response = Response {
            reply: Some(reply),
            ..Response::default()
        };
        let (shown, _) = render(&Display::default(), &request, &response).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&shown.text).unwrap(),
            json!({"n": 1, "ok": 1.0})
        );
        assert_eq!(shown.outcome, "OK");
        let failing = Response {
            reply: Some(json!({"n": 0, "writeErrors": [{"code": 11000}]})),
            ..Response::default()
        };
        assert_eq!(
            render(&Display::default(), &request, &failing)
                .unwrap()
                .0
                .outcome,
            "ERROR"
        );
    }

    #[test]
    fn rows_of_a_reply_field_and_column_options() {
        let request = Request::new("mongodb://h/admin", "admin", json!({"listDatabases": 1}));
        let response = Response {
            reply: Some(
                json!({"databases": [{"name": "app", "sizeOnDisk": {"$numberLong": "8192"}}]}),
            ),
            ..Response::default()
        };
        let display = Display {
            view: Some(View::Rows("databases")),
            ..Display::default()
        };
        let (shown, _) = render(&display, &request, &response).unwrap();
        assert!(shown.text.contains("sizeOnDisk") && shown.text.contains("8192"));
        let bad = Display {
            columns_type: Some("nope=json".into()),
            ..display
        };
        assert!(
            render(&bad, &request, &response)
                .unwrap_err()
                .contains("unknown result column")
        );
    }

    #[test]
    fn many_fields_are_capped() {
        let document: Map<String, Value> = (0..45).map(|i| (format!("f{i}"), json!(i))).collect();
        let (shown, _) = render(
            &Display::default(),
            &find(),
            &answer(vec![Value::Object(document)]),
        )
        .unwrap();
        assert!(shown.text.contains("5 more fields not shown"));
    }
}
