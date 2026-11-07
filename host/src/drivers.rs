//! Drivers: protocols the App cannot speak from the browser. They know no services; a module
//! describes the call, a driver runs it.
//!
//! `{"service":"drivers","driver":"http"|"grpc"|"mysql"|"process"|"tcp","op":"call"|"stream",
//! "request":{…}}`. A call answers `{"ok":true,"result":…}`; a stream sends `data` events and
//! ends with `exit`. A failure is `{"ok":false,"error":…,"failure":{"stage":…,"causes":[…]}}`.
//! `stop` cancels by id; the failure stage is then `cancelled`.

mod grpc;
mod http;
mod mysql;
mod process;
mod tcp;

use serde_json::{Value, json};
use services_sdk::driver::{DriverFailure, Stage};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Connect timeout when the call sets none.
pub const CONNECT_MS: u64 = 10_000;

/// The drivers and their versions, for `health`.
pub fn versions() -> Value {
    json!({
        services_sdk::http::DRIVER: "1",
        services_sdk::grpc::DRIVER: "1",
        services_sdk::mysql::DRIVER: "1",
        services_sdk::process::DRIVER: "1",
        services_sdk::tcp::DRIVER: "1",
    })
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

    /// A failure with the message and its cause chain; the stage comes from the causes when
    /// they name one, else it is `fallback`.
    pub fn chain(fallback: Stage, error: &(dyn std::error::Error + 'static)) -> Self {
        let mut failure = Self::at(fallback, error);
        failure.failure.stage = stage(fallback, &failure.failure.causes);
        failure
    }

    /// A failure at `stage` with the error's cause chain.
    pub fn at(stage: Stage, error: &(dyn std::error::Error + 'static)) -> Self {
        let mut causes = vec![error.to_string()];
        let mut source = error.source();
        while let Some(cause) = source.filter(|_| causes.len() < 9) {
            causes.push(cause.to_string());
            source = cause.source();
        }
        let mut failure = Self::new(stage, error.to_string());
        failure.failure.causes = causes;
        failure
    }

    pub fn reply(&self, id: &Value) -> Value {
        json!({"id": id, "ok": false, "error": self.message, "failure": self.failure})
    }
}

/// The stage a cause chain names: lookup, TLS, connect or timeout; `fallback` otherwise.
fn stage(fallback: Stage, causes: &[String]) -> Stage {
    let all = causes.join("\n").to_lowercase();
    let any = |words: &[&str]| words.iter().any(|word| all.contains(word));
    if any(&[
        "dns error",
        "failed to lookup",
        "nodename nor servname",
        "name or service not known",
    ]) {
        Stage::Dns
    } else if any(&["certificate", "tls", "handshake"]) {
        Stage::Tls
    } else if any(&["timed out", "timeout expired", "deadline has elapsed"]) {
        Stage::Timeout
    } else if any(&[
        "connection refused",
        "tcp connect error",
        "error trying to connect",
        "unreachable",
    ]) {
        Stage::Connect
    } else {
        fallback
    }
}

#[derive(Clone)]
pub struct Drivers {
    http: http::Http,
    grpc: grpc::Grpc,
    mysql: mysql::Mysql,
}

impl Drivers {
    pub fn new() -> crate::Result<Self> {
        Ok(Self {
            http: http::Http::new().map_err(|failure| crate::Error::new(failure.message))?,
            grpc: grpc::Grpc::default(),
            mysql: mysql::Mysql::default(),
        })
    }

    /// The process and mysql drivers handle `cancel` themselves: they stop the process group or
    /// kill the query before they answer. The others drop the call.
    pub async fn call(&self, message: &Value, cancel: CancellationToken) -> Result<Value, Failure> {
        use services_sdk::{grpc, http, mysql, tcp};
        match driver(message)? {
            http::DRIVER => cancellable(self.http.call(request(message)?), cancel).await,
            grpc::DRIVER => cancellable(self.grpc.run(request(message)?, None), cancel).await,
            tcp::DRIVER => cancellable(self::tcp::run(request(message)?, None), cancel).await,
            mysql::DRIVER => self.mysql.run(request(message)?, cancel, None).await,
            _ => self::process::run(request(message)?, cancel, None).await,
        }
    }

    pub async fn stream(
        &self,
        message: &Value,
        cancel: CancellationToken,
        events: mpsc::Sender<Value>,
    ) -> Result<Value, Failure> {
        use services_sdk::{grpc, http, mysql, tcp};
        match driver(message)? {
            http::DRIVER => cancellable(self.http.stream(request(message)?, events), cancel).await,
            grpc::DRIVER => {
                cancellable(self.grpc.run(request(message)?, Some(&events)), cancel).await
            }
            tcp::DRIVER => {
                cancellable(self::tcp::run(request(message)?, Some(&events)), cancel).await
            }
            mysql::DRIVER => {
                self.mysql
                    .run(request(message)?, cancel, Some(&events))
                    .await
            }
            _ => self::process::run(request(message)?, cancel, Some(&events)).await,
        }
    }
}

fn driver(message: &Value) -> Result<&'static str, Failure> {
    use services_sdk::{grpc, http, mysql, tcp};
    [
        http::DRIVER,
        grpc::DRIVER,
        mysql::DRIVER,
        services_sdk::process::DRIVER,
        tcp::DRIVER,
    ]
    .into_iter()
    .find(|name| message["driver"] == *name)
    .ok_or_else(|| {
        Failure::new(
            Stage::Send,
            format!(
                "Unknown driver \"{}\"",
                message["driver"].as_str().unwrap_or_default()
            ),
        )
    })
}

/// Sends a stream event; a reader that went away cancels the stream.
async fn emit(events: &mpsc::Sender<Value>, event: Value) -> Result<(), Failure> {
    events
        .send(event)
        .await
        .map_err(|_| Failure::new(Stage::Cancelled, "The stream reader went away"))
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

fn micros(started: std::time::Instant) -> u64 {
    started.elapsed().as_micros() as u64
}
