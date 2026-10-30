use axum::{Router, extract::Request};
use serde_json::{Value, json};
use services_testkit::{
    Host,
    http::{self, Server},
};

#[tokio::test]
async fn variables_reach_the_server_as_typed_json() {
    let host = Host::new();
    let (server, seen) = http::services().await;
    let sku = "SKU-\"blue\"\\path\nnext";
    host.op(
        "vars",
        "save",
        json!({"name":"var","variables":{"sku":sku}}),
    )
    .await
    .ok();
    let query = "query ($skus: [String]) {\n  products (skus: $skus) {\n    sku\n    images {\n      url\n    }\n  }\n}";
    let variables = json!({"skus":["{{var:sku}}"]});
    for variables in [variables.clone(), json!(variables.to_string())] {
        let params =
            json!({"url":format!("{}/graphql",server.url),"query":query,"variables":variables});
        let reply = host.call("graphql", params).await;
        let body: Value = serde_json::from_str(reply.ok()["body"].as_str().unwrap()).unwrap();
        assert_eq!(body["data"]["echo"], json!({"skus":[sku]}), "{reply}");
        let request = seen.lock().unwrap().last().unwrap().clone();
        let sent: Value = serde_json::from_str(request["body"].as_str().unwrap()).unwrap();
        assert_eq!(sent, json!({"query":query,"variables":{"skus":[sku]}}));
    }
    let reply = host
        .call(
            "graphql",
            json!({
                "url":format!("{}/graphql",server.url),"query":query,
                "variables":{"skus":["{{var:missing}}"]}
            }),
        )
        .await;
    assert!(reply.error().contains("missing"), "{reply}");
    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "A missing nested variable must fail before any network request"
    );
}

#[tokio::test]
async fn discovery_errors_are_not_cached_as_empty_schemas() {
    let host = Host::new();
    let (server, seen) = http::services().await;
    let mut params = json!({"url":format!("{}/graphql-protected",server.url)});
    let reply = host.discover("graphql", params.clone()).await;
    assert!(
        reply
            .discovery_failure()
            .contains("Discovery requires X-Token"),
        "{reply}"
    );
    params["headers"] = json!({"X-Token":"discovery-token"});
    let reply = host.discover("graphql", params).await;
    assert_eq!(
        reply.ok()["types"]["Query"]["fields"][0]["name"],
        "pets",
        "{reply}"
    );
    assert_eq!(
        seen.lock().unwrap().last().unwrap()["token"],
        "discovery-token"
    );
    let failures = Server::new(Router::new().fallback(|request: Request| async move {
        let (status, body) = match request.uri().path() {
            "/errors" => (
                200,
                json!({"errors":[{"message":"Introspection is disabled"}]}).to_string(),
            ),
            "/missing" => (200, json!({"data":{"__schema":null}}).to_string()),
            "/empty" => (
                200,
                json!({"data":{"__schema":{"queryType":{"name":"Query"},"types":[]}}}).to_string(),
            ),
            "/partial" => (
                200,
                json!({"data":{"__schema":{}},"errors":[{"message":"Partial schema denied"}]})
                    .to_string(),
            ),
            "/http" => (403, "Forbidden".into()),
            _ => (200, "not JSON".into()),
        };
        (axum::http::StatusCode::from_u16(status).unwrap(), body)
    }))
    .await;
    for (path, expected) in [
        ("errors", "Introspection is disabled"),
        ("missing", "no schema"),
        ("empty", "query fields"),
        ("partial", "Partial schema denied"),
        ("http", "403"),
        ("invalid", "JSON"),
    ] {
        let reply = host
            .discover("graphql", json!({"url":format!("{}/{path}",failures.url)}))
            .await;
        assert!(reply.discovery_failure().contains(expected), "{reply}");
        assert!(
            reply.ok()["debug"].is_object(),
            "{path}: preserve discovery error diagnostics\n{reply}"
        );
    }
}
