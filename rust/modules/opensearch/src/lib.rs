//! OpenSearch: requests to the cluster's HTTP API and discovery from its index mappings,
//! all I/O through the host.

mod discovery;
pub mod schema;

use serde_json::{Map, Value, json};
use services_module_api::error::{Error, Result, headers, required, string};
use services_module_api::http::{self, header_list};
use services_module_api::proto::host::HttpRequest;
use services_module_api::sdk::{self, Call, Manifest, Service, Threads};
use services_module_api::transport::{EndpointArc, EndpointBox, Status};
use url::Url;

pub struct OpenSearch;

impl Service for OpenSearch {
    fn manifest(&self) -> Manifest {
        Manifest {
            execute: true,
            discover: true,
            stream: false,
            ..Manifest::from_json(include_str!("../manifest.json"), env!("CARGO_PKG_VERSION"))
        }
    }

    async fn execute(&self, call: Call) -> Result<Value> {
        http::send(&call.host, request(&call.params)?).await
    }

    async fn discover(&self, call: Call) -> Result<Value> {
        discovery::discover(&call.host, &call.params).await
    }
}

pub fn endpoint(host: EndpointArc) -> std::result::Result<EndpointBox, Status> {
    sdk::serve(OpenSearch, host, Threads::Host)
}

services_module_api::export!(endpoint);

/// The methods of the console's `#endpoint`.
const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
/// APIs whose body is NDJSON: one JSON document per line.
const NDJSON_APIS: [&str; 3] = ["/_bulk", "/_msearch", "/_msearch/template"];
/// Searches and bulk loads may run long.
const TIMEOUT_MS: u64 = 120_000;

/// The HTTP call for request params. Unlike REST, a GET keeps its body (search); a body
/// without a Content-Type gets the JSON one, or NDJSON's for bulk and multi-search, whose
/// body then also ends with a newline. Writes are never retried or redirected.
fn request(params: &Value) -> Result<HttpRequest> {
    let url = Url::parse(required(params, "url")?.trim())
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
        .ok_or_else(|| Error::new("url must be an absolute HTTP(S) URL"))?;
    if url.fragment().is_some() {
        return Err(Error::new("url must not contain a fragment"));
    }
    let method = params["method"]
        .as_str()
        .unwrap_or("GET")
        .trim()
        .to_ascii_uppercase();
    if !METHODS.contains(&method.as_str()) {
        return Err(Error::new(format!("Unsupported HTTP method: {method}")));
    }
    let mut headers = headers(params)?;
    let mut body = params
        .get("body")
        .filter(|body| !body.is_null())
        .map(string)
        .filter(|body| !body.is_empty());
    if let (Some(body), Some(headers)) = (&mut body, headers.as_object_mut()) {
        let ndjson = ndjson(&url, headers);
        if ndjson && !body.ends_with('\n') {
            body.push('\n');
        }
        if !headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("content-type"))
        {
            let content_type = if ndjson {
                "application/x-ndjson"
            } else {
                "application/json"
            };
            headers.insert("Content-Type".into(), json!(content_type));
        }
    }
    Ok(HttpRequest {
        method,
        url: url.into(),
        headers: header_list(&headers),
        body: body.map(String::into_bytes),
        timeout_ms: TIMEOUT_MS,
        no_redirects: true,
        no_retries: true,
        ..Default::default()
    })
}

/// Whether the body is NDJSON: by the API in the path, or by the Content-Type given.
fn ndjson(url: &Url, headers: &Map<String, Value>) -> bool {
    let path = url.path().trim_end_matches('/');
    NDJSON_APIS.iter().any(|api| path.ends_with(api))
        || headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("content-type")
                && value.as_str().is_some_and(|value| {
                    let media = value.split(';').next().unwrap_or_default();
                    media.trim().eq_ignore_ascii_case("application/x-ndjson")
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_get_search_keeps_its_body_query_and_proxy_mount() {
        let request = request(&json!({
            "url": "https://example.test/proxy/opensearch/logs-*/_search?size=5&filter_path=hits.hits",
            "method": "get",
            "body": "{\"query\":{\"match_all\":{}}}",
            "headers": {"Authorization": "Basic fixture"}
        }))
        .unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(
            request.url,
            "https://example.test/proxy/opensearch/logs-*/_search?size=5&filter_path=hits.hits"
        );
        assert_eq!(request.body.unwrap(), br#"{"query":{"match_all":{}}}"#);
        assert!(
            request
                .headers
                .iter()
                .any(|header| header.name == "Authorization")
        );
        assert!(
            request.headers.iter().any(|header| {
                header.name == "Content-Type" && header.value == "application/json"
            })
        );
        assert!(request.no_retries && request.no_redirects);
    }

    #[test]
    fn ndjson_apis_get_their_content_type_and_exactly_one_final_newline() {
        for path in [
            "/_bulk?refresh=wait_for",
            "/logs/_bulk",
            "/_msearch",
            "/logs/_msearch/template/",
        ] {
            for body in ["{}\n{}", "{}\n{}\n"] {
                let request = request(&json!({
                    "url": format!("http://localhost:9200{path}"),
                    "method": "POST",
                    "body": body
                }))
                .unwrap();
                assert_eq!(request.body.unwrap(), b"{}\n{}\n", "{path}");
                assert!(request.headers.iter().any(|header| {
                    header.name == "Content-Type" && header.value == "application/x-ndjson"
                }));
            }
        }
    }

    #[test]
    fn a_given_content_type_wins_and_an_ndjson_one_ends_the_body() {
        let bulk = request(&json!({
            "url": "http://localhost:9200/_bulk",
            "headers": {"content-type": "application/json"},
            "body": "{}"
        }))
        .unwrap();
        assert_eq!(bulk.headers.len(), 1);
        assert_eq!(bulk.headers[0].value, "application/json");
        assert_eq!(bulk.body.unwrap(), b"{}\n");
        let custom = request(&json!({
            "url": "http://localhost:9200/custom",
            "headers": {"Content-Type": "application/x-ndjson; charset=utf-8"},
            "body": "{}"
        }))
        .unwrap();
        assert_eq!(custom.body.unwrap(), b"{}\n");
    }

    #[test]
    fn an_empty_body_adds_no_content_type() {
        let request = request(&json!({"url": "http://localhost:9200", "body": ""})).unwrap();
        assert!(request.body.is_none());
        assert!(request.headers.is_empty());
        assert_eq!(request.url, "http://localhost:9200/");
    }

    #[test]
    fn invalid_urls_methods_and_headers_are_rejected() {
        for url in [
            "file:///tmp/test",
            "localhost:9200",
            "//localhost:9200/",
            "/_search",
            "http://localhost:9200/_search#fragment",
        ] {
            assert!(request(&json!({"url": url})).is_err(), "{url}");
        }
        assert!(request(&json!({"url": "http://localhost:9200", "headers": []})).is_err());
        assert!(request(&json!({"url": "http://localhost:9200", "method": "TRACE"})).is_err());
    }
}
