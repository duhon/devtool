//! The mysql driver: a pool per URL, text-protocol queries with every result set and its column
//! types. Cancel and a timeout run `KILL QUERY` on a separate connection.

use super::{CONNECT_MS, Failure, emit, micros, millis};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use mysql_async::{
    Conn, Opts, Pool, Row, Value as MyValue, consts::ColumnType, prelude::Queryable,
};
use serde_json::{Value, json};
use services_sdk::{
    driver::Stage,
    mysql::{BINARY_CHARSET, Column, Request, Response, ResultSet, ServerError, Summary},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Rows per `rows` event of a stream.
const BATCH: usize = 500;
const KILL_MS: u64 = 2_000;

#[derive(Clone, Default)]
pub struct Mysql {
    pools: Arc<Mutex<HashMap<String, Pool>>>,
}

impl Mysql {
    fn pool(&self, url: &str) -> Result<Pool, Failure> {
        let invalid = |message: String| Failure::new(Stage::Send, message);
        if !url
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("mysql://")
        {
            return Err(invalid("The URL must use the mysql:// scheme".into()));
        }
        let mut pools = self.pools.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pool) = pools.get(url) {
            return Ok(pool.clone());
        }
        let opts = Opts::from_url(url).map_err(|error| invalid(format!("Invalid URL: {error}")))?;
        let pool = Pool::new(opts);
        pools.insert(url.into(), pool.clone());
        Ok(pool)
    }

    /// With `events`, the stream sends `columns`, `rows` and `done` per result set and the
    /// answer carries no sets.
    pub async fn run(
        &self,
        request: Request,
        cancel: CancellationToken,
        events: Option<&mpsc::Sender<Value>>,
    ) -> Result<Value, Failure> {
        let started = Instant::now();
        let pool = self.pool(&request.url)?;
        let connect = tokio::time::timeout(
            millis(request.connect_ms.unwrap_or(CONNECT_MS)),
            pool.get_conn(),
        );
        let mut conn = tokio::select! {
            result = connect => result
                .map_err(|_| Failure::new(Stage::Timeout, "Connecting timed out"))?
                .map_err(|error| failure(Stage::Connect, error))?,
            () = cancel.cancelled() => return Err(Failure::new(Stage::Cancelled, "Cancelled")),
        };
        let thread_id = conn.id();
        let deadline = async {
            match request.timeout_ms {
                Some(ms) => tokio::time::sleep(millis(ms)).await,
                None => std::future::pending().await,
            }
        };
        let stopped = {
            let query = query(&mut conn, &request, events, started);
            tokio::select! {
                result = query => {
                    let mut answer = result?;
                    answer.thread_id = thread_id;
                    answer.timing.total_us = micros(started);
                    return Ok(json!(answer));
                }
                () = cancel.cancelled() => Stage::Cancelled,
                () = deadline => Stage::Timeout,
            }
        };
        // The connection stays checked out until KILL is done, so its id is not reused.
        let killed = tokio::time::timeout(millis(KILL_MS), async {
            let opts = Opts::from_url(&request.url).map_err(mysql_async::Error::from)?;
            let mut control = Conn::new(opts).await?;
            control
                .query_drop(format!("KILL QUERY {thread_id}"))
                .await?;
            control.disconnect().await
        })
        .await;
        let _ = tokio::time::timeout(millis(500), conn.disconnect()).await;
        let outcome = match killed {
            Ok(Ok(())) => format!(
                "KILL QUERY {thread_id} completed. Already committed changes are not rolled back."
            ),
            Ok(Err(error)) => format!(
                "KILL QUERY {thread_id} failed: {error}. Already committed changes are not rolled back."
            ),
            Err(_) => format!(
                "KILL QUERY {thread_id} timed out; query termination could not be confirmed."
            ),
        };
        let mut failure = Failure::new(
            stopped,
            match stopped {
                Stage::Timeout => "The query timed out",
                _ => "Cancelled",
            },
        );
        failure.failure.causes.push(outcome);
        Err(failure)
    }
}

async fn query(
    conn: &mut Conn,
    request: &Request,
    events: Option<&mpsc::Sender<Value>>,
    started: Instant,
) -> Result<Response, Failure> {
    let mut answer = Response::default();
    let mut result = match conn.query_iter(request.sql.as_str()).await {
        Ok(result) => result,
        Err(error) => return answered(answer, error),
    };
    answer.timing.first_byte_us = micros(started);
    loop {
        let columns: Vec<Column> = result.columns_ref().iter().map(column).collect();
        // The summary of a set without rows is known before it is read.
        let mut summary = Summary {
            affected_rows: result.affected_rows(),
            last_insert_id: result.last_insert_id(),
            warnings: result.warnings(),
            info: result.info().into_owned(),
            truncated: false,
        };
        if let Some(events) = events {
            emit(events, json!({ "columns": columns })).await?;
        }
        let mut rows = Vec::new();
        let mut kept = 0u64;
        loop {
            let row = match result.next().await {
                Ok(Some(row)) => row,
                Ok(None) => break,
                Err(error) => {
                    flush(events, &mut rows).await?;
                    answer.sets.push(ResultSet {
                        columns,
                        rows,
                        summary,
                    });
                    return answered(answer, error);
                }
            };
            if request.max_rows.is_some_and(|max| kept >= max) {
                summary.truncated = true;
                continue;
            }
            kept += 1;
            rows.push(cells(row));
            if events.is_some() && rows.len() >= BATCH {
                flush(events, &mut rows).await?;
            }
        }
        flush(events, &mut rows).await?;
        match events {
            Some(events) => emit(events, json!({ "done": summary })).await?,
            None => answer.sets.push(ResultSet {
                columns,
                rows,
                summary,
            }),
        }
        if result.is_empty() {
            return Ok(answer);
        }
    }
}

async fn flush(
    events: Option<&mpsc::Sender<Value>>,
    rows: &mut Vec<Vec<Value>>,
) -> Result<(), Failure> {
    match events {
        Some(events) if !rows.is_empty() => {
            emit(events, json!({ "rows": std::mem::take(rows) })).await
        }
        _ => Ok(()),
    }
}

/// A server error is the query's answer; anything else fails the call.
fn answered(mut answer: Response, error: mysql_async::Error) -> Result<Response, Failure> {
    match error {
        mysql_async::Error::Server(server) => {
            answer.error = Some(ServerError {
                code: server.code,
                state: server.state,
                message: server.message,
            });
            Ok(answer)
        }
        error => Err(failure(Stage::Send, error)),
    }
}

fn failure(fallback: Stage, error: mysql_async::Error) -> Failure {
    let mut failure = Failure::chain(fallback, &error);
    failure.message = format!("MySQL failed: {error}");
    failure
}

fn column(column: &mysql_async::Column) -> Column {
    let kind = format!("{:?}", column.column_type());
    Column {
        name: column.name_str().into_owned(),
        kind: kind.trim_start_matches("MYSQL_TYPE_").to_ascii_lowercase(),
        length: column.column_length(),
        charset: column.character_set(),
        flags: column.flags().bits(),
        decimals: column.decimals(),
    }
}

fn cells(row: Row) -> Vec<Value> {
    let columns = row.columns();
    row.unwrap()
        .into_iter()
        .zip(columns.iter())
        .map(|(value, column)| cell(value, column))
        .collect()
}

fn cell(value: MyValue, column: &mysql_async::Column) -> Value {
    use ColumnType::*;
    match value {
        MyValue::NULL => Value::Null,
        MyValue::Int(n) => json!(n),
        MyValue::UInt(n) => json!(n),
        MyValue::Float(n) => json!(n),
        MyValue::Double(n) => json!(n),
        MyValue::Bytes(bytes) => match column.column_type() {
            MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT | MYSQL_TYPE_LONG | MYSQL_TYPE_LONGLONG
            | MYSQL_TYPE_INT24 | MYSQL_TYPE_YEAR => {
                let raw = String::from_utf8_lossy(&bytes);
                raw.parse::<i64>()
                    .map(|n| json!(n))
                    .or_else(|_| raw.parse::<u64>().map(|n| json!(n)))
                    .unwrap_or_else(|_| json!(raw))
            }
            MYSQL_TYPE_FLOAT | MYSQL_TYPE_DOUBLE => {
                let raw = String::from_utf8_lossy(&bytes);
                raw.parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite())
                    .map_or_else(|| json!(raw), |n| json!(n))
            }
            MYSQL_TYPE_BLOB
            | MYSQL_TYPE_TINY_BLOB
            | MYSQL_TYPE_MEDIUM_BLOB
            | MYSQL_TYPE_LONG_BLOB
            | MYSQL_TYPE_STRING
            | MYSQL_TYPE_VAR_STRING
            | MYSQL_TYPE_VARCHAR
            | MYSQL_TYPE_BIT
            | MYSQL_TYPE_GEOMETRY
                if column.character_set() == BINARY_CHARSET =>
            {
                json!({ "base64": B64.encode(&bytes) })
            }
            _ => json!(String::from_utf8_lossy(&bytes)),
        },
        MyValue::Date(year, month, day, hour, minute, second, micros) => {
            let date = format!("{year:04}-{month:02}-{day:02}");
            if matches!(column.column_type(), MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE) {
                json!(date)
            } else {
                json!(format!(
                    "{date} {hour:02}:{minute:02}:{second:02}{}",
                    fraction(micros)
                ))
            }
        }
        MyValue::Time(negative, days, hours, minutes, seconds, micros) => json!(format!(
            "{}{:02}:{minutes:02}:{seconds:02}{}",
            if negative { "-" } else { "" },
            days * 24 + u32::from(hours),
            fraction(micros)
        )),
    }
}

fn fraction(micros: u32) -> String {
    if micros > 0 {
        format!(".{micros:06}")
    } else {
        String::new()
    }
}
