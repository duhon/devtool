//! Prometheus response envelopes and lossless sample tables.
use crate::{
    chart,
    request::{Format, Plan},
};
use serde_json::{Value, json};
use services_sdk::{
    Rendered,
    http::Response,
    lang::table::{self, Column, Kind},
    run::Answer,
};

fn notices(envelope: &Value) -> Result<Vec<String>, String> {
    let mut notices = vec![];
    for (key, label) in [("warnings", "Warning"), ("infos", "Info")] {
        if let Some(values) = envelope.get(key) {
            for value in values
                .as_array()
                .ok_or_else(|| format!("Invalid {key} array"))?
            {
                notices.push(format!(
                    "{label}: {}",
                    value
                        .as_str()
                        .ok_or_else(|| format!("Invalid {key} message"))?
                ));
            }
        }
    }
    Ok(notices)
}

fn pair(value: &Value, histogram: bool) -> Result<(Value, Value), String> {
    let sample = value
        .as_array()
        .filter(|s| s.len() == 2)
        .ok_or("Invalid Prometheus sample; expected [timestamp, value]")?;
    sample[0]
        .as_f64()
        .filter(|s| s.is_finite())
        .ok_or("Invalid Prometheus sample timestamp")?;
    // Epoch seconds stay numeric: no timestamp precision is lost by datetime formatting.
    if (histogram && !sample[1].is_object()) || (!histogram && !sample[1].is_string()) {
        return Err("Invalid Prometheus sample value".into());
    }
    Ok((sample[0].clone(), sample[1].clone()))
}

pub fn rows(envelope: &Value) -> Result<(Vec<Column>, Vec<Vec<Value>>), String> {
    let data = envelope.get("data").ok_or("Missing Prometheus data")?;
    let result = data.get("result").ok_or("Missing Prometheus result")?;
    let result_type = data["resultType"]
        .as_str()
        .ok_or("Missing Prometheus resultType")?;
    let mut rows = vec![];
    let mut histogram_seen = false;
    match result_type {
        "vector" | "matrix" => {
            for series in result
                .as_array()
                .ok_or("Prometheus result must be an array")?
            {
                let labels = series["metric"]
                    .as_object()
                    .ok_or("Missing Prometheus metric labels")?;
                if labels.values().any(|v| !v.is_string()) {
                    return Err("Prometheus metric label values must be strings".into());
                }
                let mut count = 0;
                for (key, histogram) in if result_type == "vector" {
                    [("value", false), ("histogram", true)]
                } else {
                    [("values", false), ("histograms", true)]
                } {
                    let Some(samples) = series.get(key) else {
                        continue;
                    };
                    let samples = if result_type == "vector" {
                        vec![samples]
                    } else {
                        samples
                            .as_array()
                            .ok_or("Invalid Prometheus sample array")?
                            .iter()
                            .collect()
                    };
                    for sample in samples {
                        let (timestamp, value) = pair(sample, histogram)?;
                        histogram_seen |= histogram;
                        rows.push(vec![
                            timestamp,
                            Value::Object(labels.clone()),
                            if histogram {
                                Value::Null
                            } else {
                                value.clone()
                            },
                            if histogram { value } else { Value::Null },
                        ]);
                        count += 1;
                    }
                }
                if result_type == "vector" && count != 1 {
                    return Err("A Prometheus vector entry must contain exactly one sample".into());
                }
                if result_type == "matrix"
                    && series.get("values").is_none()
                    && series.get("histograms").is_none()
                {
                    return Err("A Prometheus matrix entry needs values or histograms".into());
                }
            }
        }
        "scalar" | "string" => {
            let (timestamp, value) = pair(result, false)?;
            rows.push(vec![timestamp, json!({}), value, Value::Null]);
        }
        _ => return Err(format!("Unsupported Prometheus resultType: {result_type}")),
    }
    let mut columns = vec![
        Column::new("timestamp", Kind::Datetime),
        Column::new("labels", Kind::Json),
        Column::new(
            "value",
            if result_type == "string" {
                Kind::Text
            } else {
                Kind::Number
            },
        ),
    ];
    if histogram_seen {
        columns.push(Column::new("histogram", Kind::Json));
    } else {
        for row in &mut rows {
            row.pop();
        }
    }
    Ok((columns, rows))
}

pub fn answer(plan: &Plan, value: &Value) -> Result<Answer, String> {
    answer_with_chart(plan, value, &mut chart::State::default())
}

pub fn answer_with_chart(
    plan: &Plan,
    value: &Value,
    state: &mut chart::State,
) -> Result<Answer, String> {
    let response = Response::parse(value)?;
    let body = response.body.as_deref().ok_or_else(|| {
        format!(
            "HTTP {}: Prometheus response has no text body",
            response.status
        )
    })?;
    let envelope: Value = serde_json::from_str(body)
        .map_err(|e| format!("HTTP {}: invalid Prometheus JSON: {e}", response.status))?;
    let notices = notices(&envelope)?;
    let status = envelope["status"]
        .as_str()
        .ok_or_else(|| format!("HTTP {}: missing Prometheus status", response.status))?;
    if !(200..300).contains(&response.status) || status != "success" {
        let mut error = format!("HTTP {}: Prometheus {status}", response.status);
        if let Some(kind) = envelope["errorType"].as_str() {
            error.push_str(&format!(" ({kind})"));
        }
        if let Some(message) = envelope["error"].as_str() {
            error.push_str(&format!(": {message}"));
        }
        if !notices.is_empty() {
            error.push_str(&format!("\n{}", notices.join("\n")));
        }
        return Err(error);
    }
    let (mut columns, rows) = rows(&envelope)?;
    plan.columns.apply(&mut columns)?;
    let text = if matches!(plan.format, Format::Json) {
        serde_json::to_string_pretty(&envelope).expect("JSON values serialize")
    } else {
        let table = if matches!(plan.format, Format::Chart) {
            if columns[2].kind == Kind::Text {
                return Err(
                    "Charts need numeric samples; use table or JSON for string results".into(),
                );
            }
            chart::render(
                &plan.chart,
                plan.interval,
                plan.step,
                &plan.chart_identity,
                &rows,
                state,
            )?
        } else {
            table::render(&columns, &rows, false)
        };
        if notices.is_empty() {
            table
        } else {
            format!("{}\n\n{table}", notices.join("\n"))
        }
    };
    Ok(Answer {
        rendered: Rendered {
            text,
            language: if matches!(plan.format, Format::Json) {
                "json"
            } else if matches!(plan.format, Format::Chart) {
                "text"
            } else {
                "prometheus"
            }
            .into(),
            outcome: if notices.is_empty() { "OK" } else { "WARNING" }.into(),
            columns: if !matches!(plan.format, Format::Table) {
                vec![]
            } else {
                table::types(&columns)
            },
        },
        status: Some(format!(
            "{} samples{}",
            rows.len(),
            if notices.is_empty() {
                ""
            } else {
                "; server notices"
            }
        )),
    })
}
