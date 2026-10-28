use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct Error {
    pub message: String,
    pub debug: Option<Box<Value>>,
    pub duration_ms: Option<u128>,
    pub history: Option<Box<Value>>,
}
impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            debug: None,
            duration_ms: None,
            history: None,
        }
    }
    pub fn with_debug(mut self, request: Value) -> Self {
        if let Some(debug) = &mut self.debug {
            debug["request"] = request;
        } else {
            self.debug = Some(Box::new(
                json!({"request":request,"error":{"name":"Error","message":self.message}}),
            ));
        }
        self
    }
    pub fn json(&self) -> Value {
        let mut value = json!({"error":self.message});
        if let Some(debug) = &self.debug {
            value["debug"] = (**debug).clone();
        }
        if let Some(duration) = self.duration_ms {
            value["durationMs"] = json!(duration);
        }
        if let Some(history) = &self.history {
            value["history"] = (**history).clone();
        }
        value
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::new(e.to_string())
    }
}
impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        fn details(error: &dyn std::error::Error, depth: usize) -> Value {
            let mut value = json!({"name":"Error","message":error.to_string()});
            if depth < 8
                && let Some(cause) = error.source()
            {
                value["cause"] = details(cause, depth + 1);
            }
            value
        }
        let detail = details(&e, 0);
        let mut error = Self::new(format!("Request failed: {e}"));
        error.debug = Some(Box::new(
            json!({"error":detail,"connection":{"events":[],"error":detail}}),
        ));
        error
    }
}
impl From<mysql_async::Error> for Error {
    fn from(e: mysql_async::Error) -> Self {
        let mut detail = json!({"name":"Error","message":e.to_string()});
        let mut message = e.to_string();
        if let mysql_async::Error::Server(server) = &e {
            message = server.message.clone();
            detail["message"] = json!(message);
            detail["errno"] = json!(server.code);
            detail["sqlState"] = json!(server.state);
            detail["sqlMessage"] = json!(server.message);
        }
        Self {
            message,
            debug: Some(Box::new(json!({"error":detail}))),
            duration_ms: None,
            history: None,
        }
    }
}
pub type Result<T> = std::result::Result<T, Error>;
pub fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}
pub fn string(v: &Value) -> String {
    v.as_str()
        .map(str::to_string)
        .unwrap_or_else(|| v.to_string())
}
pub fn required(v: &Value, key: &str) -> Result<String> {
    let value = text(v, key);
    if value.trim().is_empty() {
        Err(Error::new(format!("{key} is required")))
    } else {
        Ok(value)
    }
}
