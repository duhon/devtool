//! The mongodb driver: the official driver's `Client` per connection string, reused across calls;
//! one database command, its cursor iterated batch by batch. Documents become canonical Extended
//! JSON; a server error is the answer's `error`. `timeoutMs` also becomes the command's
//! `maxTimeMS`. A timeout or cancel drops the call: the connection is closed and the driver kills
//! an open cursor with `killCursors`. Credentials of the connection string are scrubbed from
//! failures.

use super::{CONNECT_MS, Failure, emit, micros, millis};
use bson::{Bson, Document, RawDocument};
use futures_util::StreamExt;
use mongodb::{
    Client,
    error::{Error, ErrorKind},
    options::ClientOptions,
    raw_batch_cursor::RawBatch,
};
use serde_json::{Value, json};
use services_sdk::{
    driver::Stage,
    mongodb::{Request, Response, ServerError},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// The server's `MaxTimeMSExpired` code.
const MAX_TIME_EXPIRED: i32 = 50;
const STOPPED: &str = "The connection was closed and an open cursor killed; a write the server \
                       received is not rolled back.";

/// Clients by connection string and `connectMs`.
type Clients = HashMap<(String, Option<u64>), Client>;

#[derive(Clone, Default)]
pub struct Mongodb {
    clients: Arc<Mutex<Clients>>,
}

impl Mongodb {
    /// With `events`, the stream sends `documents` per server batch, or the `reply`, and the
    /// answer carries neither.
    pub async fn run(
        &self,
        request: Request,
        cancel: CancellationToken,
        events: Option<&mpsc::Sender<Value>>,
    ) -> Result<Value, Failure> {
        let budget = request.timeout_ms.map(|ms| (Instant::now(), millis(ms)));
        let deadline = async {
            match request.timeout_ms {
                Some(ms) => tokio::time::sleep(millis(ms)).await,
                None => std::future::pending().await,
            }
        };
        let result = tokio::select! {
            result = self.execute(&request, budget, events) => result.map(|answer| json!(answer)),
            () = deadline => Err(stopped(Stage::Timeout, "The command timed out")),
            () = cancel.cancelled() => Err(stopped(Stage::Cancelled, "Cancelled")),
        };
        result.map_err(|failure| scrub(&request.uri, failure))
    }

    async fn client(&self, uri: &str, connect_ms: Option<u64>) -> Result<Client, Failure> {
        let key = (uri.to_owned(), connect_ms);
        if let Some(client) = self.lock().get(&key) {
            return Ok(client.clone());
        }
        let scheme = uri.trim_start().to_ascii_lowercase();
        if !scheme.starts_with("mongodb://") && !scheme.starts_with("mongodb+srv://") {
            return Err(Failure::new(
                Stage::Send,
                "The URI must use the mongodb:// or mongodb+srv:// scheme",
            ));
        }
        let connect = connect_ms.map(millis);
        // A mongodb+srv:// string is resolved (SRV and TXT records) while it is parsed.
        let mut options = tokio::time::timeout(
            connect.unwrap_or(millis(CONNECT_MS)),
            ClientOptions::parse(uri),
        )
        .await
        .map_err(|_| Failure::new(Stage::Timeout, "Resolving the URI timed out"))?
        .map_err(|error| failure(Stage::Send, error))?;
        if connect.is_some() {
            options.connect_timeout = connect;
            options.server_selection_timeout = connect;
        }
        options.connect_timeout.get_or_insert(millis(CONNECT_MS));
        options
            .server_selection_timeout
            .get_or_insert(millis(CONNECT_MS));
        options
            .app_name
            .get_or_insert_with(|| "Services DevTools".into());
        let client = Client::with_options(options).map_err(|error| failure(Stage::Send, error))?;
        Ok(self.lock().entry(key).or_insert(client).clone())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Clients> {
        self.clients.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn execute(
        &self,
        request: &Request,
        budget: Option<(Instant, Duration)>,
        events: Option<&mpsc::Sender<Value>>,
    ) -> Result<Response, Failure> {
        let started = Instant::now();
        let mut command = match &request.command {
            Value::Object(command) if !command.is_empty() => Document::try_from(command.clone())
                .map_err(|error| Failure::new(Stage::Send, format!("Invalid command: {error}")))?,
            _ => {
                return Err(Failure::new(
                    Stage::Send,
                    "The command must be a non-empty JSON object",
                ));
            }
        };
        if let Some(name) = &request.database {
            database_name(name)?;
        }
        let client = self.client(&request.uri, request.connect_ms).await?;
        let database = match &request.database {
            Some(name) => client.database(name),
            None => client.default_database().ok_or_else(|| {
                Failure::new(
                    Stage::Send,
                    "No database: neither the request nor the URI names one",
                )
            })?,
        };
        let mut answer = Response::default();
        let injected = budget.is_some_and(|(since, timeout)| {
            max_time(&mut command, timeout.saturating_sub(since.elapsed()))
        });
        if !request.is_cursor() {
            let reply = match database.run_command(command).await {
                Ok(reply) => reply,
                Err(error) => return answered(answer, error, injected, started),
            };
            answer.timing.first_byte_us = micros(started);
            let reply = Bson::Document(reply).into_canonical_extjson();
            match events {
                Some(events) => emit(events, json!({ "reply": reply })).await?,
                None => answer.reply = Some(reply),
            }
            answer.timing.total_us = micros(started);
            return Ok(answer);
        }
        let mut run = database.run_cursor_command(command);
        if let Some(size) = request.batch_size {
            run = run.batch_size(size);
        }
        // runCommand ignores the URI's read preference unless it is passed explicitly.
        if let Some(criteria) = client.selection_criteria() {
            run = run.selection_criteria(criteria.clone());
        }
        let mut cursor = match run.batch().await {
            Ok(cursor) => cursor,
            Err(error) => return answered(answer, error, injected, started),
        };
        answer.timing.first_byte_us = micros(started);
        let limit = request.max_documents.unwrap_or(u64::MAX);
        while let Some(batch) = cursor.next().await {
            let batch = match batch {
                Ok(batch) => batch,
                Err(error) => return answered(answer, error, injected, started),
            };
            let open = cursor_id(&batch) != 0;
            let mut slices = batch
                .doc_slices()
                .map_err(|error| failure(Stage::Decode, error))?
                .into_iter()
                .peekable();
            if answer.count >= limit {
                // Only an empty batch of a still open cursor leaves it unknown; count it as more.
                answer.truncated = slices.peek().is_some() || open;
                break;
            }
            let mut documents = Vec::new();
            while answer.count < limit {
                let Some(item) = slices.next() else { break };
                let item = item.map_err(|error| decode(error.to_string()))?;
                documents.push(extjson(item.as_document())?);
                answer.count += 1;
            }
            answer.truncated = slices.peek().is_some();
            match events {
                Some(events) if !documents.is_empty() => {
                    emit(events, json!({ "documents": documents })).await?;
                }
                Some(_) => {}
                None => answer.documents.extend(documents),
            }
            if answer.truncated {
                break;
            }
        }
        drop(cursor);
        answer.timing.total_us = micros(started);
        Ok(answer)
    }
}

/// Sets the command's `maxTimeMS` to what is left of `timeoutMs` unless the command has its own,
/// so the server stops it too. The server applies a cursor command's limit to its `getMore`s.
fn max_time(command: &mut Document, left: Duration) -> bool {
    if command.contains_key("maxTimeMS") {
        return false;
    }
    let ms = i64::try_from(left.as_millis()).unwrap_or(i64::MAX).max(1);
    command.insert("maxTimeMS", Bson::Int64(ms));
    true
}

/// A server error is the command's answer, with the documents read before it; the server stopping
/// the command at the injected `maxTimeMS` is the call's timeout.
fn answered(
    mut answer: Response,
    error: Error,
    injected: bool,
    started: Instant,
) -> Result<Response, Failure> {
    let ErrorKind::Command(command) = error.kind.as_ref() else {
        return Err(failure(Stage::Send, error));
    };
    if injected && command.code == MAX_TIME_EXPIRED {
        return Err(stopped(
            Stage::Timeout,
            "The command timed out: the server stopped it at maxTimeMS",
        ));
    }
    answer.error = Some(ServerError {
        code: command.code,
        code_name: command.code_name.clone(),
        message: command.message.clone(),
    });
    answer.timing.total_us = micros(started);
    Ok(answer)
}

/// Names MongoDB refuses for a database.
fn database_name(name: &str) -> Result<(), Failure> {
    if name.is_empty() || name.contains(['/', '\\', '.', ' ', '"', '$', '\0']) {
        return Err(Failure::new(
            Stage::Send,
            format!("Invalid database name \"{name}\""),
        ));
    }
    Ok(())
}

fn cursor_id(batch: &RawBatch) -> i64 {
    batch
        .as_raw_document()
        .get_document("cursor")
        .and_then(|cursor| cursor.get_i64("id"))
        .unwrap_or(0)
}

fn extjson(document: Option<&RawDocument>) -> Result<Value, Failure> {
    let document = document.ok_or_else(|| decode("A cursor item is not a document".into()))?;
    let document = Document::try_from(document).map_err(|error| decode(error.to_string()))?;
    Ok(Bson::Document(document).into_canonical_extjson())
}

fn decode(message: String) -> Failure {
    Failure::new(
        Stage::Decode,
        format!("Invalid BSON from the server: {message}"),
    )
}

fn stopped(stage: Stage, message: &str) -> Failure {
    let mut failure = Failure::new(stage, message);
    failure.failure.causes.push(STOPPED.into());
    failure
}

fn failure(fallback: Stage, error: Error) -> Failure {
    let mut failure = Failure::chain(fallback, &error);
    let (stage, message) = match error.kind.as_ref() {
        ErrorKind::DnsResolve { .. } => (Stage::Dns, format!("MongoDB DNS lookup failed: {error}")),
        ErrorKind::Authentication { .. } => (
            Stage::Connect,
            format!("MongoDB authentication failed: {error}"),
        ),
        ErrorKind::InvalidTlsConfig { .. } => (Stage::Tls, format!("MongoDB TLS failed: {error}")),
        ErrorKind::InvalidArgument { .. } => (Stage::Send, format!("MongoDB refused: {error}")),
        ErrorKind::Bson(_) | ErrorKind::InvalidResponse { .. } => {
            (Stage::Decode, format!("MongoDB answer is invalid: {error}"))
        }
        ErrorKind::ServerSelection { .. } => (
            match failure.failure.stage {
                Stage::Send => Stage::Connect,
                stage => stage,
            },
            format!("MongoDB server selection failed: {error}"),
        ),
        _ => (failure.failure.stage, format!("MongoDB failed: {error}")),
    };
    failure.failure.stage = stage;
    failure.message = message;
    failure
}

/// The failure without the URI's password, in any form it may take in a message.
fn scrub(uri: &str, mut failure: Failure) -> Failure {
    let secrets = secrets(uri);
    let clean = |text: &str| {
        secrets
            .iter()
            .fold(text.to_owned(), |text, secret| text.replace(secret, "****"))
    };
    failure.message = clean(&failure.message);
    for cause in &mut failure.failure.causes {
        *cause = clean(cause);
    }
    failure
}

/// The password of the URI's user info as written and percent-decoded, longest first.
fn secrets(uri: &str) -> Vec<String> {
    let Some((_, rest)) = uri.split_once("://") else {
        return vec![];
    };
    let authority = rest.split('?').next().unwrap_or_default();
    let Some((user_info, _)) = authority.rsplit_once('@') else {
        return vec![];
    };
    let Some((_, password)) = user_info.split_once(':') else {
        return vec![];
    };
    let mut secrets = vec![password.to_owned(), percent_decoded(password)];
    secrets.retain(|secret| !secret.is_empty());
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    secrets.dedup();
    secrets
}

fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                decoded.push(byte);
                i += 3;
            }
            (byte, _) => {
                decoded.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_removes_the_password_as_written_and_decoded() {
        let uri = "mongodb://admin:p%40ss%2Fw0rd@db1:27017,db2/app?authSource=admin";
        let mut failure = Failure::new(Stage::Connect, format!("Cannot use {uri}"));
        failure
            .failure
            .causes
            .push("auth p@ss/w0rd rejected".into());
        let failure = scrub(uri, failure);
        assert_eq!(
            failure.message,
            "Cannot use mongodb://admin:****@db1:27017,db2/app?authSource=admin"
        );
        assert_eq!(failure.failure.causes[1], "auth **** rejected");
        assert!(secrets("mongodb://admin@db/app").is_empty());
        assert!(secrets("mongodb+srv://db/app?x=a:b@c").is_empty());
    }

    #[test]
    fn max_time_is_the_budget_left_unless_the_command_sets_it() {
        let mut command = bson::doc! {"find": "c"};
        assert!(max_time(&mut command, Duration::from_millis(250)));
        assert_eq!(command, bson::doc! {"find": "c", "maxTimeMS": 250_i64});
        let mut spent = bson::doc! {"ping": 1};
        assert!(max_time(&mut spent, Duration::ZERO));
        assert_eq!(spent.get_i64("maxTimeMS").ok(), Some(1));
        let mut own = bson::doc! {"find": "c", "maxTimeMS": 5};
        assert!(!max_time(&mut own, Duration::from_millis(250)));
        assert_eq!(own, bson::doc! {"find": "c", "maxTimeMS": 5});
    }

    #[test]
    fn database_names_follow_mongodb_rules() {
        assert!(database_name("app_1-x").is_ok());
        for name in ["", "a.b", "a b", "a/b", "a$b"] {
            assert_eq!(database_name(name).unwrap_err().failure.stage, Stage::Send);
        }
    }
}
