use super::http;
use super::splunk_rate_limit::{API_TIMEOUT, INTERVAL, RateLimit, remaining};
use crate::core::error::{Error, Result, required, string, text};
use serde_json::{Value, json};
use services_contracts::native_messaging::MAX_RESPONSE_BYTES;
use std::{
    path::Path,
    time::{Duration, Instant},
};

pub fn normalize_search(search: &str) -> String {
    let search = search.trim();
    let generating = search
        .get(..6)
        .is_some_and(|s| s.eq_ignore_ascii_case("search"))
        && search[6..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
    if search.starts_with('|') || generating {
        search.into()
    } else {
        format!("search {search}")
    }
}

fn row_limit(params: &Value) -> Result<Option<u64>> {
    let count = unsigned_option(params, "count", "Count", 100)?;
    Ok((count != 0).then_some(count))
}

fn unsigned_option(params: &Value, key: &str, label: &str, default: u64) -> Result<u64> {
    let invalid = || Error::new(format!("Splunk {label} must be a non-negative integer"));
    Ok(match params.get(key).filter(|v| !v.is_null()) {
        None => default,
        Some(Value::String(s)) if s.trim().is_empty() => default,
        Some(Value::String(s)) => s.trim().parse::<u64>().map_err(|_| invalid())?,
        Some(v) => v
            .as_u64()
            .or_else(|| {
                v.as_f64()
                    .filter(|n| *n >= 0. && *n < u64::MAX as f64 && n.fract() == 0.)
                    .map(|n| n as u64)
            })
            .ok_or_else(invalid)?,
    })
}

fn preview_option(params: &Value) -> Result<bool> {
    match params.get("preview") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(Value::String(value)) if value.trim().is_empty() => Ok(false),
        Some(Value::String(value)) if value.trim().eq_ignore_ascii_case("true") => Ok(true),
        Some(Value::String(value)) if value.trim().eq_ignore_ascii_case("false") => Ok(false),
        _ => Err(Error::new("Splunk Preview must be true or false")),
    }
}

pub async fn send(
    client: &reqwest::Client,
    params: &Value,
    rate_limit_directory: &Path,
) -> Result<Value> {
    let started = Instant::now();
    let base = required(params, "baseUrl")?;
    let search = required(params, "search")?;
    let limit = row_limit(params)?;
    let offset = unsigned_option(params, "offset", "Offset", 0)?;
    let preview = preview_option(params)?;
    let app = text(params, "app").trim().to_string();
    let base = url::Url::parse(&format!("{}/", base.trim_end_matches('/')))
        .map_err(|_| Error::new(format!("Invalid baseUrl: {base}")))?;
    if !matches!(base.scheme(), "http" | "https") {
        return Err(Error::new("Splunk endpoint must use HTTP or HTTPS"));
    }
    let path = if app.is_empty() {
        "services/search/v2/jobs/export".to_string()
    } else {
        format!(
            "servicesNS/-/{}/search/v2/jobs/export",
            url::form_urlencoded::byte_serialize(app.as_bytes())
                .collect::<String>()
                .replace('+', "%20")
        )
    };
    let legacy = base
        .join(&path.replace("search/v2/jobs/", "search/jobs/"))
        .map_err(|e| Error::new(e.to_string()))?;
    let mut url = base.join(&path).map_err(|e| Error::new(e.to_string()))?;
    let mut headers = json!({"Content-Type":"application/x-www-form-urlencoded"});
    headers
        .as_object_mut()
        .unwrap()
        .extend(http::headers(params)?.as_object().unwrap().clone());
    let body = {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("search", &normalize_search(&search))
            .append_pair("output_mode", "json")
            .append_pair("preview", if preview { "true" } else { "false" })
            .append_pair("segmentation", "none");
        for (param, key) in [
            ("earliestTime", "earliest_time"),
            ("latestTime", "latest_time"),
        ] {
            if !text(params, param).is_empty() {
                form.append_pair(key, &text(params, param));
            }
        }
        form.finish()
    };
    let mut debug = json!({
        "request":{"method":"POST","url":url.as_str(),"headers":headers},
        "export":{"rowLimit":limit,"offset":offset,"preview":preview,"rateLimitedRetries":0,"legacyFallbacks":0},
        "connection":{"events":[]}
    });
    let deadline = started + Duration::from_secs(services_contracts::SPLUNK_TIMEOUT_SECONDS);
    let limiter = RateLimit::new(rate_limit_directory, &base.origin().ascii_serialization());
    let result = async {
        loop {
            let mut permit = limiter.acquire(deadline).await?;
            permit.begin(remaining(deadline)?)?;
            debug["request"]["url"] = json!(url.as_str());
            let mut builder = client.post(url.clone()).body(body.clone());
            for (name, value) in headers.as_object().unwrap() {
                builder = builder.header(name, string(value));
            }
            let sent = tokio::time::timeout(
                remaining(deadline)?.min(API_TIMEOUT),
                builder.send(),
            )
            .await
            .map_err(|_| Error::new("Splunk export timed out waiting for response headers"))
            .and_then(|result| result.map_err(Error::from));
            let mut response = match sent {
                Ok(response) => response,
                Err(error) => {
                    permit.finish(INTERVAL)?;
                    return Err(error);
                }
            };
            let status = response.status();
            let response_headers: serde_json::Map<String, Value> = response
                .headers()
                .iter()
                .map(|(key, value)| {
                    (key.to_string(), json!(value.to_str().unwrap_or("")))
                })
                .collect();
            debug["response"] = json!({
                "status":status.as_u16(),"statusText":status.canonical_reason().unwrap_or(""),
                "headers":response_headers
            });
            debug["connection"]["timing"]["ttfbMs"] = json!(started.elapsed().as_millis());
            if let Some(remote) = response.remote_addr() {
                debug["connection"]["remoteAddress"] = json!(remote.ip().to_string());
                debug["connection"]["remotePort"] = json!(remote.port());
                debug["connection"]["remoteFamily"] =
                    json!(if remote.is_ipv4() { "IPv4" } else { "IPv6" });
            }
            if url.scheme() == "https" {
                debug["connection"]["tls"] = json!({"encrypted":true});
            }
            event(&mut debug, started, "responseHeaders", json!({
                "httpVersion":format!("{:?}",response.version()),"status":status.as_u16(),"url":url.as_str()
            }));
            if status == reqwest::StatusCode::NOT_FOUND && url != legacy {
                drop(response);
                permit.finish(INTERVAL)?;
                drop(permit);
                event(&mut debug, started, "responseBody", json!({"byteLength":0,"closedEarly":true}));
                debug["export"]["legacyFallbacks"] = json!(1);
                url = legacy.clone();
                continue;
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let delay = debug["response"]["headers"]["retry-after"]
                    .as_str()
                    .and_then(retry_after)
                    .unwrap_or(Duration::from_secs(60))
                    .max(INTERVAL);
                // Persist Retry-After before any further await, including cancellation.
                permit.finish(delay)?;
                drop(permit);
                drop(response);
                event(&mut debug, started, "responseBody", json!({"byteLength":0,"closedEarly":true}));
                if delay >= deadline.saturating_duration_since(Instant::now()) {
                    return Err(Error::new("Splunk returned HTTP 429; Retry-After exceeds the remaining search timeout"));
                }
                debug["export"]["rateLimitedRetries"] =
                    json!(debug["export"]["rateLimitedRetries"].as_u64().unwrap_or(0) + 1);
                continue;
            }
            let mut stream = Export::new(limit, offset, preview);
            let mut bytes = 0usize;
            let read = async {
                if status.is_redirection() {
                    return Err(Error::new(
                        "Splunk redirected the API call. Use the direct API endpoint; automatic redirects are disabled to preserve the server's API limit.",
                    ));
                }
                if !status.is_success() {
                    let mut body = Vec::new();
                    while let Some(chunk) = next_chunk(&mut response, deadline).await? {
                        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                            return Err(Error::new(format!("Splunk returned HTTP {status} with an oversized error body")));
                        }
                        bytes += chunk.len();
                        body.extend_from_slice(&chunk);
                    }
                    let value = serde_json::from_slice::<Value>(&body).ok();
                    let messages = value.as_ref().map(summary).unwrap_or_default();
                    return Err(Error::new(if messages.is_empty() {
                        format!("Splunk returned HTTP {status}: {}", String::from_utf8_lossy(&body).chars().take(200).collect::<String>())
                    } else { messages }));
                }
                while let Some(chunk) = next_chunk(&mut response, deadline).await? {
                    bytes += chunk.len();
                    if stream.push(&chunk)? {
                        return Ok(());
                    }
                }
                stream.finish()
            }.await;
            drop(response);
            permit.finish(INTERVAL)?;
            drop(permit);
            debug["export"]["bytesRead"] = json!(bytes);
            debug["export"]["records"] = json!(stream.records);
            debug["export"]["previewRecords"] = json!(stream.previews);
            debug["export"]["ignoredPreviewRecords"] = json!(if preview { 0 } else { stream.previews });
            debug["export"]["rowsSkipped"] = json!(offset - stream.skip);
            debug["export"]["selectedPreview"] = json!(stream.preview == Some(true));
            debug["export"]["stoppedAfterCount"] = json!(stream.limited);
            debug["export"]["stoppedAfterPreview"] = json!(stream.preview_complete);
            debug["export"]["streamComplete"] = json!(stream.complete);
            debug["response"]["messages"] = json!(stream.messages);
            event(&mut debug, started, "responseBody", json!({"byteLength":bytes,"closedEarly":!stream.complete}));
            read?;
            let mut result = json!({
                "columns":stream.columns,"rows":stream.rows,"resultCount":stream.rows.len(),
                "preview":stream.preview == Some(true),"offset":offset,
                "durationMs":started.elapsed().as_millis()
            });
            if stream.preview == Some(true) {
                result["warning"] = json!(services_contracts::SPLUNK_PREVIEW_WARNING);
            }
            return Ok(result);
        }
    }.await;
    match result {
        Ok(mut result) => {
            result["debug"] = debug;
            Ok(result)
        }
        Err(mut error) => {
            if let Some(connection) = error.debug.as_ref().and_then(|d| d.get("connection")) {
                debug["connection"]["error"] = connection["error"].clone();
            }
            debug["error"] = json!({"name":"Error","message":error.message});
            error.debug = Some(Box::new(debug));
            error.duration_ms = Some(started.elapsed().as_millis());
            Err(error)
        }
    }
}

async fn next_chunk(
    response: &mut reqwest::Response,
    deadline: Instant,
) -> Result<Option<bytes::Bytes>> {
    tokio::time::timeout(remaining(deadline)?.min(API_TIMEOUT), response.chunk())
        .await
        .map_err(|_| Error::new("Splunk export timed out waiting for stream data"))?
        .map_err(Error::from)
}

fn event(debug: &mut Value, started: Instant, name: &str, data: Value) {
    debug["connection"]["events"]
        .as_array_mut()
        .unwrap()
        .push(json!({"event":name,"atMs":started.elapsed().as_millis(),"data":data}));
}

fn summary(value: &Value) -> String {
    value["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["text"].as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

fn retry_after(value: &str) -> Option<Duration> {
    value
        .parse::<f64>()
        .ok()
        .filter(|s| s.is_finite())
        .and_then(|s| Duration::try_from_secs_f64(s.max(0.)).ok())
        .or_else(|| {
            httpdate::parse_http_date(value).ok().map(|date| {
                date.duration_since(std::time::SystemTime::now())
                    .unwrap_or_default()
            })
        })
}

struct Export {
    limit: Option<u64>,
    offset: u64,
    skip: u64,
    allow_preview: bool,
    preview: Option<bool>,
    rows_seen: u64,
    batch_ended: bool,
    preview_complete: bool,
    pending: Vec<u8>,
    columns: Vec<String>,
    rows: Vec<Value>,
    messages: Vec<Value>,
    retained: usize,
    message_bytes: usize,
    records: usize,
    previews: usize,
    final_seen: bool,
    limited: bool,
    complete: bool,
}

impl Export {
    fn new(limit: Option<u64>, offset: u64, allow_preview: bool) -> Self {
        Self {
            limit,
            offset,
            skip: offset,
            allow_preview,
            preview: None,
            rows_seen: 0,
            batch_ended: false,
            preview_complete: false,
            pending: vec![],
            columns: vec![],
            rows: vec![],
            messages: vec![],
            retained: 0,
            message_bytes: 0,
            records: 0,
            previews: 0,
            final_seen: false,
            limited: false,
            complete: false,
        }
    }

    fn push(&mut self, data: &[u8]) -> Result<bool> {
        for part in data.split_inclusive(|byte| *byte == b'\n') {
            if self.pending.len().saturating_add(part.len()) > MAX_RESPONSE_BYTES {
                return Err(Error::new(
                    "Splunk export JSON record exceeds the 64 MiB response limit",
                ));
            }
            self.pending.extend_from_slice(part);
            if part.ends_with(b"\n") {
                let record = std::mem::take(&mut self.pending);
                self.record(&record)?;
                if self.limited || self.preview_complete {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn retain(&mut self, value: &Value) -> Result<()> {
        self.retained = self
            .retained
            .saturating_add(serde_json::to_vec(value)?.len());
        if self.retained > MAX_RESPONSE_BYTES {
            return Err(Error::new(
                "Splunk export results exceed the 64 MiB response limit; reduce Count or narrow the search",
            ));
        }
        Ok(())
    }

    fn column(&mut self, name: &str) -> Result<()> {
        if !self.columns.iter().any(|column| column == name) {
            self.retain(&json!(name))?;
            self.columns.push(name.to_string());
        }
        Ok(())
    }

    fn record(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|e| Error::new(format!("Invalid or truncated Splunk export JSON: {e}")))?;
        if !value.is_object()
            || ![
                "result", "results", "messages", "fields", "preview", "lastrow",
            ]
            .iter()
            .any(|key| value.get(key).is_some())
        {
            return Err(Error::new("Unrecognized Splunk export JSON record"));
        }
        self.records += 1;
        if let Some(messages) = value.get("messages") {
            for message in messages
                .as_array()
                .ok_or_else(|| Error::new("Invalid Splunk export messages"))?
            {
                self.retain(message)?;
                self.message_bytes = self
                    .message_bytes
                    .saturating_add(serde_json::to_vec(message)?.len());
                self.messages.push(message.clone());
                if message["type"].as_str().is_some_and(|kind| {
                    kind.eq_ignore_ascii_case("ERROR") || kind.eq_ignore_ascii_case("FATAL")
                }) {
                    return Err(Error::new(
                        message["text"]
                            .as_str()
                            .unwrap_or("Splunk export search failed"),
                    ));
                }
            }
        }
        if value["preview"] == true {
            self.previews += 1;
            if !self.allow_preview {
                return Ok(());
            }
        }
        if value["preview"] == false {
            self.final_seen = true;
        } else if value.get("preview").is_some() && value["preview"] != true {
            return Err(Error::new("Invalid Splunk export preview flag"));
        }
        let rows = if let Some(row) = value.get("result") {
            std::slice::from_ref(row)
        } else if let Some(rows) = value.get("results") {
            rows.as_array()
                .ok_or_else(|| Error::new("Invalid Splunk export results"))?
                .as_slice()
        } else {
            &[]
        };
        if !rows.is_empty() && !value["preview"].is_boolean() {
            return Err(Error::new(
                "Splunk export results are missing a boolean preview flag",
            ));
        }
        if let Some(preview) = value["preview"].as_bool() {
            if self.allow_preview
                && (self.preview != Some(preview)
                    || self.batch_ended
                    || (!rows.is_empty() && value["offset"] == 0 && self.rows_seen > 0))
            {
                self.rows.clear();
                self.columns.clear();
                self.retained = self.message_bytes;
                self.skip = self.offset;
                self.rows_seen = 0;
                self.batch_ended = false;
            }
            self.preview = Some(preview);
        }
        if let Some(fields) = value.get("fields") {
            for field in fields
                .as_array()
                .ok_or_else(|| Error::new("Invalid Splunk export fields"))?
            {
                let name = field
                    .as_str()
                    .or_else(|| field["name"].as_str())
                    .ok_or_else(|| Error::new("Invalid Splunk export field name"))?;
                self.column(name)?;
            }
        }
        for row in rows {
            let object = row
                .as_object()
                .ok_or_else(|| Error::new("Invalid Splunk export result row"))?;
            self.rows_seen = self.rows_seen.saturating_add(1);
            if self.skip > 0 {
                self.skip -= 1;
                continue;
            }
            self.retain(row)?;
            for key in object.keys() {
                self.column(key)?;
            }
            self.rows.push(row.clone());
            if self
                .limit
                .is_some_and(|limit| self.rows.len() as u64 >= limit)
            {
                self.limited = true;
                break;
            }
        }
        if value["lastrow"] == true {
            self.batch_ended = true;
            self.preview_complete = self.preview == Some(true) && !self.rows.is_empty();
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        let pending = std::mem::take(&mut self.pending);
        self.record(&pending)?;
        if self.records == 0 {
            return Err(Error::new(
                "Splunk export returned an empty stream, not JSON",
            ));
        }
        if self.previews > 0
            && !self.final_seen
            && !(self.allow_preview && (self.limited || self.preview_complete))
        {
            return Err(Error::new(
                "Splunk export ended with preview results only, without final results",
            ));
        }
        self.complete = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_is_not_capped_below_the_servers_required_cooldown() {
        assert_eq!(retry_after("300"), Some(Duration::from_secs(300)));
        assert_eq!(retry_after("0.5"), Some(Duration::from_millis(500)));
        assert_eq!(
            retry_after("Thu, 01 Jan 1970 00:00:00 GMT"),
            Some(Duration::ZERO)
        );
        assert_eq!(retry_after("invalid"), None);
        assert_eq!(retry_after("NaN"), None);
        assert_eq!(retry_after("inf"), None);
    }

    #[test]
    fn count_is_explicit_validated_and_defaults_to_a_small_portion() {
        for value in [json!({}), json!({"count":""}), json!({"count":null})] {
            assert_eq!(row_limit(&value).unwrap(), Some(100));
        }
        assert_eq!(row_limit(&json!({"count":"0"})).unwrap(), None);
        assert_eq!(row_limit(&json!({"count":30})).unwrap(), Some(30));
        assert_eq!(row_limit(&json!({"count":30.0})).unwrap(), Some(30));
        assert_eq!(row_limit(&json!({"count":0.0})).unwrap(), None);
        for value in [json!(-1), json!("abc"), json!(1.5), json!(false), json!([])] {
            assert!(row_limit(&json!({"count":value})).is_err());
        }
    }

    #[test]
    fn preview_and_offset_have_strict_values_and_compatible_defaults() {
        assert!(!preview_option(&json!({})).unwrap());
        assert_eq!(
            unsigned_option(&json!({}), "offset", "Offset", 0).unwrap(),
            0
        );
        for value in [json!(true), json!("true"), json!(" TRUE ")] {
            assert!(preview_option(&json!({"preview":value})).unwrap());
        }
        for value in [json!(false), json!("false"), json!(""), Value::Null] {
            assert!(!preview_option(&json!({"preview":value})).unwrap());
        }
        for value in [json!("yes"), json!(1), json!([])] {
            assert!(preview_option(&json!({"preview":value})).is_err());
        }
        for value in [json!("30"), json!(30), json!(30.0)] {
            assert_eq!(
                unsigned_option(&json!({"offset":value}), "offset", "Offset", 0).unwrap(),
                30
            );
        }
        for value in [json!("-1"), json!(-1), json!(1.5), json!(false)] {
            assert!(unsigned_option(&json!({"offset":value}), "offset", "Offset", 0).is_err());
        }
    }

    #[test]
    fn preview_offset_restarts_for_each_snapshot_and_zero_count_returns_one_snapshot() {
        for limit in [None, Some(30)] {
            let mut stream = Export::new(limit, 2, true);
            assert!(!stream.push(b"{\"preview\":true,\"results\":[{\"old\":0},{\"old\":1}],\"lastrow\":true}\n").unwrap());
            assert!(stream.push(b"{\"preview\":true,\"offset\":0,\"fields\":[\"n\"],\"results\":[{\"n\":0},{\"n\":1},{\"n\":2},{\"n\":3}],\"lastrow\":true}\n").unwrap());
            assert_eq!(stream.rows, [json!({"n":2}), json!({"n":3})]);
            assert_eq!(stream.columns, ["n"]);
            assert_eq!(stream.preview, Some(true));
            assert_eq!(stream.skip, 0);
            assert!(stream.preview_complete && !stream.complete && !stream.limited);
        }
    }

    #[test]
    fn a_single_preview_row_and_separate_completion_markers_return_without_eof() {
        for limit in [None, Some(30)] {
            let mut stream = Export::new(limit, 0, true);
            assert!(
                stream
                    .push(b"{\"preview\":true,\"result\":{\"n\":1},\"lastrow\":true}\n")
                    .unwrap()
            );
            assert_eq!(stream.rows, [json!({"n":1})]);
            assert!(stream.preview_complete && !stream.complete);
            let mut stream = Export::new(limit, 0, true);
            assert!(
                !stream
                    .push(b"{\"preview\":true,\"result\":{\"n\":1}}\n")
                    .unwrap()
            );
            assert!(
                stream
                    .push(b"{\"preview\":true,\"lastrow\":true}\n")
                    .unwrap()
            );
            assert_eq!(stream.rows, [json!({"n":1})]);
        }
        let mut stream = Export::new(None, 1, true);
        assert!(
            !stream
                .push(b"{\"preview\":true,\"result\":{\"old\":1},\"lastrow\":true}\n")
                .unwrap()
        );
        assert!(
            stream
                .push(b"{\"preview\":true,\"results\":[{\"n\":1},{\"n\":2}],\"lastrow\":true}\n")
                .unwrap()
        );
        assert_eq!(stream.rows, [json!({"n":2})]);
    }

    #[test]
    fn preview_count_and_new_snapshot_boundaries_never_mix_rows() {
        for final_batch in [false, true] {
            let mut stream = Export::new(Some(2), 0, true);
            assert!(
                !stream
                    .push(b"{\"preview\":true,\"offset\":0,\"result\":{\"old\":\"discard\"}}\n")
                    .unwrap()
            );
            let body = format!(
                "{}\n{}\n",
                json!({"preview":!final_batch,"offset":0,"result":{"new":0}}),
                json!({"preview":!final_batch,"offset":1,"result":{"new":1}})
            );
            assert!(stream.push(body.as_bytes()).unwrap());
            assert_eq!(stream.rows, [json!({"new":0}), json!({"new":1})]);
            assert_eq!(stream.columns, ["new"]);
            assert_eq!(stream.preview, Some(!final_batch));
            assert!(stream.limited && !stream.complete);
        }
    }

    #[test]
    fn final_offset_skips_only_final_rows_and_incomplete_preview_is_not_success() {
        let mut stream = Export::new(Some(1), 2, false);
        assert!(stream.push(concat!(
            "{\"preview\":true,\"results\":[{\"n\":100},{\"n\":101},{\"n\":102}],\"lastrow\":true}\n",
            "{\"preview\":false,\"results\":[{\"n\":0},{\"n\":1},{\"n\":2},{\"n\":3}]}\n"
        ).as_bytes()).unwrap());
        assert_eq!(stream.rows, [json!({"n":2})]);
        assert_eq!(stream.preview, Some(false));
        for (offset, body) in [
            (0, "{\"preview\":true,\"result\":{\"n\":1}}\n"),
            (
                2,
                "{\"preview\":true,\"result\":{\"n\":1},\"lastrow\":true}\n",
            ),
        ] {
            let mut stream = Export::new(Some(30), offset, true);
            assert!(!stream.push(body.as_bytes()).unwrap());
            assert!(stream.finish().is_err());
        }
    }

    #[test]
    fn export_handles_every_utf8_boundary_previews_fields_and_messages() {
        let body = concat!(
            "\n{\"preview\":true,\"result\":{\"temporary\":\"ignore\"},\"lastrow\":true}\n",
            "{\"preview\":false,\"fields\":[{\"name\":\"host\"}],\"result\":{\"host\":\"сервер🦀\"}}\n",
            "{\"preview\":false,\"results\":[{\"host\":\"two\",\"extra\":[\"a\",\"b\"]}],",
            "\"messages\":[{\"type\":\"WARN\",\"text\":\"fixture\"}],\"lastrow\":true}"
        );
        for boundary in 0..=body.len() {
            let mut stream = Export::new(None, 0, false);
            stream.push(&body.as_bytes()[..boundary]).unwrap();
            stream.push(&body.as_bytes()[boundary..]).unwrap();
            stream.finish().unwrap();
            assert_eq!(stream.rows.len(), 2);
            assert_eq!(stream.rows[0]["host"], "сервер🦀");
            assert_eq!(stream.columns, ["host", "extra"]);
            assert_eq!(stream.messages[0]["text"], "fixture");
            assert_eq!(stream.previews, 1);
            assert!(stream.complete);
        }
    }

    #[test]
    fn export_stops_at_count_and_never_treats_bad_streams_as_success() {
        let mut stream = Export::new(Some(1), 0, false);
        assert!(
            stream
                .push(b"{\"preview\":false,\"result\":{\"x\":1}}\nnot read")
                .unwrap()
        );
        assert_eq!(stream.rows, [json!({"x":1})]);
        assert!(stream.limited && !stream.complete);
        for body in [
            "",
            "\n ",
            "{\"preview\":false,\"result\":",
            "{\"unrecognized\":1}",
            "{\"preview\":true,\"result\":{\"x\":1}}\n",
            "{\"preview\":true,\"result\":{\"x\":1}}\n{\"messages\":[]}\n",
            "{\"preview\":\"false\",\"results\":[]}\n",
            "{\"result\":{\"x\":1}}\n",
            "{\"preview\":false,\"result\":[]}\n",
            "{\"messages\":[{\"type\":\"ERROR\",\"text\":\"bad search\"}]}\n",
            "{\"preview\":false,\"results\":{}}\n",
        ] {
            let mut stream = Export::new(None, 0, false);
            assert!(
                stream
                    .push(body.as_bytes())
                    .and_then(|_| stream.finish())
                    .is_err(),
                "{body}"
            );
        }
        let mut stream = Export::new(None, 0, false);
        stream
            .push(b"{\"preview\":false,\"results\":[]}\n")
            .unwrap();
        stream.finish().unwrap();
        assert!(stream.rows.is_empty());
    }

    #[test]
    fn export_bounds_partial_records_and_retained_results() {
        let mut stream = Export::new(None, 0, false);
        stream.pending = vec![b' '; MAX_RESPONSE_BYTES];
        assert!(stream.push(b"x").unwrap_err().message.contains("64 MiB"));
        stream.pending.clear();
        stream.retained = MAX_RESPONSE_BYTES;
        assert!(
            stream
                .push(b"{\"preview\":false,\"result\":{\"x\":1}}\n")
                .unwrap_err()
                .message
                .contains("reduce Count")
        );
    }
}
