use crate::core::error::{Error, Result, required, string, text};
use serde_json::{Value, json};
use std::time::Instant;

pub fn headers(params: &Value) -> Result<Value> {
    match params.get("headers") {
        None | Some(Value::Null) => Ok(json!({})),
        Some(v) if v.is_object() => Ok(v.clone()),
        _ => Err(Error::new("headers must be an object of name/value pairs")),
    }
}
pub async fn request(
    client: &reqwest::Client,
    method: &str,
    url: &str,
    headers: &Value,
    body: Option<String>,
) -> Result<Value> {
    let started = Instant::now();
    let url = reqwest::Url::parse(url).map_err(|_| Error::new(format!("Invalid URL: {url}")))?;
    let method = method.to_uppercase();
    let request = json!({"method":method,"url":url.as_str(),"headers":headers});
    let method =
        reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| Error::new(e.to_string()))?;
    let secure = url.scheme() == "https";
    let mut builder = client.request(method.clone(), url);
    if let Some(headers) = headers.as_object() {
        for (name, value) in headers {
            builder = builder.header(name, string(value));
        }
    }
    if method != reqwest::Method::GET
        && method != reqwest::Method::HEAD
        && let Some(body) = body.filter(|s| !s.is_empty())
    {
        builder = builder.body(body);
    }
    let result = async {
        let response = builder.send().await?;
        let ttfb=started.elapsed().as_millis();
        let remote=response.remote_addr();
        let version=format!("{:?}",response.version());
        let status = response.status();
        let headers:serde_json::Map<String,Value> = response.headers().iter().map(|(k,v)|(k.to_string(),json!(v.to_str().unwrap_or("")))).collect();
        let body = response.text().await?;
        let duration = started.elapsed().as_millis();
        let response = json!({"status":status.as_u16(),"statusText":status.canonical_reason().unwrap_or(""),"headers":headers});
        let mut connection=json!({"timing":{"ttfbMs":ttfb},"events":[
            {"event":"responseHeaders","atMs":ttfb,"data":{"httpVersion":version,"status":status.as_u16()}},
            {"event":"responseBody","atMs":duration,"data":{"byteLength":body.len()}}
        ]});
        if let Some(remote)=remote {
            connection["remoteAddress"]=json!(remote.ip().to_string());
            connection["remotePort"]=json!(remote.port());
            connection["remoteFamily"]=json!(if remote.is_ipv4(){"IPv4"}else{"IPv6"});
        }
        if secure{connection["tls"]=json!({"encrypted":true});}
        Ok::<_,reqwest::Error>(json!({
            "status":status.as_u16(),"statusText":status.canonical_reason().unwrap_or(""),
            "headers":headers,"body":body,"durationMs":duration,
            "debug":{"request":request,"connection":connection,"response":response}
        }))
    }.await;
    result.map_err(|e| {
        let mut error = Error::from(e).with_debug(request);
        error.duration_ms = Some(started.elapsed().as_millis());
        error
    })
}
pub async fn send(client: &reqwest::Client, params: &Value) -> Result<Value> {
    let method = required(params, "method")?;
    let url = text(params, "url");
    let headers = headers(params)?;
    let body = params.get("body").filter(|v| !v.is_null()).map(string);
    request(client, &method, &url, &headers, body).await
}
