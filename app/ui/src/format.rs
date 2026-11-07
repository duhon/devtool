use serde_json::Value;
pub const MAX_RESPONSE_CHARS: usize = 300_000;
pub fn display(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}
/// A finished run's text: what its module rendered, cut to what the editor can show.
pub fn console_response(result: &Value) -> String {
    if result["cancelled"] == true {
        let warning = result["warning"]
            .as_str()
            .unwrap_or("Request stopped before dispatch.");
        // A run stops with what it showed so far.
        return match result["body"].as_str() {
            Some(body) => stream_body(&serde_json::json!({"warning": warning}), body, ""),
            None => warning.into(),
        };
    }
    truncated(result["body"].as_str().unwrap_or("").trim_end_matches('\n'))
}
fn truncated(text: &str) -> String {
    let count = text.chars().count();
    if count > MAX_RESPONSE_CHARS {
        format!(
            "{}\n\n… response truncated ({} more characters omitted) — too large to display inline without freezing the editor.",
            text.chars().take(MAX_RESPONSE_CHARS).collect::<String>(),
            count - MAX_RESPONSE_CHARS
        )
    } else {
        text.into()
    }
}
/// A check's response: the run's result, its error, or (still running) its output so far.
pub fn check_response(result: &Value, stdout: &str, stderr: &str) -> String {
    if result.is_null() {
        stream_body(result, stdout, stderr)
    } else if result.get("error").is_some() {
        // A failed streamed run keeps the output it showed before its error.
        let error = result["error"]
            .as_str()
            .or(result["message"].as_str())
            .unwrap_or("Unknown request error");
        format!("{stdout}{stderr}{error}")
    } else {
        console_response(result)
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
/// A saved run as History shows it: its source, then `#responce <time> · <outcome>` and the
/// rendered body it saved, which is its response region.
pub fn history_document(
    snapshot: &str,
    time: &str,
) -> (String, Vec<crate::document::ResponseRegion>) {
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
        return (snapshot.into(), vec![]);
    };
    let request = snapshot[..offset].trim_end_matches('\n');
    let body = &snapshot[offset + marker.len()..];
    let text = format!("{request}\n#responce {time} · {outcome}\n{body}");
    let region = crate::document::ResponseRegion {
        request: request
            .lines()
            .position(crate::console::is_check)
            .unwrap_or(0),
        header: request.lines().count(),
        end: text.split('\n').count(),
        columns: Vec::new(),
    };
    (text, vec![region])
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
pub fn status(result: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(ms) = result["durationMs"].as_f64() {
        parts.push(duration(ms));
    }
    parts.push(outcome(result).into());
    if let Some(summary) = result["summary"].as_str() {
        parts.push(summary.into());
    }
    parts.join(" · ")
}
