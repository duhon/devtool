use crate::action::Format;
use kafka_contract::{DeliveryStatus, FetchSummary, Operation, Record, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use services_sdk::{
    Rendered,
    lang::table::{self, Column, Kind},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LagRow {
    pub group: String,
    pub topic: String,
    pub partition: i32,
    pub committed: Option<kafka_contract::Offset>,
    pub end: Option<kafka_contract::Offset>,
    pub lag: Option<i64>,
    pub error: Option<String>,
}

fn grid(
    columns: Vec<Column>,
    rows: Vec<Vec<Value>>,
    format: Format,
    raw: &impl Serialize,
    warning: bool,
) -> Rendered {
    Rendered {
        text: if format == Format::Json {
            serde_json::to_string_pretty(raw).unwrap_or_default()
        } else {
            table::show(
                &columns,
                &rows,
                (format == Format::Table).then_some(table::Format::Table),
                format == Format::Vertical,
            )
        },
        language: if format == Format::Json {
            "json"
        } else {
            "kafka"
        }
        .into(),
        columns: table::types(&columns),
        outcome: if warning { "WARNING" } else { "OK" }.into(),
    }
}

fn columns(names: &[(&str, Kind)]) -> Vec<Column> {
    names
        .iter()
        .map(|(name, kind)| Column::new(*name, *kind))
        .collect()
}

fn error(error: &Option<kafka_contract::KafkaError>) -> Value {
    error
        .as_ref()
        .map_or(Value::Null, |e| json!(format!("{}: {}", e.code, e.message)))
}

pub fn response(response: &Response, operation: &Operation, format: Format) -> Rendered {
    use Kind::{Boolean as B, Json as J, Number as N, Text as T};
    let (cols, rows, warning) = match response {
        Response::Cluster(cluster) => (
            columns(&[("id", N), ("host", T), ("port", N), ("controller", B)]),
            cluster
                .brokers
                .iter()
                .map(|b| {
                    vec![
                        json!(b.id),
                        json!(b.host),
                        json!(b.port),
                        json!(cluster.controller_id == Some(b.id)),
                    ]
                })
                .collect(),
            false,
        ),
        Response::Topics(topics) if matches!(operation, Operation::ListTopics) => (
            columns(&[
                ("topic", T),
                ("internal", B),
                ("partitions", N),
                ("error", T),
            ]),
            topics
                .items
                .iter()
                .map(|t| {
                    vec![
                        json!(t.name),
                        json!(t.internal),
                        json!(t.partitions.len()),
                        error(&t.error),
                    ]
                })
                .collect(),
            topics.truncated
                || topics
                    .items
                    .iter()
                    .any(|t| t.error.is_some() || t.partitions.iter().any(|p| p.error.is_some())),
        ),
        Response::Topics(topics) => {
            let mut rows = Vec::new();
            for topic in &topics.items {
                if topic.partitions.is_empty() {
                    rows.push(vec![
                        json!(topic.name),
                        Value::Null,
                        Value::Null,
                        json!([]),
                        json!([]),
                        error(&topic.error),
                    ]);
                }
                for p in &topic.partitions {
                    rows.push(vec![
                        json!(topic.name),
                        json!(p.id),
                        json!(p.leader),
                        json!(p.replicas),
                        json!(p.isr),
                        error(&p.error.clone().or_else(|| topic.error.clone())),
                    ]);
                }
            }
            (
                columns(&[
                    ("topic", T),
                    ("partition", N),
                    ("leader", N),
                    ("replicas", J),
                    ("isr", J),
                    ("error", T),
                ]),
                rows,
                topics.truncated
                    || topics.items.iter().any(|t| {
                        t.error.is_some() || t.partitions.iter().any(|p| p.error.is_some())
                    }),
            )
        }
        Response::Offsets(offsets) => (
            columns(&[
                ("topic", T),
                ("partition", N),
                ("offset", T),
                ("timestampMs", N),
                ("error", T),
            ]),
            offsets
                .items
                .iter()
                .map(|o| {
                    vec![
                        json!(o.topic),
                        json!(o.partition),
                        json!(o.offset),
                        json!(o.timestamp_ms),
                        error(&o.error),
                    ]
                })
                .collect(),
            offsets.truncated || offsets.items.iter().any(|o| o.error.is_some()),
        ),
        Response::Groups(groups) => (
            columns(&[
                ("group", T),
                ("state", T),
                ("protocol", T),
                ("members", J),
                ("error", T),
            ]),
            groups
                .items
                .iter()
                .map(|g| {
                    vec![
                        json!(g.id),
                        json!(g.state),
                        json!(g.protocol),
                        json!(g.members),
                        error(&g.error),
                    ]
                })
                .collect(),
            groups.truncated || groups.items.iter().any(|g| g.error.is_some()),
        ),
        Response::GroupOffsets(offsets) => (
            columns(&[
                ("group", T),
                ("topic", T),
                ("partition", N),
                ("committed", T),
                ("error", T),
            ]),
            offsets
                .items
                .iter()
                .map(|o| {
                    vec![
                        json!(o.group),
                        json!(o.topic),
                        json!(o.partition),
                        json!(o.committed),
                        error(&o.error),
                    ]
                })
                .collect(),
            offsets.truncated || offsets.items.iter().any(|o| o.error.is_some()),
        ),
        Response::Configs(entries) => (
            columns(&[
                ("name", T),
                ("value", T),
                ("source", T),
                ("sensitive", B),
                ("error", T),
            ]),
            entries
                .items
                .iter()
                .map(|e| {
                    vec![
                        json!(e.name),
                        if e.sensitive {
                            json!("[redacted]")
                        } else {
                            json!(e.value)
                        },
                        json!(e.source),
                        json!(e.sensitive),
                        error(&e.error),
                    ]
                })
                .collect(),
            entries.truncated || entries.items.iter().any(|e| e.error.is_some()),
        ),
        Response::Produced(delivery) => {
            let confirmed = delivery.status == DeliveryStatus::Confirmed;
            let mut result = grid(
                columns(&[
                    ("topic", T),
                    ("partition", N),
                    ("offset", T),
                    ("status", T),
                    ("error", T),
                ]),
                vec![vec![
                    json!(delivery.topic),
                    json!(delivery.partition),
                    json!(delivery.offset),
                    json!(delivery.status),
                    error(&delivery.error),
                ]],
                format,
                response,
                !confirmed,
            );
            if matches!(
                delivery.status,
                DeliveryStatus::Failed | DeliveryStatus::NotQueued
            ) {
                result.outcome = "ERROR".into();
            }
            return result;
        }
        Response::Error(error) => {
            return Rendered {
                text: if format == Format::Json {
                    serde_json::to_string_pretty(response).unwrap_or_default()
                } else {
                    let mut text = format!("{}: {}", error.code, error.message);
                    if error.possibly_written == Some(true) {
                        text.push_str("\nDelivery unknown: the record may have been written.");
                    }
                    text
                },
                language: if format == Format::Json {
                    "json"
                } else {
                    "text"
                }
                .into(),
                outcome: "ERROR".into(),
                columns: Vec::new(),
            };
        }
        Response::Changed(changes) => {
            let done = match operation {
                Operation::CreateTopic { .. } => "created",
                Operation::DeleteTopic { .. } | Operation::DeleteGroup { .. } => "deleted",
                Operation::CreatePartitions { .. } | Operation::AlterConfigs { .. } => "altered",
                Operation::DeleteRecords { .. } => "purged",
                _ => "committed",
            };
            let failed = changes.items.iter().filter(|c| c.error.is_some()).count();
            let mut result = grid(
                columns(&[
                    ("resource", T),
                    ("partition", N),
                    ("offset", T),
                    ("value", T),
                    ("status", T),
                    ("error", T),
                ]),
                changes
                    .items
                    .iter()
                    .map(|c| {
                        vec![
                            json!(c.resource),
                            json!(c.partition),
                            json!(c.offset),
                            json!(c.value),
                            json!(if c.error.is_some() { "failed" } else { done }),
                            error(&c.error),
                        ]
                    })
                    .collect(),
                format,
                response,
                changes.truncated || failed > 0,
            );
            if failed > 0 && failed == changes.items.len() {
                result.outcome = "ERROR".into();
            }
            return result;
        }
        Response::Fetched(_) => return Rendered::default(),
    };
    grid(cols, rows, format, response, warning)
}

fn bytes(bytes: &Option<kafka_contract::Bytes>) -> Value {
    bytes.as_ref().map_or(Value::Null, |bytes| {
        let raw = bytes.decode();
        match String::from_utf8(raw) {
            Ok(text) => serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text)),
            Err(_) => json!({"base64": bytes}),
        }
    })
}

pub fn records(records: &[Record], format: Format, summary: Option<&FetchSummary>) -> Rendered {
    use Kind::{Json as J, Number as N, Text as T};
    let cols = columns(&[
        ("topic", T),
        ("partition", N),
        ("offset", T),
        ("timestampMs", N),
        ("timestampType", T),
        ("key", J),
        ("headers", J),
        ("value", J),
    ]);
    let rows = records
        .iter()
        .map(|r| {
            vec![
                json!(r.topic),
                json!(r.partition),
                json!(r.offset),
                json!(r.timestamp_ms),
                json!(r.timestamp_type),
                bytes(&r.key),
                json!(
                    r.headers
                        .iter()
                        .map(|h| json!({"name": h.name, "value": bytes(&h.value)}))
                        .collect::<Vec<_>>()
                ),
                bytes(&r.value),
            ]
        })
        .collect();
    let mut result = grid(
        cols,
        rows,
        format,
        &json!({"records": records, "summary": summary}),
        summary.is_some_and(|s| {
            matches!(
                s.end_reason,
                kafka_contract::EndReason::ByteLimit | kafka_contract::EndReason::Timeout
            )
        }),
    );
    if format != Format::Json
        && let Some(summary) = summary
    {
        result.text.push_str(&format!(
            "\nScanned: {}; shown: {}; end: {:?}\nNext offsets: {}",
            summary.scanned,
            records.len(),
            summary.end_reason,
            serde_json::to_string(&summary.next_offsets).unwrap_or_default()
        ));
    }
    result
}

pub fn lag(rows: &[LagRow], format: Format) -> Rendered {
    use Kind::{Number as N, Text as T};
    grid(
        columns(&[
            ("group", T),
            ("topic", T),
            ("partition", N),
            ("committed", T),
            ("end", T),
            ("lag", N),
            ("error", T),
        ]),
        rows.iter()
            .map(|r| {
                vec![
                    json!(r.group),
                    json!(r.topic),
                    json!(r.partition),
                    json!(r.committed),
                    json!(r.end),
                    json!(r.lag),
                    json!(r.error),
                ]
            })
            .collect(),
        format,
        &rows,
        rows.iter().any(|r| r.error.is_some() || r.lag.is_none()),
    )
}

/// The records a `Transform` sees: one object per record, keys, values and headers decoded as
/// the table shows them.
pub fn record_data(records: &[Record]) -> Value {
    records
        .iter()
        .map(|r| {
            json!({
                "topic": r.topic,
                "partition": r.partition,
                "offset": r.offset,
                "timestampMs": r.timestamp_ms,
                "timestampType": r.timestamp_type,
                "key": bytes(&r.key),
                "headers": r.headers
                    .iter()
                    .map(|h| json!({"name": h.name, "value": bytes(&h.value)}))
                    .collect::<Vec<_>>(),
                "value": bytes(&r.value),
            })
        })
        .collect()
}

/// The data of an answer a `Transform` sees: the items of a list, else the answer itself.
pub fn response_data(response: &Response) -> Value {
    let mut value = serde_json::to_value(response).unwrap_or_default();
    let data = value["data"].take();
    match data {
        Value::Object(mut batch) if batch.contains_key("truncated") => {
            batch.remove("items").unwrap_or_default()
        }
        other => other,
    }
}

/// What a `Transform` returned: an array of objects as a grid, text as text, else JSON.
pub fn shaped(data: Value, format: Format) -> Rendered {
    if format != Format::Json
        && let Some((names, cells)) = table::grid(&data)
    {
        let columns = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let kind = cells
                    .iter()
                    .filter_map(|row| row.get(i))
                    .find(|cell| !cell.is_null())
                    .map_or(Kind::Text, |cell| match cell {
                        Value::Number(_) => Kind::Number,
                        Value::Bool(_) => Kind::Boolean,
                        Value::String(_) => Kind::Text,
                        _ => Kind::Json,
                    });
                Column::new(name, kind)
            })
            .collect();
        return grid(columns, cells, format, &data, false);
    }
    match data {
        Value::String(text) => Rendered {
            text,
            language: "text".into(),
            outcome: "OK".into(),
            columns: Vec::new(),
        },
        other => Rendered {
            text: serde_json::to_string_pretty(&other).unwrap_or_default(),
            language: "json".into(),
            outcome: "OK".into(),
            columns: Vec::new(),
        },
    }
}
