//! MySQL: queries and schema discovery on a module-owned runtime.

mod dates;

use mysql_async::{Opts, Pool, Row, Value as MyValue, prelude::Queryable};
use serde_json::{Map, Value, json};
use services_contracts::columns;
use services_module_api::error::{Error, Result, required};
use services_module_api::sdk::{self, Call, Manifest, Service, Threads};
use services_module_api::transport::{EndpointArc, EndpointBox, Status};
use std::{collections::HashMap, sync::Arc, time::Instant};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct MysqlService {
    mysql: Mysql,
}

impl Service for MysqlService {
    fn manifest(&self) -> Manifest {
        Manifest {
            execute: true,
            discover: true,
            stream: false,
            ..Manifest::from_json(include_str!("../manifest.json"), env!("CARGO_PKG_VERSION"))
        }
    }

    /// `KILL QUERY` on stop.
    fn soft_cancel(&self) -> bool {
        true
    }

    async fn execute(&self, call: Call) -> Result<Value> {
        let options = columns::Options::parse(&call.params).map_err(Error::without_debug)?;
        let mut result = self
            .mysql
            .send_cancellable(&call.params, call.cancel)
            .await?;
        // The host attaches the request to this error, as it did before modules.
        options.apply(&mut result).map_err(Error::new)?;
        Ok(result)
    }

    async fn discover(&self, call: Call) -> Result<Value> {
        self.mysql
            .discover_cancellable(&call.params, call.cancel)
            .await
    }

    fn outcome(&self, result: &Value) -> &'static str {
        sdk::common_outcome(result).unwrap_or("OK")
    }
}

pub fn endpoint(host: EndpointArc) -> std::result::Result<EndpointBox, Status> {
    // The host may have installed the process-wide provider already.
    let _ = rustls::crypto::ring::default_provider().install_default();
    sdk::serve(MysqlService::default(), host, Threads::Own("mysql-module"))
}

services_module_api::export!(endpoint);

/// Server errors keep their code, SQL state and message.
fn mysql_error(e: mysql_async::Error) -> Error {
    let mut detail = json!({"name":"Error","message":e.to_string()});
    let mut message = e.to_string();
    if let mysql_async::Error::Server(server) = &e {
        message = server.message.clone();
        detail["message"] = json!(message);
        detail["errno"] = json!(server.code);
        detail["sqlState"] = json!(server.state);
        detail["sqlMessage"] = json!(server.message);
    }
    let mut error = Error::new(message);
    error.debug = Some(Box::new(json!({"error":detail})));
    error
}
#[derive(Clone, Default)]
pub struct Mysql {
    pools: Arc<Mutex<HashMap<String, Pool>>>,
}
impl Mysql {
    async fn pool(&self, dsn: &str) -> Result<Pool> {
        let url = url::Url::parse(dsn)
            .map_err(|_| Error::new(format!("Invalid connection string: {dsn}")))?;
        if url.scheme() != "mysql" {
            return Err(Error::new(format!(
                "Connection string must use the mysql:// scheme, got: {}:",
                url.scheme()
            )));
        }
        let mut pools = self.pools.lock().await;
        if let Some(pool) = pools.get(dsn) {
            return Ok(pool.clone());
        }
        let opts = Opts::from_url(dsn).map_err(|e| Error::new(e.to_string()))?;
        let pool = Pool::new(opts);
        pools.insert(dsn.into(), pool.clone());
        Ok(pool)
    }
    pub async fn send(&self, params: &Value) -> Result<Value> {
        self.send_cancellable(params, CancellationToken::new())
            .await
    }
    pub async fn send_cancellable(
        &self,
        params: &Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let dsn = required(params, "connectionString")?;
        let sql = required(params, "sql")?;
        let started = Instant::now();
        let request = json!({"connectionString":dsn,"sql":sql});
        let inner=async {
            let pool=self.pool(&dsn).await?;
            let mut conn=tokio::select! {
                result=pool.get_conn()=>result.map_err(mysql_error)?,
                _=cancel.cancelled()=>return Ok(json!({"cancelled":true,"phase":"acquiring-connection"})),
            };
            let id=conn.id();
            let result = {
            let query=async {
            let mut result=conn.query_iter(sql).await.map_err(mysql_error)?;
            let columns:Vec<String>=result.columns_ref().iter().map(|c|c.name_str().into_owned()).collect();
            let mut column_types:serde_json::Map<String,Value>=result.columns_ref().iter()
                .map(|column|(column.name_str().into_owned(),json!(column_kind(column)))).collect();
            let affected=result.affected_rows();
            let raw:Vec<Row>=result.collect().await.map_err(mysql_error)?;
            result.drop_result().await.map_err(mysql_error)?;
            let (columns,rows)=if columns.is_empty(){column_types.insert("affectedRows".into(),json!("number"));(vec!["affectedRows".into()],vec![json!({"affectedRows":affected})])}
                else {(columns.clone(),raw.into_iter().map(|row|row_json(&columns,row)).collect())};
            let duration=started.elapsed().as_millis();
            Ok(json!({"columns":columns,"columnTypes":column_types,"rows":rows,"durationMs":duration,"debug":{"request":request,"connection":{"threadId":id,"events":[{"event":"acquire","atMs":0,"data":{"threadId":id}},{"event":"release","atMs":duration,"data":{"threadId":id}}]},"response":{"columns":columns,"rows":rows}}}))
            };
            tokio::pin!(query);
            tokio::select! {
                result=&mut query=>Some(result),
                _=cancel.cancelled()=>None,
            }
            };
            if let Some(result) = result { return result; }
            // Keep the original connection checked out until KILL completes: its
            // thread ID must never be reused for another pooled request.
            let killed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                let opts = Opts::from_url(&dsn).map_err(|e|Error::new(e.to_string()))?;
                let mut control = mysql_async::Conn::new(opts).await.map_err(mysql_error)?;
                control.query_drop(format!("KILL QUERY {id}")).await.map_err(mysql_error)?;
                control.disconnect().await.map_err(mysql_error)
            }).await.map_err(|_|Error::new("MySQL cancellation timed out; query termination could not be confirmed"))
                .and_then(|result|result);
            let disconnected = tokio::time::timeout(std::time::Duration::from_millis(500), conn.disconnect()).await;
            killed.map_err(|e|Error::new(format!("MySQL cancellation failed: {e}. Already committed changes are not rolled back.")))?;
            match disconnected {
                Ok(Ok(())) => {}
                Ok(Err(error)) => eprintln!("Cancelled MySQL connection cleanup: {error}"),
                Err(_) => eprintln!("Cancelled MySQL connection cleanup timed out"),
            }
            Ok(json!({"cancelled":true,"threadId":id,"warning":"MySQL KILL QUERY completed. Already committed changes are not rolled back."}))
        }.await;
        inner.map_err(|mut error: Error| {
            error = error.with_debug(request);
            error.duration_ms = Some(started.elapsed().as_millis());
            error
        })
    }
    pub async fn discover(&self, params: &Value) -> Result<Value> {
        self.discover_cancellable(params, CancellationToken::new())
            .await
    }
    pub async fn discover_cancellable(
        &self,
        params: &Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let dsn = required(params, "connectionString")?;
        let result = self.send_cancellable(&json!({"connectionString":dsn,
            "sql":"SELECT table_name AS tableName, column_name AS columnName, column_type AS columnType, column_key AS columnKey FROM information_schema.columns WHERE table_schema = DATABASE() ORDER BY table_name, ordinal_position"}), cancel).await?;
        if result["cancelled"] == true {
            return Ok(result);
        }
        let rows = result["rows"]
            .as_array()
            .ok_or_else(|| Error::new("Invalid MySQL discovery rows"))?
            .iter()
            .map(|row| {
                let field = |name: &str| {
                    row[name].as_str().map(str::to_owned).ok_or_else(|| {
                        Error::new(format!("Invalid MySQL discovery column: {name}"))
                    })
                };
                Ok((
                    field("tableName")?,
                    field("columnName")?,
                    field("columnType")?,
                    field("columnKey")?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(discovery_rows(rows))
    }
}
pub fn discovery_rows(rows: Vec<(String, String, String, String)>) -> Value {
    let mut tables: Map<String, Value> = Map::new();
    for (table, column, kind, key) in rows {
        tables
            .entry(table.clone())
            .or_insert(json!({"name":table,"columns":[]}))["columns"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":column,"type":kind,"primaryKey":key=="PRI"}));
    }
    json!({"tables":tables.into_values().collect::<Vec<_>>()})
}
fn column_kind(column: &mysql_async::Column) -> services_contracts::columns::Kind {
    use mysql_async::consts::ColumnType::*;
    use services_contracts::columns::Kind;
    match column.column_type() {
        MYSQL_TYPE_TINY if column.column_length() == 1 => Kind::Boolean,
        MYSQL_TYPE_DECIMAL
        | MYSQL_TYPE_NEWDECIMAL
        | MYSQL_TYPE_TINY
        | MYSQL_TYPE_SHORT
        | MYSQL_TYPE_LONG
        | MYSQL_TYPE_LONGLONG
        | MYSQL_TYPE_INT24
        | MYSQL_TYPE_FLOAT
        | MYSQL_TYPE_DOUBLE
        | MYSQL_TYPE_YEAR => Kind::Number,
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE | MYSQL_TYPE_DATETIME | MYSQL_TYPE_TIMESTAMP
        | MYSQL_TYPE_TIME => Kind::Datetime,
        MYSQL_TYPE_JSON => Kind::Json,
        _ => Kind::Text,
    }
}
pub fn row_json(columns: &[String], row: Row) -> Value {
    let mut out = Map::new();
    let descriptors = row.columns_ref().to_vec();
    for ((column, value), descriptor) in columns.iter().zip(row.unwrap()).zip(descriptors) {
        let value = match value {
            MyValue::Bytes(bytes)
                if matches!(
                    descriptor.column_type(),
                    mysql_async::consts::ColumnType::MYSQL_TYPE_TINY
                        | mysql_async::consts::ColumnType::MYSQL_TYPE_SHORT
                        | mysql_async::consts::ColumnType::MYSQL_TYPE_LONG
                        | mysql_async::consts::ColumnType::MYSQL_TYPE_LONGLONG
                        | mysql_async::consts::ColumnType::MYSQL_TYPE_INT24
                        | mysql_async::consts::ColumnType::MYSQL_TYPE_YEAR
                ) =>
            {
                let raw = String::from_utf8_lossy(&bytes);
                raw.parse::<i64>()
                    .map(|v| json!(v))
                    .or_else(|_| raw.parse::<u64>().map(|v| json!(v)))
                    .unwrap_or_else(|_| json!(raw))
            }
            MyValue::Bytes(bytes)
                if matches!(
                    descriptor.column_type(),
                    mysql_async::consts::ColumnType::MYSQL_TYPE_FLOAT
                        | mysql_async::consts::ColumnType::MYSQL_TYPE_DOUBLE
                ) =>
            {
                let raw = String::from_utf8_lossy(&bytes);
                raw.parse::<f64>()
                    .map(|v| json!(v))
                    .unwrap_or_else(|_| json!(raw))
            }
            MyValue::Bytes(bytes)
                if matches!(
                    descriptor.column_type(),
                    mysql_async::consts::ColumnType::MYSQL_TYPE_DATE
                        | mysql_async::consts::ColumnType::MYSQL_TYPE_NEWDATE
                        | mysql_async::consts::ColumnType::MYSQL_TYPE_DATETIME
                        | mysql_async::consts::ColumnType::MYSQL_TYPE_TIMESTAMP
                ) =>
            {
                dates::text(
                    &String::from_utf8_lossy(&bytes),
                    matches!(
                        descriptor.column_type(),
                        mysql_async::consts::ColumnType::MYSQL_TYPE_DATE
                            | mysql_async::consts::ColumnType::MYSQL_TYPE_NEWDATE
                    ),
                )
            }
            MyValue::Bytes(bytes)
                if descriptor.column_type() == mysql_async::consts::ColumnType::MYSQL_TYPE_JSON =>
            {
                serde_json::from_slice(&bytes)
                    .unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)))
            }
            MyValue::Bytes(bytes)
                if descriptor.character_set() == 63
                    && matches!(
                        descriptor.column_type(),
                        mysql_async::consts::ColumnType::MYSQL_TYPE_BLOB
                            | mysql_async::consts::ColumnType::MYSQL_TYPE_TINY_BLOB
                            | mysql_async::consts::ColumnType::MYSQL_TYPE_MEDIUM_BLOB
                            | mysql_async::consts::ColumnType::MYSQL_TYPE_LONG_BLOB
                            | mysql_async::consts::ColumnType::MYSQL_TYPE_STRING
                            | mysql_async::consts::ColumnType::MYSQL_TYPE_VAR_STRING
                            | mysql_async::consts::ColumnType::MYSQL_TYPE_BIT
                    ) =>
            {
                json!({"type":"Buffer","data":bytes})
            }
            value => value_json(value),
        };
        out.insert(column.clone(), value);
    }
    Value::Object(out)
}
pub fn value_json(value: MyValue) -> Value {
    match value {
        MyValue::NULL => Value::Null,
        MyValue::Int(n) => json!(n),
        MyValue::UInt(n) => json!(n),
        MyValue::Float(n) => json!(n),
        MyValue::Double(n) => json!(n),
        MyValue::Bytes(v) => json!(String::from_utf8_lossy(&v)),
        MyValue::Date(y, m, d, h, min, s, micros) => dates::binary((y, m, d, h, min, s, micros)),
        MyValue::Time(negative, days, h, m, s, micros) => json!(format!(
            "{}{:02}:{m:02}:{s:02}{}",
            if negative { "-" } else { "" },
            days * 24 + h as u32,
            if micros > 0 {
                format!(".{micros:06}")
            } else {
                String::new()
            }
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_values_become_grid_cells() {
        for (wire, cell) in [
            (MyValue::NULL, Value::Null),
            (MyValue::Int(-3), json!(-3)),
            (MyValue::Bytes(b"decimal".to_vec()), json!("decimal")),
        ] {
            assert_eq!(value_json(wire.clone()), cell, "{wire:?}");
        }
    }
}
