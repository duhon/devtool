//! Discovery: finds the cluster root behind a request URL, also under a reverse-proxy
//! mount, and reads its mappings and aliases with the check's headers. It never writes.

use crate::schema;
use serde_json::Value;
use services_module_api::error::{Error, Result, headers};
use services_module_api::http::header_list;
use services_module_api::proto::host::HttpRequest;
use services_module_api::sdk::Host;
use url::Url;

/// API families that need only their paths: discovering them skips the mappings.
const SERVICE_APIS: [&str; 5] = ["_cluster", "_cat", "_nodes", "_tasks", "_snapshot"];
/// Discovery reads metadata only; a slow cluster must not hold the console.
const TIMEOUT_MS: u64 = 5000;

/// The console's discovery for the request in `params`.
pub async fn discover(host: &Host, params: &Value) -> Result<Value> {
    let url = Url::parse(&crate::request(params)?.url).map_err(|e| Error::new(e.to_string()))?;
    let headers = headers(params)?;
    let base = cluster(host, &url, &headers).await?;
    if let Some(scope) = service_scope(&base, &url) {
        return Ok(schema::services(&base, &scope));
    }
    let mappings = metadata(host, &base, "_mapping", "mappings", &headers).await?;
    let aliases = metadata(host, &base, "_alias", "aliases", &headers).await?;
    Ok(schema::indices(&base, &mappings, &aliases))
}

/// The first of the URL's roots whose `GET` answers with OpenSearch's cluster info.
async fn cluster(host: &Host, url: &Url, headers: &Value) -> Result<Url> {
    let mut failure = None;
    for root in roots(url) {
        let (status, info) = fetch(host, &root, headers).await?;
        if status == 200 && opensearch(&info) {
            return Ok(root);
        }
        // A refused authorization explains the miss better than a proxy path's 404.
        if status != 200 && (failure.is_none() || matches!(status, 401 | 403)) {
            failure = Some(http_error(status));
        }
    }
    Err(failure.unwrap_or_else(|| Error::new("No OpenSearch cluster found at this endpoint")))
}

/// Where the cluster may be mounted: the URL's path prefixes before its first `_api`
/// segment, longest first, without query.
fn roots(url: &Url) -> Vec<Url> {
    let parts: Vec<_> = url
        .path()
        .split('/')
        .filter(|part| !part.is_empty())
        .take_while(|part| !part.starts_with('_'))
        .collect();
    (0..=parts.len())
        .rev()
        .map(|length| {
            let mut root = url.clone();
            root.set_path(&match length {
                0 => "/".to_owned(),
                _ => format!("/{}/", parts[..length].join("/")),
            });
            root.set_query(None);
            root.set_fragment(None);
            root
        })
        .collect()
}

fn opensearch(info: &Value) -> bool {
    info["version"]["number"].is_string()
        && (info["version"]["distribution"] == "opensearch"
            || info["tagline"]
                .as_str()
                .is_some_and(|tagline| tagline.contains("OpenSearch")))
}

/// The cache scope of a request to a service API under `base`, e.g. `base/_cluster`:
/// its paths-only discovery must not hide the index schema from other requests.
fn service_scope(base: &Url, url: &Url) -> Option<Url> {
    let family = url.path().strip_prefix(base.path())?.split('/').next()?;
    if !SERVICE_APIS.contains(&family) {
        return None;
    }
    let mut scope = base.clone();
    scope.path_segments_mut().ok()?.pop_if_empty().push(family);
    Some(scope)
}

/// `GET base/path`: an object of `{name: {key: {...}}}` for every index.
async fn metadata(
    host: &Host,
    base: &Url,
    path: &str,
    key: &str,
    headers: &Value,
) -> Result<Value> {
    let url = base.join(path).map_err(|e| Error::new(e.to_string()))?;
    let (status, document) = fetch(host, &url, headers).await?;
    if status != 200 {
        return Err(http_error(status));
    }
    let valid = document
        .as_object()
        .is_some_and(|indices| indices.values().all(|index| index[key].is_object()));
    if !valid {
        return Err(Error::new(format!(
            "OpenSearch returned invalid discovery metadata for /{path}"
        )));
    }
    Ok(document)
}

/// `GET url`: the status and the body as JSON, `null` when it is not.
async fn fetch(host: &Host, url: &Url, headers: &Value) -> Result<(u32, Value)> {
    let response = host
        .http(&HttpRequest {
            method: "GET".into(),
            url: url.to_string(),
            headers: header_list(headers),
            timeout_ms: TIMEOUT_MS,
            no_redirects: true,
            no_retries: true,
            ..Default::default()
        })
        .await?;
    Ok((
        response.status,
        serde_json::from_slice(&response.body).unwrap_or(Value::Null),
    ))
}

fn http_error(status: u32) -> Error {
    Error::new(format!(
        "OpenSearch discovery failed (HTTP {status}); check the endpoint and metadata permissions"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_strip_the_api_and_query_and_keep_a_proxy_mount() {
        let url =
            Url::parse("https://example.test/proxy/opensearch/logs-*/_search?size=5").unwrap();
        assert_eq!(
            roots(&url).iter().map(Url::as_str).collect::<Vec<_>>(),
            [
                "https://example.test/proxy/opensearch/logs-*/",
                "https://example.test/proxy/opensearch/",
                "https://example.test/proxy/",
                "https://example.test/"
            ]
        );
    }

    #[test]
    fn only_service_apis_get_their_own_scope() {
        let base = Url::parse("http://localhost:9201/proxy/").unwrap();
        let scope = service_scope(&base, &base.join("_cluster/health").unwrap()).unwrap();
        assert_eq!(scope.as_str(), "http://localhost:9201/proxy/_cluster");
        for path in ["logs/_search", "_search", "_bulk"] {
            assert!(
                service_scope(&base, &base.join(path).unwrap()).is_none(),
                "{path}"
            );
        }
    }
}
