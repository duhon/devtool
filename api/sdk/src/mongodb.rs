//! The Host `mongodb` driver: one database command on a client cached per connection string,
//! with the official driver's topology handling (`mongodb+srv://`, replica sets, read
//! preference, failover), TLS, authentication and retryable writes.
//!
//! Documents cross the boundary as canonical Extended JSON v2 (`{"$numberInt": "1"}`,
//! `{"$date": {"$numberLong": …}}`, `{"$oid": …}`): it keeps every BSON type, so a document read
//! and written back keeps int32/int64/double apart. The command may be canonical or relaxed.
//!
//! Cursor commands (`find`, `aggregate`, `listCollections`, `listIndexes`, see [`Request::cursor`])
//! are iterated with `getMore` until the cursor ends or `maxDocuments` is reached; other commands
//! answer their reply. A server error (`ok: 0`) fails with stage `status` and `codeName (code)` in
//! the message; `writeErrors` of an `ok: 1` reply are part of the answer.
//!
//! A timeout or cancel closes the connection and kills an open cursor (`killCursors`). Reads stop
//! on the server when their client goes away; a write the server received is not rolled back, and
//! no write is retried beyond the official driver's retryable writes. Put `maxTimeMS` in the
//! command for a server-side limit. Credentials of the connection string never appear in errors.

use crate::{http::Timing, wire::DriverCall};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DRIVER: &str = "mongodb";

/// Commands that answer a cursor when [`Request::cursor`] is not set; `aggregate` with
/// `explain` does not.
pub const CURSOR_COMMANDS: [&str; 4] = ["find", "aggregate", "listCollections", "listIndexes"];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// `mongodb://` or `mongodb+srv://` connection string with its options; its `readPreference`
    /// applies to cursor commands, other commands go to the primary.
    pub uri: String,
    /// Defaults to the connection string's database.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// The command document, its name first: `{"find": "users", "filter": {…}}`, `{"ping": 1}`.
    pub command: Value,
    /// Whether the command answers a cursor to iterate; by default it does for [`CURSOR_COMMANDS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<bool>,
    /// Documents kept from a cursor; the cursor is then killed and the answer `truncated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_documents: Option<u64>,
    /// Documents per `getMore`; the first batch size is the command's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<u32>,
    /// The whole call, including server selection and every `getMore`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Server selection and connect timeout; defaults to the connection string's, else 10 s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<u64>,
}

impl Request {
    pub fn new(uri: &str, database: &str, command: Value) -> Self {
        Self {
            uri: uri.into(),
            database: Some(database.into()),
            command,
            ..Self::default()
        }
    }

    pub fn call(&self) -> DriverCall {
        DriverCall::new(DRIVER, self)
    }

    /// The command name: the first key of the command document.
    pub fn name(&self) -> Option<&str> {
        self.command.as_object()?.keys().next().map(String::as_str)
    }

    /// Whether the driver iterates a cursor for this request.
    pub fn is_cursor(&self) -> bool {
        self.cursor.unwrap_or_else(|| {
            self.name().is_some_and(|name| {
                CURSOR_COMMANDS.contains(&name)
                    && !(name == "aggregate" && self.command.get("explain").is_some())
            })
        })
    }
}

/// A call answer, or the end of a stream (then `documents` is empty: they came as [`Event`]s).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    /// The reply of a command without a cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<Value>,
    /// The documents of a cursor command.
    #[serde(default)]
    pub documents: Vec<Value>,
    /// Documents read from the cursor and kept.
    #[serde(default)]
    pub count: u64,
    /// The cursor had documents past `maxDocuments`.
    #[serde(default)]
    pub truncated: bool,
    /// `firstByteUs` is when the command's reply (the first batch) arrived.
    #[serde(default)]
    pub timing: Timing,
}

impl Response {
    pub fn parse(value: &Value) -> Result<Self, String> {
        Self::deserialize(value).map_err(|error| format!("Invalid mongodb answer: {error}"))
    }
}

/// One piece of a mongodb stream: the documents of a server batch, or the reply of a command
/// without a cursor. The `exit` info is the [`Response`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Event {
    Documents(Vec<Value>),
    Reply(Value),
}

impl Event {
    pub fn parse(value: &Value) -> Result<Self, String> {
        Self::deserialize(value).map_err(|error| format!("Invalid mongodb event: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cursor_commands_are_known_or_requested() {
        let request = |command| Request::new("mongodb://h", "db", command);
        assert!(request(json!({"find": "users", "filter": {}})).is_cursor());
        assert!(request(json!({"aggregate": "u", "pipeline": [], "cursor": {}})).is_cursor());
        assert!(!request(json!({"aggregate": "u", "pipeline": [], "explain": true})).is_cursor());
        assert!(!request(json!({"ping": 1})).is_cursor());
        let mut forced = request(json!({"ping": 1}));
        forced.cursor = Some(true);
        assert!(forced.is_cursor());

        let value = forced.call().request();
        assert_eq!(value["database"], "db");
        assert_eq!(Request::deserialize(&value).unwrap(), forced);
        assert_eq!(
            Event::parse(&json!({"documents": [{"a": {"$numberInt": "1"}}]})).unwrap(),
            Event::Documents(vec![json!({"a": {"$numberInt": "1"}})])
        );
    }
}
