use crate::core::error::{Error, Result, text};
use futures_util::future::join_all;
use serde_json::{Map, Value, json};
const CANDIDATES: [&str; 7] = [
    "/rest/default/schema?services=all",
    "/openapi.json",
    "/v3/api-docs",
    "/swagger.json",
    "/spec.json",
    "/swagger/v1/swagger.json",
    "/api-docs",
];
pub async fn discover(client: &reqwest::Client, params: &Value) -> Result<Value> {
    let headers = super::http::headers(params)?;
    let address = text(params, "url");
    let url = url::Url::parse(address.trim())
        .ok()
        .filter(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && address.trim().contains("://")
                && !address.trim().contains(char::is_whitespace)
        })
        .ok_or_else(|| {
            Error::new("Invalid discovery URL: expected http:// or https:// with a host")
        })?;
    let parts: Vec<_> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    let mut candidates = vec![];
    for i in (0..=parts.len()).rev() {
        let prefix = if i == 0 {
            String::new()
        } else {
            format!("/{}", parts[..i].join("/"))
        };
        for candidate in CANDIDATES {
            candidates.push(format!(
                "{}{prefix}{candidate}",
                url.origin().ascii_serialization()
            ));
        }
    }
    let headers = &headers;
    let mut debug = vec![];
    let mut failure = None;
    for group in candidates.chunks(CANDIDATES.len()) {
        let attempts = join_all(group.iter().map(|url| async move {
            let mut request = client.get(url);
            for (name, value) in headers.as_object().unwrap() {
                request = request.header(name, crate::core::error::string(value));
            }
            request
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await?
                .error_for_status()?
                .json::<Value>()
                .await
        }))
        .await;
        let mut transport_only = true;
        for (url, attempt) in group.iter().zip(attempts) {
            match attempt {
                Ok(spec) => {
                    transport_only = false;
                    let document = spec["swagger"] == "2.0"
                        || spec["openapi"]
                            .as_str()
                            .is_some_and(|v| v.starts_with("3."));
                    if document
                        && spec["paths"]
                            .as_object()
                            .is_some_and(|paths| !paths.is_empty())
                    {
                        let search_base = group[0]
                            .strip_suffix(CANDIDATES[0])
                            .expect("Discovery groups start with the first candidate");
                        let mut summary = summarize_at(&spec, search_base)?;
                        if summary["methodsByPath"]
                            .as_object()
                            .is_some_and(|methods| !methods.is_empty())
                        {
                            summary["specUrl"] = json!(url);
                            return Ok(summary);
                        }
                    }
                    debug.push(json!({"url":url,"outcome":"No OpenAPI or Swagger operations"}));
                }
                Err(error) => {
                    transport_only &= error.is_connect() || error.is_builder();
                    let status = error.status().map(|status| status.as_u16());
                    let failed = status.is_some_and(|status| ![404, 410].contains(&status))
                        || status.is_none() && !error.is_decode();
                    let error = Error::from(error);
                    debug.push(json!({"url":url,"status":status,"error":error.json()}));
                    if failed && (failure.is_none() || matches!(status, Some(401 | 403))) {
                        failure = Some(error);
                    }
                }
            }
        }
        if transport_only {
            break;
        }
    }
    let request = json!({"url":address,"headers":headers,"attempts":debug});
    if let Some(error) = failure {
        return Err(error.with_debug(request));
    }
    Ok(
        json!({"warning":"No OpenAPI or Swagger schema found at known discovery locations","debug":{"request":request}}),
    )
}
fn summarize_at(spec: &Value, search_base: &str) -> Result<Value> {
    let mut summary = summarize(spec);
    let base = url::Url::parse(&format!("{}/", search_base.trim_end_matches('/')))
        .map_err(|error| Error::new(format!("Invalid discovery base URL: {error}")))?;
    let mount = base.path().trim_end_matches('/');
    let absolute_paths = !mount.is_empty()
        && spec["paths"].as_object().is_some_and(|paths| {
            paths
                .keys()
                .any(|path| path == mount || path.starts_with(&format!("{mount}/")))
        });
    let scope = if absolute_paths {
        base.origin().ascii_serialization()
    } else {
        search_base.to_owned()
    };
    let base = if let Some(declared) = spec["basePath"]
        .as_str()
        .filter(|base| !base.is_empty())
        .or_else(|| spec["servers"][0]["url"].as_str())
    {
        base.join(declared)
            .map_err(|error| Error::new(format!("Invalid OpenAPI base URL: {error}")))?
    } else if absolute_paths {
        base.join("/")
            .map_err(|error| Error::new(format!("Invalid discovery origin: {error}")))?
    } else {
        base
    };
    let old = summary["basePath"].as_str().unwrap_or("").to_owned();
    let path = base.path().trim_end_matches('/').to_owned();
    let relocate = |value: &str| format!("{path}{}", value.strip_prefix(&old).unwrap_or(value));
    summary["paths"] = summary["paths"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|value| Value::String(relocate(value)))
        .collect();
    for field in ["methodsByPath", "bodySchemas"] {
        summary[field] = Value::Object(
            summary[field]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(key, value)| {
                    let key = if field == "bodySchemas" {
                        let (method, path) = key
                            .split_once(' ')
                            .expect("Body schema keys contain a method and path");
                        format!("{method} {}", relocate(path))
                    } else {
                        relocate(key)
                    };
                    (key, value.clone())
                })
                .collect(),
        );
    }
    summary["basePath"] = json!(path);
    summary["baseUrl"] = json!(scope);
    Ok(summary)
}
fn resolve<'a>(spec: &'a Value, mut schema: &'a Value) -> &'a Value {
    for _ in 0..12 {
        let Some(reference) = schema["$ref"].as_str().filter(|s| s.starts_with("#/")) else {
            break;
        };
        let Some(next) = spec.pointer(&reference[1..]) else {
            break;
        };
        schema = next;
    }
    schema
}
pub fn fields(spec: &Value, schema: &Value, depth: usize) -> Vec<Value> {
    if depth > 5 {
        return vec![];
    }
    let schema = resolve(spec, schema);
    schema["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, raw)| {
            let mut field = shape(spec, raw, depth);
            field["name"] = json!(name);
            if schema["required"]
                .as_array()
                .is_some_and(|required| required.iter().any(|value| value == name))
            {
                field["required"] = json!(true);
            }
            field
        })
        .collect()
}
fn shape(spec: &Value, schema: &Value, depth: usize) -> Value {
    let schema = resolve(spec, schema);
    let kind = match schema["type"].as_str() {
        Some("object") => "object",
        Some("array") => "array",
        Some("number" | "integer" | "boolean") => "bare",
        _ if schema.get("properties").is_some() => "object",
        _ if schema.get("items").is_some() => "array",
        _ => "string",
    };
    let mut field = json!({"kind":kind});
    if kind == "object" {
        field["fields"] = json!(fields(spec, schema, depth + 1));
    } else if kind == "array" && depth < 5 && schema["items"].is_object() {
        field["items"] = shape(spec, &schema["items"], depth + 1);
    }
    if let Some(values) = schema["enum"].as_array() {
        field["enum"] = json!(values);
    }
    if (kind == "bare" || field.get("enum").is_some())
        && let Some(data_type) = schema["type"].as_str()
    {
        field["type"] = json!(data_type);
    }
    if schema["readOnly"] == true {
        field["readOnly"] = json!(true);
    }
    if let Some(format) = schema["format"].as_str() {
        field["format"] = json!(format);
    }
    field
}
pub fn summarize(spec: &Value) -> Value {
    let base = if let Some(base) = spec["basePath"].as_str().filter(|s| !s.is_empty()) {
        base.to_string()
    } else if let Some(server) = spec["servers"][0]["url"].as_str() {
        url::Url::parse(server)
            .map(|u| u.path().to_string())
            .unwrap_or_else(|_| format!("/{}", server.trim_start_matches('/')))
    } else {
        String::new()
    };
    let base = base.trim_end_matches('/');
    let mut paths = vec![];
    let mut methods = Map::new();
    let mut headers: Vec<Value> = vec![];
    let mut bodies = Map::new();
    let mut add_header = |param: &Value| {
        if param["in"] == "header" && param["name"].is_string() && !headers.contains(&param["name"])
        {
            headers.push(param["name"].clone());
        }
    };
    for param in spec["parameters"].as_array().into_iter().flatten() {
        add_header(resolve(spec, param));
    }
    for (path, item) in spec["paths"].as_object().into_iter().flatten() {
        if !item.is_object() {
            continue;
        }
        let path = format!("{base}{path}");
        paths.push(path.clone());
        let mut names = vec![];
        for (method, op) in item.as_object().unwrap() {
            if ![
                "get", "put", "post", "delete", "options", "head", "patch", "trace",
            ]
            .contains(&method.as_str())
                || !op.is_object()
            {
                continue;
            }
            names.push(method.to_uppercase());
            for param in item["parameters"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(op["parameters"].as_array().into_iter().flatten())
            {
                add_header(resolve(spec, param));
            }
            let body = resolve(spec, &op["requestBody"]);
            let schema = if let Some(content) = body["content"].as_object() {
                content
                    .iter()
                    .find(|(k, _)| k.to_lowercase().contains("json"))
                    .or_else(|| content.iter().next())
                    .map(|(_, v)| &v["schema"])
            } else {
                op["parameters"]
                    .as_array()
                    .and_then(|p| p.iter().find(|v| v["in"] == "body"))
                    .map(|v| &v["schema"])
            };
            if let Some(schema) = schema {
                let fields = fields(spec, schema, 0);
                if !fields.is_empty() {
                    bodies.insert(format!("{} {path}", method.to_uppercase()), json!(fields));
                }
            }
        }
        if !names.is_empty() {
            methods.insert(path, json!(names));
        }
    }
    json!({"basePath":base,"paths":paths,"methodsByPath":methods,"headers":headers,"bodySchemas":bodies})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovered_mount_relocates_paths_methods_and_body_schemas_together() {
        let spec = json!({"openapi":"3.0.0","paths":{
            "/rest/v1/product":{"post":{"requestBody":{"content":{"application/json":{"schema":{
                "type":"object","required":["name"],"properties":{"name":{"type":"string"}}
            }}}}}}
        }});
        for site in ["site1", "site2"] {
            let summary = summarize_at(&spec, &format!("http://test.com/{site}")).unwrap();
            let path = format!("/{site}/rest/v1/product");
            assert_eq!(summary["baseUrl"], format!("http://test.com/{site}"));
            assert_eq!(summary["paths"], json!([path]));
            assert_eq!(summary["methodsByPath"][&path], json!(["POST"]));
            assert_eq!(
                summary["bodySchemas"][format!("POST {path}")][0]["name"],
                "name"
            );
        }
        let root = summarize_at(&spec, "http://test.com").unwrap();
        assert_eq!(root["baseUrl"], "http://test.com");
        assert_eq!(root["paths"], json!(["/rest/v1/product"]));
    }

    #[test]
    fn declared_bases_are_not_duplicated_and_relative_servers_use_the_mount() {
        for (declared, expected) in [
            ("/site1/rest/v1", "/site1/rest/v1"),
            ("http://test.com/site1/rest/v1", "/site1/rest/v1"),
            ("rest/v1", "/site1/rest/v1"),
        ] {
            let spec = json!({"openapi":"3.0.0","servers":[{"url":declared}],
                "paths":{"/product":{"get":{}}}});
            let summary = summarize_at(&spec, "http://test.com/site1").unwrap();
            assert_eq!(summary["baseUrl"], "http://test.com/site1");
            assert_eq!(summary["paths"], json!([format!("{expected}/product")]));
        }
        let spec = json!({"swagger":"2.0","basePath":"/rest/default",
            "paths":{"/V1/token":{"post":{}}}});
        let summary = summarize_at(&spec, "http://test.com/rest/default").unwrap();
        assert_eq!(summary["baseUrl"], "http://test.com/rest/default");
        assert_eq!(summary["paths"], json!(["/rest/default/V1/token"]));
    }

    #[test]
    fn paths_already_containing_the_mount_belong_to_the_origin_cache() {
        let spec = json!({"openapi":"3.0.0","paths":{
            "/site1/rest/v1/product":{"get":{}},
            "/site2/rest/v1/product":{"get":{}}
        }});
        for base in ["http://test.com", "http://test.com/site1"] {
            let summary = summarize_at(&spec, base).unwrap();
            assert_eq!(summary["baseUrl"], "http://test.com");
            assert_eq!(
                summary["paths"],
                json!(["/site1/rest/v1/product", "/site2/rest/v1/product"])
            );
        }
    }
}
