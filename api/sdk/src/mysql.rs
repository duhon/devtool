//! The Host `mysql` driver: one SQL text (several statements allowed) on a pooled connection;
//! every result set comes back with its column types. Schema discovery is just a query.
//! Cells are JSON: `null`, a number for integer and float columns, `{"base64": …}` for binary
//! columns, text for everything else (decimals, dates, JSON documents) as the server sent it.

use crate::{http::Timing, wire::DriverCall};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DRIVER: &str = "mysql";

/// The `charset` of binary columns.
pub const BINARY_CHARSET: u16 = 63;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// `mysql://user:password@host:port/db?options`; TLS is chosen by its options.
    pub url: String,
    pub sql: String,
    /// Rows kept per result set; the rest are read and dropped, the set is `truncated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<u64>,
    /// The query; when it passes, the driver runs `KILL QUERY` as on cancel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<u64>,
}

impl Request {
    pub fn new(url: &str, sql: &str) -> Self {
        Self {
            url: url.into(),
            sql: sql.into(),
            ..Self::default()
        }
    }
    pub fn call(&self) -> DriverCall {
        DriverCall::new(DRIVER, self)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub name: String,
    /// The protocol type without `MYSQL_TYPE_`, lower case: `long`, `newdecimal`, `datetime`, `json`…
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub length: u32,
    #[serde(default)]
    pub charset: u16,
    #[serde(default)]
    pub flags: u16,
    #[serde(default)]
    pub decimals: u8,
}

impl Column {
    pub fn binary(&self) -> bool {
        self.charset == BINARY_CHARSET
    }
}

/// What the server reported for a result set besides its rows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    #[serde(default)]
    pub affected_rows: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_insert_id: Option<u64>,
    #[serde(default)]
    pub warnings: u16,
    #[serde(default)]
    pub info: String,
    /// Rows past `maxRows` were dropped.
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultSet {
    /// Empty for a statement without rows (`UPDATE`, `SET`).
    #[serde(default)]
    pub columns: Vec<Column>,
    #[serde(default)]
    pub rows: Vec<Vec<Value>>,
    #[serde(flatten)]
    pub summary: Summary,
}

/// An error the server answered with: the query ran and failed. Result sets before it stay.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerError {
    pub code: u16,
    #[serde(default)]
    pub state: String,
    pub message: String,
}

/// A call answer, or the end of a stream (then `sets` is empty: they came as [`Event`]s).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    #[serde(default)]
    pub sets: Vec<ResultSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ServerError>,
    /// The server connection id, the one `KILL QUERY` names.
    #[serde(default)]
    pub thread_id: u32,
    #[serde(default)]
    pub timing: Timing,
}

impl Response {
    pub fn parse(value: &Value) -> Result<Self, String> {
        Self::deserialize(value).map_err(|error| format!("Invalid mysql answer: {error}"))
    }
}

/// One piece of a mysql stream, per result set: `columns`, `rows` in batches, then `done`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Event {
    Columns(Vec<Column>),
    Rows(Vec<Vec<Value>>),
    Done(Summary),
}

impl Event {
    pub fn parse(value: &Value) -> Result<Self, String> {
        Self::deserialize(value).map_err(|error| format!("Invalid mysql event: {error}"))
    }
}

/// The bytes of a `{"base64": …}` cell.
pub fn binary(cell: &Value) -> Option<Vec<u8>> {
    crate::driver::decode(cell.get("base64")?.as_str()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_and_response_round_trip() {
        let mut request = Request::new("mysql://u@h/db", "SELECT 1");
        request.max_rows = Some(10);
        let value = request.call().request();
        assert_eq!(value["maxRows"], 10);
        assert_eq!(Request::deserialize(&value).unwrap(), request);

        let answer = json!({
            "sets": [
                {"columns": [{"name": "id", "type": "long", "length": 11, "charset": 63,
                    "flags": 0, "decimals": 0}], "rows": [[42], [{"base64": "AQ=="}]],
                 "affectedRows": 0, "warnings": 0, "info": "", "truncated": true},
                {"columns": [], "rows": [], "affectedRows": 7, "lastInsertId": 3,
                 "warnings": 1, "info": "Rows matched: 7", "truncated": false}
            ],
            "error": {"code": 1064, "state": "42000", "message": "syntax"},
            "threadId": 5, "timing": {"firstByteUs": 1, "totalUs": 2}
        });
        let response = Response::parse(&answer).unwrap();
        assert!(response.sets[0].columns[0].binary());
        assert!(response.sets[0].summary.truncated);
        assert_eq!(binary(&response.sets[0].rows[1][0]), Some(vec![1]));
        assert_eq!(response.sets[1].summary.last_insert_id, Some(3));
        assert_eq!(response.error.as_ref().unwrap().code, 1064);
        assert_eq!(serde_json::to_value(&response).unwrap(), answer);
        assert_eq!(
            Event::parse(&json!({"rows": [[1, "a"]]})).unwrap(),
            Event::Rows(vec![vec![json!(1), json!("a")]])
        );
    }
}
