//! The http driver: one client without automatic redirects or retries; the driver follows
//! redirects itself and records each hop.

use super::{CONNECT_MS, Failure, millis};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use reqwest::{Method, StatusCode, Url, header};
use serde_json::{Value, json};
use services_sdk::{
    driver::Stage,
    http::{Hop, Remote, Request, Response, Timing},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::mpsc;

const REDIRECTS: u32 = 10;
/// Headers dropped when a redirect leaves the origin.
const PRIVATE: [header::HeaderName; 3] = [
    header::AUTHORIZATION,
    header::COOKIE,
    header::PROXY_AUTHORIZATION,
];

/// Clients by connect timeout: reqwest sets it per client, not per request.
#[derive(Clone)]
pub struct Http {
    clients: Arc<Mutex<HashMap<u64, reqwest::Client>>>,
}

impl Http {
    pub fn new() -> Result<Self, Failure> {
        let http = Self {
            clients: Default::default(),
        };
        http.client(CONNECT_MS)?;
        Ok(http)
    }

    fn client(&self, connect_ms: u64) -> Result<reqwest::Client, Failure> {
        let mut clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(client) = clients.get(&connect_ms) {
            return Ok(client.clone());
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(millis(connect_ms))
            .build()
            .map_err(|error| failure(&error))?;
        clients.insert(connect_ms, client.clone());
        Ok(client)
    }

    pub async fn call(&self, request: Request) -> Result<Value, Failure> {
        let started = Instant::now();
        let (response, mut head) = self.send(&request, started).await?;
        let body = response.bytes().await.map_err(|error| failure(&error))?;
        if request.text {
            head.body = Some(String::from_utf8_lossy(&body).into_owned());
        } else {
            head.body_base64 = Some(B64.encode(&body));
        }
        head.timing.total_us = micros(started);
        Ok(json!(head))
    }

    /// Events: `{"head": response without a body}`, then `{"chunk": text}` or
    /// `{"chunkBase64": …}`; the answer is `{"timing": …}`.
    pub async fn stream(
        &self,
        request: Request,
        events: mpsc::Sender<Value>,
    ) -> Result<Value, Failure> {
        let started = Instant::now();
        let (mut response, mut head) = self.send(&request, started).await?;
        let closed = || Failure::new(Stage::Cancelled, "The stream reader went away");
        events
            .send(json!({ "head": head }))
            .await
            .map_err(|_| closed())?;
        let mut pending = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| failure(&error))? {
            let event = if request.text {
                pending.extend_from_slice(&chunk);
                let text = complete_text(&mut pending);
                if text.is_empty() {
                    continue;
                }
                json!({ "chunk": text })
            } else {
                json!({ "chunkBase64": B64.encode(&chunk) })
            };
            events.send(event).await.map_err(|_| closed())?;
        }
        if !pending.is_empty() {
            let text = String::from_utf8_lossy(&pending).into_owned();
            events
                .send(json!({ "chunk": text }))
                .await
                .map_err(|_| closed())?;
        }
        head.timing.total_us = micros(started);
        Ok(json!({ "timing": head.timing }))
    }

    /// Sends the request and follows redirects; the head has everything but the body.
    async fn send(
        &self,
        request: &Request,
        started: Instant,
    ) -> Result<(reqwest::Response, Response), Failure> {
        let invalid = |message: String| Failure::new(Stage::Send, message);
        let client = self.client(request.connect_ms.unwrap_or(CONNECT_MS))?;
        let mut method = Method::from_bytes(request.method.as_bytes())
            .map_err(|_| invalid(format!("Invalid method \"{}\"", request.method)))?;
        let mut url = Url::parse(&request.url)
            .map_err(|error| invalid(format!("Invalid URL \"{}\": {error}", request.url)))?;
        let mut headers = header::HeaderMap::new();
        for (name, value) in &request.headers {
            let name = header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| invalid(format!("Invalid header name \"{name}\"")))?;
            let value = header::HeaderValue::from_str(value)
                .map_err(|_| invalid(format!("Invalid value of header \"{name}\"")))?;
            headers.append(name, value);
        }
        let mut body = match (&request.body, &request.body_base64) {
            (Some(body), _) => Some(body.clone().into_bytes()),
            (None, Some(body)) => Some(
                B64.decode(body)
                    .map_err(|error| invalid(format!("Invalid bodyBase64: {error}")))?,
            ),
            (None, None) => None,
        };
        let deadline = request.timeout_ms.map(|ms| started + millis(ms));
        let mut hops = Vec::new();
        loop {
            let mut builder = client
                .request(method.clone(), url.clone())
                .headers(headers.clone());
            if let Some(body) = &body {
                builder = builder.body(body.clone());
            }
            if let Some(deadline) = deadline {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(Failure::new(Stage::Timeout, "The request timed out"));
                }
                builder = builder.timeout(left);
            }
            let response = builder.send().await.map_err(|error| failure(&error))?;
            let status = response.status();
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .filter(|_| status.is_redirection() && status != StatusCode::NOT_MODIFIED);
            let follow = hops.len() < request.redirects.unwrap_or(REDIRECTS) as usize;
            let Some(location) = location.filter(|_| follow) else {
                let head = head(&response, hops, micros(started));
                return Ok((response, head));
            };
            let next = url.join(location).map_err(|error| {
                Failure::new(
                    Stage::Decode,
                    format!("Invalid redirect location \"{location}\": {error}"),
                )
            })?;
            hops.push(Hop {
                status: status.as_u16(),
                url: url.to_string(),
            });
            let to_get = status == StatusCode::SEE_OTHER && method != Method::HEAD
                || matches!(status, StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND)
                    && method == Method::POST;
            if to_get {
                method = Method::GET;
                body = None;
                headers.remove(header::CONTENT_TYPE);
                headers.remove(header::CONTENT_LENGTH);
            }
            if next.origin() != url.origin() {
                for name in &PRIVATE {
                    headers.remove(name);
                }
            }
            url = next;
        }
    }
}

fn head(response: &reqwest::Response, redirects: Vec<Hop>, first_byte_us: u64) -> Response {
    let status = response.status();
    Response {
        status: status.as_u16(),
        status_text: status.canonical_reason().unwrap_or("").into(),
        http_version: format!("{:?}", response.version()),
        headers: response
            .headers()
            .iter()
            .map(|(name, value)| {
                let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
                (name.to_string(), value)
            })
            .collect(),
        url: response.url().to_string(),
        body: None,
        body_base64: None,
        redirects,
        timing: Timing {
            first_byte_us,
            total_us: first_byte_us,
        },
        remote: response.remote_addr().map(|remote| Remote {
            address: remote.ip().to_string(),
            port: remote.port(),
            family: if remote.is_ipv4() { "IPv4" } else { "IPv6" }.into(),
        }),
    }
}

fn micros(started: Instant) -> u64 {
    started.elapsed().as_micros() as u64
}

/// The valid UTF-8 prefix of `pending`; an incomplete character at the end waits for the next
/// chunk, invalid bytes become U+FFFD.
fn complete_text(pending: &mut Vec<u8>) -> String {
    match std::str::from_utf8(pending) {
        Ok(text) => {
            let text = text.to_owned();
            pending.clear();
            text
        }
        Err(error) if error.error_len().is_none() => {
            let rest = pending.split_off(error.valid_up_to());
            let text = std::mem::replace(pending, rest);
            String::from_utf8(text).unwrap_or_default()
        }
        Err(_) => {
            let text = String::from_utf8_lossy(pending).into_owned();
            pending.clear();
            text
        }
    }
}

/// A reqwest error with its cause chain and the stage it failed at.
fn failure(error: &reqwest::Error) -> Failure {
    let mut causes = vec![error.to_string()];
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source.filter(|_| causes.len() < 9) {
        causes.push(cause.to_string());
        source = cause.source();
    }
    let all = causes.join("\n").to_lowercase();
    let stage = if error.is_timeout() {
        Stage::Timeout
    } else if error.is_connect() {
        if all.contains("dns error") || all.contains("failed to lookup address") {
            Stage::Dns
        } else if all.contains("certificate") || all.contains("tls") || all.contains("handshake") {
            Stage::Tls
        } else {
            Stage::Connect
        }
    } else if error.is_decode() || error.is_body() {
        Stage::Decode
    } else {
        Stage::Send
    };
    let mut failure = Failure::new(stage, format!("Request failed: {error}"));
    failure.failure.causes = causes;
    failure
}

#[cfg(test)]
mod tests {
    use super::complete_text;

    #[test]
    fn text_waits_for_a_split_character() {
        let crab = "🦀".as_bytes();
        let mut pending = vec![b'a', crab[0], crab[1]];
        assert_eq!(complete_text(&mut pending), "a");
        pending.extend_from_slice(&crab[2..]);
        assert_eq!(complete_text(&mut pending), "🦀");
        assert!(pending.is_empty());
    }
}
