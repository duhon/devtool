use axum::{Router, body::to_bytes, extract::Request, http::StatusCode, response::IntoResponse};
use serde_json::{Value, json};
use services_testkit::{Host, http::Server};
use std::sync::{Arc, Mutex};

type Seen = Arc<Mutex<Vec<Value>>>;

/// A cluster mounted at `/proxy/` behind a reverse proxy; `info` answers its root.
async fn cluster(info: Value, status: StatusCode) -> (Server, Seen) {
    let seen: Seen = Arc::default();
    let requests = seen.clone();
    let server = Server::new(Router::new().fallback(move |request: Request| {
        let requests = requests.clone();
        let info = info.clone();
        async move {
            let mut record = record(&request);
            let body = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
            record["body"] = json!(String::from_utf8(body.to_vec()).unwrap());
            let path = record["path"].as_str().unwrap_or_default().to_owned();
            requests.lock().unwrap().push(record);
            let reply = match path.as_str() {
                "/proxy/" => info,
                "/proxy/_mapping" => json!({
                    "logs":{"mappings":{"properties":{"message":{"type":"text"}}}},
                    ".system":{"mappings":{"properties":{"secret":{"type":"keyword"}}}}
                }),
                "/proxy/_alias" => json!({"logs":{"aliases":{"recent":{}}}}),
                "/proxy/logs/_search" | "/proxy/_bulk" => json!({"took":1,"errors":false}),
                _ => {
                    return (StatusCode::NOT_FOUND, r#"{"error":"no_such_index"}"#).into_response();
                }
            };
            (status, axum::Json(reply)).into_response()
        }
    }))
    .await;
    (server, seen)
}

/// What the cluster saw of a request, besides its body.
fn record(request: &Request) -> Value {
    let header = |name| {
        let value = request.headers().get(name)?;
        value.to_str().ok().map(str::to_owned)
    };
    json!({
        "method":request.method().as_str(),"path":request.uri().path(),
        "authorization":header("authorization"),"contentType":header("content-type")
    })
}

fn opensearch() -> Value {
    json!({"version":{"distribution":"opensearch","number":"2.19.0"},"tagline":"The OpenSearch Project"})
}

#[tokio::test]
async fn searches_keep_get_bodies_and_bulk_bodies_become_ndjson() {
    let (server, seen) = cluster(opensearch(), StatusCode::OK).await;
    let host = Host::new();
    let search = r#"{"query":{"match_all":{}}}"#;
    let reply = host
        .call(
            "opensearch",
            json!({"method":"GET","url":format!("{}/proxy/logs/_search",server.url),"body":search}),
        )
        .await;
    assert_eq!(reply.ok()["status"], 200, "{reply}");
    assert_eq!(
        reply.ok()["debug"]["request"]["headers"]["Content-Type"],
        "application/json"
    );
    let reply = host
        .call(
            "opensearch",
            json!({"method":"POST","url":format!("{}/proxy/_bulk",server.url),"body":"{\"index\":{}}\n{\"a\":1}"}),
        )
        .await;
    assert_eq!(reply.ok()["status"], 200, "{reply}");
    let missing = host
        .call(
            "opensearch",
            json!({"method":"GET","url":format!("{}/proxy/missing/_search",server.url)}),
        )
        .await;
    assert_eq!(missing.ok()["status"], 404, "{missing}");
    assert_eq!(missing.ok()["body"], r#"{"error":"no_such_index"}"#);
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["method"], "GET");
    assert_eq!(seen[0]["body"], search);
    assert_eq!(seen[0]["contentType"], "application/json");
    assert_eq!(seen[1]["body"], "{\"index\":{}}\n{\"a\":1}\n");
    assert_eq!(seen[1]["contentType"], "application/x-ndjson");
}

#[tokio::test]
async fn discovery_finds_the_mounted_cluster_and_reads_metadata_with_the_check_headers() {
    let (server, seen) = cluster(opensearch(), StatusCode::OK).await;
    let host = Host::new();
    let params = |path: &str| {
        json!({"method":"GET","url":format!("{}/proxy/{path}",server.url),
            "headers":{"Authorization":"Basic fixture"}})
    };
    let reply = host.discover("opensearch", params("logs/_search")).await;
    let schema = reply.schema();
    assert_eq!(schema["baseUrl"], format!("{}/proxy", server.url));
    let paths = schema["paths"].as_array().unwrap();
    assert!(paths.contains(&json!("/proxy/recent/_search")), "{reply}");
    assert!(
        !paths
            .iter()
            .any(|path| path.as_str().unwrap().starts_with("/proxy/."))
    );
    assert!(schema["bodySchemas"]["GET /proxy/{index}/_search"].is_array());
    let reads: Vec<_> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|request| request["path"].clone())
        .collect();
    assert_eq!(
        reads,
        [
            "/proxy/logs/",
            "/proxy/",
            "/proxy/_mapping",
            "/proxy/_alias"
        ]
    );
    assert!(seen.lock().unwrap().iter().all(|request| {
        request["method"] == "GET" && request["authorization"] == "Basic fixture"
    }));
    seen.lock().unwrap().clear();
    let reply = host.discover("opensearch", params("_cluster/health")).await;
    assert_eq!(
        reply.schema()["baseUrl"],
        format!("{}/proxy/_cluster", server.url)
    );
    assert_eq!(reply.schema()["bodySchemas"], json!({}));
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["path"] == "/proxy/_mapping")
    );
}

#[tokio::test]
async fn discovery_reports_refused_metadata_and_other_servers() {
    let host = Host::new();
    let (server, _) = cluster(opensearch(), StatusCode::FORBIDDEN).await;
    let reply = host
        .discover(
            "opensearch",
            json!({"url":format!("{}/proxy/logs/_search",server.url)}),
        )
        .await;
    assert!(reply.discovery_failure().contains("HTTP 403"), "{reply}");
    let elasticsearch = json!({"version":{"number":"8.0.0"},"tagline":"You Know, for Search"});
    let server =
        Server::new(Router::new().fallback(move || async move { axum::Json(elasticsearch) })).await;
    let reply = host
        .discover(
            "opensearch",
            json!({"url":format!("{}/proxy/logs/_search",server.url)}),
        )
        .await;
    assert!(
        reply.discovery_failure().contains("No OpenSearch cluster"),
        "{reply}"
    );
}
