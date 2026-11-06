//! HTTP requests through `Host/Http`, answered in the console's REST response shape.

use crate::error::{Error, Result, string};
use crate::proto::host::{Header, HttpRequest, HttpResponse};
use crate::sdk::Host;
use serde_json::{Map, Value, json};
use std::time::{Duration, Instant};

/// `headers` object → header list, values as text.
pub fn header_list(headers: &Value) -> Vec<Header> {
    headers
        .as_object()
        .map(|headers| {
            headers
                .iter()
                .map(|(name, value)| Header {
                    name: name.clone(),
                    value: string(value),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Response headers as a JSON object; a repeated name keeps its first position and the last value.
pub fn header_map(headers: &[Header]) -> Map<String, Value> {
    headers
        .iter()
        .map(|header| (header.name.clone(), json!(header.value)))
        .collect()
}

/// The body of an `HttpRequest.text` call as text, without validating it again.
pub fn body_text(response: &mut HttpResponse) -> String {
    let body = std::mem::take(&mut response.body);
    // SAFETY: with `HttpRequest.text` the host sends valid UTF-8 (`host.proto`).
    unsafe { String::from_utf8_unchecked(body) }
}

/// A REST request: GET and HEAD send no body.
pub async fn request(
    host: &Host,
    method: &str,
    url: &str,
    headers: &Value,
    body: Option<String>,
) -> Result<Value> {
    let started = Instant::now();
    let url = url::Url::parse(url).map_err(|_| Error::new(format!("Invalid URL: {url}")))?;
    let method = method.to_uppercase();
    let request = json!({"method":method,"url":url.as_str(),"headers":headers});
    let method =
        http::Method::from_bytes(method.as_bytes()).map_err(|e| Error::new(e.to_string()))?;
    let body = if method != http::Method::GET && method != http::Method::HEAD {
        body.filter(|s| !s.is_empty()).map(String::into_bytes)
    } else {
        None
    };
    let call = HttpRequest {
        method: method.to_string(),
        url: url.into(),
        headers: header_list(headers),
        body,
        ..Default::default()
    };
    exchange(host, call, request, started).await
}

/// Sends `call` as is, its body read as text, and answers in the console's REST response
/// shape: status, headers, body and the request, connection and response under `debug`.
pub async fn send(host: &Host, call: HttpRequest) -> Result<Value> {
    let started = Instant::now();
    let request = json!({"method":call.method,"url":call.url,"headers":header_map(&call.headers)});
    exchange(host, call, request, started).await
}

/// `send` with `request` shown under `debug.request`, timed from `started`.
async fn exchange(
    host: &Host,
    mut call: HttpRequest,
    request: Value,
    started: Instant,
) -> Result<Value> {
    call.text = true;
    let secure = call.url.starts_with("https:");
    let sent = started.elapsed();
    let mut response = match host.http(&call).await {
        Ok(response) => response,
        Err(error) => {
            let mut error = error.with_debug(request);
            error.duration_ms = Some(started.elapsed().as_millis());
            return Err(error);
        }
    };
    let ttfb = (sent + Duration::from_micros(response.headers_us)).as_millis();
    let status = response.status;
    let headers = header_map(&response.headers);
    let body = body_text(&mut response);
    let duration = started.elapsed().as_millis();
    let reply = json!({"status":status,"statusText":response.status_text,"headers":headers});
    let mut connection = json!({"timing":{"ttfbMs":ttfb},"events":[
        {"event":"responseHeaders","atMs":ttfb,"data":{"httpVersion":response.http_version,"status":status}},
        {"event":"responseBody","atMs":duration,"data":{"byteLength":body.len()}}
    ]});
    if let Some(address) = response.remote_address {
        connection["remoteAddress"] = json!(address);
        connection["remotePort"] = json!(response.remote_port);
        connection["remoteFamily"] = json!(response.remote_family);
    }
    if secure {
        connection["tls"] = json!({"encrypted":true});
    }
    Ok(json!({
        "status":status,"statusText":response.status_text,
        "headers":headers,"body":body,"durationMs":duration,
        "debug":{"request":request,"connection":connection,"response":reply}
    }))
}
