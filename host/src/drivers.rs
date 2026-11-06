//! Drivers: protocols the App cannot speak from the browser. They know no services; a module
//! describes the call, a driver runs it.
//!
//! `{"service":"drivers","driver":"http","op":"call"|"stream","request":{…}}`. A call answers
//! `{"ok":true,"result":…}`; a stream sends `data` events and ends with `exit`. A failure is
//! `{"ok":false,"error":…,"failure":{"stage":…,"causes":[…]}}`. `stop` cancels by id.

mod http;

use serde_json::{Value, json};
use services_sdk::driver::{DriverFailure, Stage};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Connect timeout when the call sets none.
pub const CONNECT_MS: u64 = 10_000;

/// The drivers and their versions, for `health`.
pub fn versions() -> Value {
    json!({ services_sdk::http::DRIVER: "1" })
}

#[derive(Debug)]
pub struct Failure {
    pub message: String,
    pub failure: DriverFailure,
}

impl Failure {
    pub fn new(stage: Stage, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            failure: DriverFailure {
                stage,
                causes: vec![message.clone()],
            },
            message,
        }
    }

    pub fn reply(&self, id: &Value) -> Value {
        json!({"id": id, "ok": false, "error": self.message, "failure": self.failure})
    }
}

#[derive(Clone)]
pub struct Drivers {
    http: http::Http,
}

impl Drivers {
    pub fn new() -> crate::Result<Self> {
        Ok(Self {
            http: http::Http::new().map_err(|failure| crate::Error::new(failure.message))?,
        })
    }

    pub async fn call(&self, message: &Value, cancel: CancellationToken) -> Result<Value, Failure> {
        let work = async {
            match driver(message)? {
                services_sdk::http::DRIVER => self.http.call(request(message)?).await,
                _ => unreachable!(),
            }
        };
        cancellable(work, cancel).await
    }

    pub async fn stream(
        &self,
        message: &Value,
        cancel: CancellationToken,
        events: mpsc::Sender<Value>,
    ) -> Result<Value, Failure> {
        let work = async {
            match driver(message)? {
                services_sdk::http::DRIVER => self.http.stream(request(message)?, events).await,
                _ => unreachable!(),
            }
        };
        cancellable(work, cancel).await
    }
}

fn driver(message: &Value) -> Result<&'static str, Failure> {
    match message["driver"].as_str() {
        Some(services_sdk::http::DRIVER) => Ok(services_sdk::http::DRIVER),
        other => Err(Failure::new(
            Stage::Send,
            format!("Unknown driver \"{}\"", other.unwrap_or_default()),
        )),
    }
}

fn request<T: serde::de::DeserializeOwned>(message: &Value) -> Result<T, Failure> {
    T::deserialize(&message["request"])
        .map_err(|error| Failure::new(Stage::Send, format!("Invalid request: {error}")))
}

async fn cancellable(
    work: impl Future<Output = Result<Value, Failure>>,
    cancel: CancellationToken,
) -> Result<Value, Failure> {
    tokio::select! {
        result = work => result,
        () = cancel.cancelled() => Err(Failure::new(Stage::Cancelled, "Cancelled")),
    }
}

fn millis(ms: u64) -> Duration {
    Duration::from_millis(ms)
}
