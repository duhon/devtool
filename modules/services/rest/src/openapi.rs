//! OpenAPI and Swagger specs summarized for completion: paths, methods per path, header
//! names and request body fields.

use serde_json::{Map, Value, json};
use services_sdk::lang::endpoint::Url;

/// Where discovery looks for a spec, under every prefix of the service path.
pub const CANDIDATES: [&str; 7] = [
    "/rest/default/schema?services=all",
    "/openapi.json",
    "/v3/api-docs",
    "/swagger.json",
    "/spec.json",
    "/swagger/v1/swagger.json",
    "/api-docs",
];

/// A Swagger 2 or OpenAPI 3 document with paths.
pub fn is_spec(spec: &Value) -> bool {
    (spec["swagger"] == "2.0"
        || spec["openapi"]
            .as_str()
            .is_some_and(|version| version.starts_with("3.")))
        && spec["paths"]
            .as_object()
            .is_some_and(|paths| !paths.is_empty())
}

/// A path as `Url::join` resolves it against a directory path ending with `/`.
fn join(directory: &str, reference: &str) -> String {
    if let Some(url) = Url::parse(reference) {
        return url.path.into();
    }
    let reference = reference.split(['?', '#']).next().unwrap_or("");
    if reference.starts_with('/') {
        reference.into()
    } else {
        format!("{directory}{reference}")
    }
}

/// The summary of a spec found under `search_base` (`scheme://host/prefix`): paths are
/// relocated under the mount the spec was found at unless they already contain it.
pub fn summarize_at(spec: &Value, search_base: &str) -> Result<Value, String> {
    let mut summary = summarize(spec);
    let base = Url::parse(search_base)
        .ok_or_else(|| format!("Invalid discovery base URL: {search_base}"))?;
    let directory = format!("{}/", base.path.trim_end_matches('/'));
    let mount = directory.trim_end_matches('/');
    let absolute_paths = !mount.is_empty()
        && spec["paths"].as_object().is_some_and(|paths| {
            paths
                .keys()
                .any(|path| path == mount || path.starts_with(&format!("{mount}/")))
        });
    let scope = if absolute_paths {
        base.origin()
    } else {
        search_base.trim_end_matches('/').to_owned()
    };
    let path = if let Some(declared) = spec["basePath"]
        .as_str()
        .filter(|base| !base.is_empty())
        .or_else(|| spec["servers"][0]["url"].as_str())
    {
        join(&directory, declared)
    } else if absolute_paths {
        "/".into()
    } else {
        directory.clone()
    };
    let path = path.trim_end_matches('/').to_owned();
    let old = summary["basePath"].as_str().unwrap_or("").to_owned();
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
                    let key = match key.split_once(' ') {
                        Some((method, path)) if field == "bodySchemas" => {
                            format!("{method} {}", relocate(path))
                        }
                        _ => relocate(key),
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

/// The fields of an object schema, nested at most 5 deep.
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

/// `{basePath, paths, methodsByPath, headers, bodySchemas}` of a spec; body schemas are
/// keyed `METHOD path`.
pub fn summarize(spec: &Value) -> Value {
    let base = if let Some(base) = spec["basePath"].as_str().filter(|s| !s.is_empty()) {
        base.to_string()
    } else if let Some(server) = spec["servers"][0]["url"].as_str() {
        Url::parse(server).map_or_else(
            || format!("/{}", server.trim_start_matches('/')),
            |url| url.path.to_owned(),
        )
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
        let Some(operations) = item.as_object() else {
            continue;
        };
        let path = format!("{base}{path}");
        paths.push(path.clone());
        let mut names = vec![];
        for (method, op) in operations {
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

    #[test]
    fn fields_preserve_date_formats_through_references() {
        let spec =
            json!({"components":{"schemas":{"Timestamp":{"type":"string","format":"date-time"}}}});
        let schema = json!({"properties":{
            "publishDate":{"$ref":"#/components/schemas/Timestamp"},
            "startDate":{"type":"string","format":"date"},
            "title":{"type":"string"}
        }});
        assert_eq!(
            fields(&spec, &schema, 0),
            vec![
                json!({"name":"publishDate","kind":"string","format":"date-time"}),
                json!({"name":"startDate","kind":"string","format":"date"}),
                json!({"name":"title","kind":"string"})
            ]
        );
    }

    #[test]
    fn enum_fields_preserve_values_types_references_and_array_items() {
        let spec = json!({"definitions":{"State":{"type":"string","enum":["pending","active","quote\"slash\\"]},
            "States":{"type":"array","items":{"$ref":"#/definitions/State"}}}});
        let schema = json!({"properties":{
            "state":{"$ref":"#/definitions/State"},
            "priority":{"type":"integer","enum":[1,2,3]},
            "enabled":{"type":"boolean","enum":[true,false]},
            "nullable":{"enum":["active",null]},
            "states":{"$ref":"#/definitions/States"},
            "nested":{"type":"object","properties":{"state":{"$ref":"#/definitions/State"}}}
        }});
        let fields = fields(&spec, &schema, 0);
        let field = |name: &str| fields.iter().find(|field| field["name"] == name).unwrap();
        assert_eq!(field("state")["enum"], spec["definitions"]["State"]["enum"]);
        assert_eq!(field("state")["type"], "string");
        assert_eq!(field("priority")["type"], "integer");
        assert_eq!(field("priority")["enum"], json!([1, 2, 3]));
        assert_eq!(field("enabled")["enum"], json!([true, false]));
        assert_eq!(field("nullable")["enum"], json!(["active", null]));
        assert_eq!(
            field("states")["items"]["enum"],
            spec["definitions"]["State"]["enum"]
        );
        assert_eq!(
            field("nested")["fields"][0]["enum"],
            spec["definitions"]["State"]["enum"]
        );
        assert!(field("states")["items"].get("name").is_none());
    }

    #[test]
    fn required_body_fields_preserve_types_and_nested_references() {
        for version in [json!({"swagger":"2.0"}), json!({"openapi":"3.0.3"})] {
            let mut spec = json!({
                "definitions":{"Body":{
                    "type":"object","required":["username","count","enabled","items","settings","id"],
                    "properties":{
                        "username":{"type":"string"},
                        "count":{"type":"integer"},
                        "enabled":{"type":"boolean"},
                        "items":{"type":"array","items":{"type":"string"}},
                        "settings":{"$ref":"#/definitions/Settings"},
                        "id":{"type":"integer","readOnly":true},
                        "optional":{"type":"string"}
                    }
                },"Settings":{"type":"object","required":["locale"],"properties":{
                    "locale":{"type":"string"},"optionalChild":{"type":"string"}
                }}},
                "paths":{"/token":{"post":{}}}
            });
            spec.as_object_mut()
                .unwrap()
                .extend(version.as_object().unwrap().clone());
            let reference = json!({"$ref":"#/definitions/Body"});
            if spec["swagger"] == "2.0" {
                spec["paths"]["/token"]["post"]["parameters"] =
                    json!([{"name":"body","in":"body","schema":reference}]);
            } else {
                spec["paths"]["/token"]["post"]["requestBody"] =
                    json!({"content":{"application/json":{"schema":reference}}});
            }
            let summary = summarize(&spec);
            let fields = summary["bodySchemas"]["POST /token"].as_array().unwrap();
            let field = |name: &str| fields.iter().find(|field| field["name"] == name).unwrap();
            assert_eq!(field("username")["required"], true);
            assert_eq!(field("count")["type"], "integer");
            assert_eq!(field("enabled")["type"], "boolean");
            assert_eq!(field("id")["readOnly"], true);
            assert!(field("optional").get("required").is_none());
            let nested = field("settings")["fields"].as_array().unwrap();
            let child = |name: &str| nested.iter().find(|field| field["name"] == name).unwrap();
            assert_eq!(child("locale")["required"], true);
            assert!(child("optionalChild").get("required").is_none());
        }
    }

    #[test]
    fn refs_swagger_parameters_and_cycles_are_bounded() {
        let spec = json!({"basePath":"/rest/","parameters":[{"name":"X-Global","in":"header"}],"definitions":{"Node":{"properties":{"id":{"type":"integer"},"child":{"$ref":"#/definitions/Node"}}}},"paths":{"/nodes":{"parameters":[{"in":"header","name":"X-Path"}],"post":{"parameters":[{"in":"body","schema":{"$ref":"#/definitions/Node"}}]}}}});
        let summary = summarize(&spec);
        assert_eq!(summary["paths"], json!(["/rest/nodes"]));
        assert_eq!(summary["headers"], json!(["X-Global", "X-Path"]));
        assert_eq!(
            summary["bodySchemas"]["POST /rest/nodes"][0]["kind"],
            "bare"
        );
        assert!(summary.to_string().len() < 4096);
    }
}
