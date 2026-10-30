use serde_json::Value;
use services_contracts::manifest::View;
pub const MAX_RESPONSE_CHARS: usize = 300_000;
pub fn display(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}
pub fn table(result: &Value, vertical: bool) -> String {
    crate::tables::render(result, vertical)
}
pub fn console_response(service: &str, result: &Value, vertical: bool) -> String {
    if result["cancelled"] == true {
        return result["warning"]
            .as_str()
            .unwrap_or("Request stopped before dispatch.")
            .into();
    }
    let view = crate::services::view(service);
    if view == View::Grid {
        if result["preview"] == true {
            return format!(
                "{}\n\n{}",
                services_contracts::SPLUNK_PREVIEW_WARNING,
                table(result, vertical)
            );
        }
        return table(result, vertical);
    }
    let raw = result["body"].as_str().unwrap_or("").trim_end_matches('\n');
    let pretty = if raw.chars().count() <= MAX_RESPONSE_CHARS {
        pretty_body(view, raw)
    } else {
        raw.into()
    };
    let count = pretty.chars().count();
    if count > MAX_RESPONSE_CHARS {
        format!(
            "{}\n\n… response truncated ({} more characters omitted) — too large to display inline without freezing the editor.",
            pretty.chars().take(MAX_RESPONSE_CHARS).collect::<String>(),
            count - MAX_RESPONSE_CHARS
        )
    } else {
        pretty
    }
}
fn pretty_body(view: View, raw: &str) -> String {
    if view == View::BodyHeaders {
        serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_else(|| raw.into())
    } else {
        raw.into()
    }
}
pub fn check_response(
    service: &str,
    result: &Value,
    vertical: bool,
    stdout: &str,
    stderr: &str,
) -> String {
    if crate::services::view(service) == View::Stream {
        stream_body(result, stdout, stderr)
    } else if result.get("error").is_some()
        && crate::services::get(service).is_none_or(|service| !service.console.error_body)
    {
        result["error"]
            .as_str()
            .or(result["message"].as_str())
            .unwrap_or("Unknown request error")
            .into()
    } else {
        console_response(service, result, vertical)
    }
}
/// A streamed response: its output, then the error or a warning.
fn stream_body(result: &Value, stdout: &str, stderr: &str) -> String {
    let mut body = format!("{stdout}{stderr}{}", result["error"].as_str().unwrap_or(""));
    if let Some(warning) = result["warning"].as_str() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("Warning: ");
        body.push_str(warning);
    }
    body
}
fn stream_history_body(raw: &str, complete: bool) -> Result<String, String> {
    let mut stdout = String::new();
    let mut stderr = String::new();
    let result_text = if let Some(output) = raw.strip_prefix("#terminal-output\n") {
        let (events, result) = match output.split_once("\n\n") {
            Some((events, result)) => (events, Some(result)),
            None if !complete => (output, None),
            None => return Err("Saved Terminal output has no completion result".into()),
        };
        for line in events.split_inclusive('\n') {
            if !line.ends_with('\n') && result.is_none() && !complete {
                break;
            }
            let event: Value = serde_json::from_str(line)
                .map_err(|error| format!("Invalid saved Terminal output: {error}"))?;
            let data = event["data"]
                .as_str()
                .ok_or("Saved Terminal output has no text")?;
            match event["type"].as_str() {
                Some("stdout") => stdout.push_str(data),
                Some("stderr") => stderr.push_str(data),
                _ => return Err("Saved Terminal output has an unknown stream".into()),
            }
        }
        result
    } else {
        Some(raw)
    };
    let result = if complete {
        serde_json::from_str(result_text.ok_or("Saved Terminal result is missing")?)
            .map_err(|error| format!("Invalid saved Terminal result: {error}"))?
    } else {
        Value::Null
    };
    Ok(stream_body(&result, &stdout, &stderr))
}
pub fn history_snapshot(
    service: &str,
    snapshot: &str,
    time: &str,
    complete: bool,
) -> Result<String, String> {
    history_document(service, snapshot, time, complete).map(|(text, _)| text)
}
pub fn history_document(
    service: &str,
    snapshot: &str,
    time: &str,
    complete: bool,
) -> Result<(String, Vec<crate::document::ResponseRegion>), String> {
    let headers = [
        ("\n\n#response success\n", "OK"),
        ("\n\n#response error\n", "ERROR"),
        ("\n\n#response stopped\n", "Stopped"),
        ("\n\n#responce OK\n", "OK"),
        ("\n\n#responce WARNING\n", "WARNING"),
        ("\n\n#responce ERROR\n", "ERROR"),
        ("\n\n#responce NOTICE\n", "NOTICE"),
        ("\n\n#responce Stopped\n", "Stopped"),
    ];
    let Some((offset, marker, outcome)) = headers
        .into_iter()
        .filter_map(|(marker, outcome)| {
            snapshot
                .find(marker)
                .map(|offset| (offset, marker, outcome))
        })
        .min_by_key(|(offset, _, _)| *offset)
    else {
        return Ok((snapshot.into(), vec![]));
    };
    let request = snapshot[..offset].trim_end_matches('\n');
    let raw = &snapshot[offset + marker.len()..];
    let mut table = None;
    let view = crate::services::view(service);
    let body = if view == View::Stream {
        stream_history_body(raw, complete)?
    } else if view == View::Grid {
        if complete {
            let result: Value = serde_json::from_str(raw)
                .map_err(|error| format!("Invalid saved {service} result: {error}"))?;
            if result["columns"].is_array() {
                table = Some(crate::tables::Metadata::from_result(&result));
            }
            let vertical = crate::console::parse(service, request)
                .is_ok_and(|params| params["vertical"] == true);
            check_response(service, &result, vertical, "", "")
        } else {
            String::new()
        }
    } else if complete && raw.chars().count() <= MAX_RESPONSE_CHARS {
        pretty_body(view, raw.trim_end_matches('\n'))
    } else {
        raw.into()
    };
    let text = format!("{request}\n#responce {time} · {outcome}\n{body}");
    let regions = table
        .map(|table| crate::document::ResponseRegion {
            request: request
                .lines()
                .position(crate::console::is_check)
                .unwrap_or(0),
            header: request.lines().count(),
            end: text.split('\n').count(),
            table: Some(table),
        })
        .into_iter()
        .collect();
    Ok((text, regions))
}
pub fn duration(ms: f64) -> String {
    if ms < 1000.0 {
        format!("{ms:.0}ms")
    } else {
        let (value, unit) = if ms < 60000.0 {
            (ms / 1000.0, "s")
        } else {
            (ms / 60000.0, "min")
        };
        format!(
            "{}{unit}",
            format!("{value:.1}")
                .trim_end_matches('0')
                .trim_end_matches('.')
        )
    }
}
/// The verdict on a run: the module's own `outcome` (the host adds it to every answer), else
/// one read from a generic answer.
pub fn outcome(result: &Value) -> &str {
    if result["cancelled"] == true {
        return "Stopped";
    }
    if result.get("error").is_some() {
        return "ERROR";
    }
    if let Some(outcome) = result["outcome"].as_str() {
        return outcome;
    }
    match result["status"].as_u64() {
        Some(200..=299) => "OK",
        Some(400..=499) => "WARNING",
        Some(500..) => "ERROR",
        _ => "NOTICE",
    }
}
pub fn status(service: &str, result: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(ms) = result["durationMs"].as_f64() {
        parts.push(duration(ms));
    }
    parts.push(outcome(result).into());
    if crate::services::view(service) == View::Grid {
        let n = result["resultCount"]
            .as_u64()
            .or_else(|| result["rows"].as_array().map(|a| a.len() as u64));
        if let Some(n) = n {
            parts.push(format!("{n} {}", if n == 1 { "row" } else { "rows" }));
        }
    }
    parts.join(" · ")
}
