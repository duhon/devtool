use crate::profiles;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub fn body_position(service: &str, field: &str, source: &str, line: usize) -> bool {
    service == "rest"
        && (field == "body"
            || field.is_empty()
                && source
                    .lines()
                    .nth(line)
                    .is_none_or(|line| !line.trim_start().starts_with('#'))
                && source
                    .lines()
                    .take(line)
                    .filter_map(|line| line.trim().strip_prefix('#'))
                    .last()
                    .is_some_and(|marker| marker.eq_ignore_ascii_case("body")))
}

#[derive(Default)]
pub struct RestCache {
    schemas: BTreeMap<String, Value>,
    attempted: BTreeSet<String>,
}
impl RestCache {
    pub fn get(&self, key: &Value) -> Option<&Value> {
        self.schemas
            .iter()
            .filter(|(base, schema)| {
                reuses_rest_schema(&serde_json::json!({"url":base}), key, schema)
            })
            .max_by_key(|(base, _)| base.len())
            .map(|(_, schema)| schema)
    }
    pub fn attempted(&self, key: &Value) -> bool {
        key["url"]
            .as_str()
            .is_some_and(|url| self.attempted.contains(url))
    }
    pub fn attempt(&mut self, key: Value) {
        if let Some(url) = key["url"].as_str() {
            self.attempted.insert(url.to_owned());
        }
    }
    pub fn insert(&mut self, key: &Value, mut schema: Value) {
        let Some(base) = rest_base(key, &schema) else {
            return;
        };
        if let Some(schema) = schema.as_object_mut() {
            schema.remove("baseUrl");
        }
        self.schemas.insert(base, schema);
    }
}
fn rest_base(key: &Value, schema: &Value) -> Option<String> {
    if schema["paths"].as_array()?.is_empty() {
        return None;
    }
    let mut url = url::Url::parse(key["url"].as_str()?).ok()?;
    if let Some(base) = schema["baseUrl"].as_str() {
        let base = url::Url::parse(base).ok()?;
        if base.origin() != url.origin() {
            return None;
        }
        url.set_path(base.path());
    } else {
        url.set_path(schema["basePath"].as_str()?);
    }
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

pub fn connection_position(service: &str, field: &str, source: &str, line: usize) -> bool {
    if !["rest", "graphql", "grpc", "mysql"].contains(&service) {
        return false;
    }
    if !field.is_empty() {
        return matches!(
            (service, field),
            ("rest" | "graphql", "url" | "headersText")
                | ("grpc", "endpointText" | "target" | "headersText")
                | ("mysql", "dsn")
        );
    }
    source
        .lines()
        .take(line + 1)
        .filter_map(|line| line.trim().strip_prefix('#'))
        .last()
        .is_some_and(|marker| {
            marker.eq_ignore_ascii_case("endpoint")
                || service != "mysql" && marker.eq_ignore_ascii_case("headers")
        })
}

pub fn key(service: &str, params: &Value, variables: &Value) -> Result<Value, String> {
    fn resolve(value: &Value, hints: &[(String, String)]) -> Result<Value, String> {
        match value {
            Value::String(text) => profiles::resolve_text(text, hints).map(Value::String),
            Value::Object(values) => values
                .iter()
                .map(|(name, value)| Ok((name.clone(), resolve(value, hints)?)))
                .collect::<Result<serde_json::Map<_, _>, String>>()
                .map(Value::Object),
            Value::Array(values) => values
                .iter()
                .map(|value| resolve(value, hints))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            value => Ok(value.clone()),
        }
    }
    let params = if service == "rest" {
        serde_json::json!({"url":params["url"]})
    } else {
        params.clone()
    };
    let mut key = resolve(&params, &profiles::hints(variables))?;
    let field = match service {
        "rest" | "graphql" => "url",
        "grpc" => "endpoint",
        "mysql" => "connectionString",
        _ => return Err("This service does not support discovery".into()),
    };
    let address = key[field].as_str().unwrap_or("").trim().to_owned();
    let validation_address = if service == "grpc" {
        let target = address
            .strip_prefix("grpc://")
            .or_else(|| address.strip_prefix("grpcs://"))
            .unwrap_or(&address);
        format!("http://{target}")
    } else {
        address.clone()
    };
    let expected = if service == "mysql" {
        &["mysql"][..]
    } else {
        &["http", "https"][..]
    };
    let valid = url::Url::parse(&validation_address).is_ok_and(|url| {
        expected.contains(&url.scheme())
            && url.host_str().is_some_and(|host| !host.is_empty())
            && validation_address
                .split_once("://")
                .is_some_and(|(scheme, rest)| {
                    scheme.eq_ignore_ascii_case(url.scheme())
                        && rest.split('/').next().is_some_and(|authority| {
                            !authority.is_empty() && !authority.ends_with(':')
                        })
                })
    }) && !validation_address.contains(char::is_whitespace);
    if !valid {
        return Err("Invalid or incomplete service address for discovery".into());
    }
    key[field] = if service == "rest" {
        let mut url = url::Url::parse(&address).map_err(|error| error.to_string())?;
        url.set_query(None);
        url.set_fragment(None);
        Value::String(url.to_string())
    } else {
        Value::String(address)
    };
    Ok(key)
}

pub fn reuses_rest_schema(previous: &Value, next: &Value, schema: &Value) -> bool {
    if schema["paths"]
        .as_array()
        .is_none_or(|paths| paths.is_empty())
    {
        return false;
    }
    let (Ok(previous_url), Ok(next_url)) = (
        url::Url::parse(previous["url"].as_str().unwrap_or("")),
        url::Url::parse(next["url"].as_str().unwrap_or("")),
    ) else {
        return false;
    };
    let base = previous_url.path().trim_end_matches('/');
    if previous_url.origin() != next_url.origin()
        || previous_url.username() != next_url.username()
        || previous_url.password() != next_url.password()
        || !(next_url.path() == base || next_url.path().starts_with(&format!("{base}/")))
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rest_body_discovery_only_belongs_to_body_content() {
        let source = "#check c\n#endpoint\nPOST http://test.com/site1/rest/v1/product\n#headers\nX-Token: value\n#body\n\n#check next\n#endpoint\nGET http://other.com/";
        for line in 0..source.lines().count() {
            assert_eq!(body_position("rest", "", source, line), line == 6);
        }
        assert!(body_position("rest", "body", "", 0));
        assert!(!body_position("rest", "url", "", 0));
        assert!(!body_position("graphql", "body", "", 0));
    }
    fn rest_key(url: &str, token: &str) -> Value {
        key(
            "rest",
            &json!({"url":url,"headers":{"X-Token":token}}),
            &json!({}),
        )
        .unwrap()
    }
    #[test]
    fn rest_cache_retains_independent_sites_and_ignores_headers() {
        let mut cache = RestCache::default();
        for (site, token) in [("site1", "a"), ("site2", "a")] {
            let base = format!("http://test.com/{site}/");
            cache.insert(
                &rest_key(&format!("{base}rest/v1/product"), token),
                json!({
                    "baseUrl":base,"paths":[format!("/{site}/rest/v1/product")],"token":token
                }),
            );
        }
        for (site, token) in [("site1", "a"), ("site2", "a"), ("site1", "b")] {
            let schema = cache
                .get(&rest_key(
                    &format!("http://test.com/{site}/rest/v1/other"),
                    token,
                ))
                .unwrap();
            assert!(schema.get("baseUrl").is_none());
            assert!(
                cache
                    .schemas
                    .contains_key(&format!("http://test.com/{site}/"))
            );
        }
        for url in [
            "http://test.com/site10/rest/v1/product",
            "http://other.com/site1/rest/v1/product",
            "https://test.com/site1/rest/v1/product",
        ] {
            assert!(cache.get(&rest_key(url, "a")).is_none());
        }
        assert!(
            cache
                .get(&rest_key("http://test.com/site2/rest/v1/product", "b"))
                .is_some()
        );
        assert_eq!(
            rest_key("http://test.com/site1/", "a"),
            rest_key("http://test.com/site1/", "b")
        );
    }
    #[test]
    fn rest_cache_reuses_root_paths_and_prefers_specific_bases() {
        let mut cache = RestCache::default();
        cache.insert(
            &rest_key("http://test.com/", ""),
            json!({
                "baseUrl":"http://test.com/","paths":["/site1/rest/v1/product"],"name":"root"
            }),
        );
        cache.insert(
            &rest_key("http://test.com/site1/rest/v1/product", ""),
            json!({
                "baseUrl":"http://test.com/site1/","paths":["/site1/rest/v1/product"],"name":"site"
            }),
        );
        assert_eq!(
            cache
                .get(&rest_key("http://test.com/site1/rest/v1/product", ""))
                .unwrap()["name"],
            "site"
        );
        assert_eq!(
            cache
                .get(&rest_key("http://test.com/site2/rest/v1/product", ""))
                .unwrap()["name"],
            "root"
        );
    }
    #[test]
    fn failed_rest_prefix_does_not_block_the_next_prefix_or_create_a_schema() {
        let mut cache = RestCache::default();
        let root = rest_key("http://test.com/", "");
        cache.attempt(root.clone());
        assert!(cache.attempted(&root));
        assert!(!cache.attempted(&rest_key("http://test.com/site1/", "")));
        assert!(cache.get(&root).is_none());
        cache.insert(&root, json!({"warning":"No schema"}));
        assert!(cache.get(&root).is_none());
    }
    #[test]
    fn rest_cache_canonicalizes_query_fragment_and_resolved_variable_urls() {
        let variables =
            json!({"profiles":{"var":{"host":"test.com","site":"site1"}},"order":["var"]});
        let resolved = key("rest", &json!({"url":"http://{{var:host}}/{{var:site}}/rest/v1/product?x=1#part","headers":{"X-Token":""}}), &variables).unwrap();
        assert_eq!(
            resolved,
            rest_key("http://test.com/site1/rest/v1/product", "")
        );
        assert_eq!(
            rest_key("http://TEST.com:80", ""),
            rest_key("http://test.com/", "")
        );
    }
    #[test]
    fn discovery_commits_belong_to_connection_sections_not_body_input() {
        let source = "#check c\n#endpoint\nhttps://fixture.invalid/graphql\n#headers\nAuthorization: token\n#query\n{\n  field\n}";
        for line in 0..source.lines().count() {
            assert_eq!(
                connection_position("graphql", "", source, line),
                (1..=4).contains(&line),
                "{line}"
            );
        }
        assert!(connection_position("graphql", "url", "", 0));
        assert!(connection_position("graphql", "headersText", "", 0));
        assert!(connection_position("grpc", "endpointText", "", 0));
        assert!(connection_position("mysql", "dsn", "", 0));
        assert!(!connection_position("graphql", "query", "", 0));
        assert!(!connection_position("grpc", "requestMessage", "", 0));
        assert!(!connection_position("rest", "body", "", 0));
        assert!(!connection_position("terminal", "command", "", 0));
    }
    #[test]
    fn incomplete_addresses_are_not_ready_for_discovery() {
        for service in ["rest", "graphql", "grpc", "mysql"] {
            let field = match service {
                "grpc" => "endpoint",
                "mysql" => "connectionString",
                _ => "url",
            };
            let valid: &[&str] = match service {
                "grpc" => &[
                    "localhost:50051/",
                    "grpc://localhost:50051/Service/",
                    "grpcs://localhost:443/Service/Method",
                    "[::1]:50051/Service/Method",
                ],
                "mysql" => &[
                    "mysql://localhost/database",
                    "mysql://user:password@localhost:3306/database",
                ],
                _ => &[
                    "https://fixture.invalid/graphql",
                    "HTTPS://fixture.invalid/graphql",
                    "http://localhost:3000/",
                    "http://[::1]:3000/",
                ],
            };
            for address in valid {
                assert!(
                    key(service, &json!({field:address}), &Value::Null).is_ok(),
                    "{service}: {address}"
                );
            }
            for address in [
                "",
                "https://",
                "http://",
                "grpc://",
                "grpcs://",
                "mysql://",
                "https:///",
                "https://host:",
                "https://host:abc",
                "https://bad host/",
                "http:/host",
                "hhttp://fake.com",
                "{{:unfinished",
                "https://{{:unknown}}/",
            ] {
                assert!(
                    key(service, &json!({field:address}), &Value::Null).is_err(),
                    "{service}: {address}"
                );
            }
        }
    }

    #[test]
    fn rest_schema_is_reused_within_its_api_regardless_of_headers() {
        let previous =
            json!({"url":"https://fixture.invalid/api/v3/","headers":{"X-Token":"first"}});
        let schema = json!({"basePath":"/api/v3","paths":["/api/v3/pets"]});
        for path in ["/api/v3", "/api/v3/pets", "/api/v3/users?limit=2"] {
            let next = json!({"url":format!("https://fixture.invalid{path}"),"headers":{"X-Token":"first"}});
            assert!(reuses_rest_schema(&previous, &next, &schema), "{path}");
        }
        for next in [
            json!({"url":"https://fixture.invalid/api/v30/pets","headers":{"X-Token":"first"}}),
            json!({"url":"https://fixture.invalid/other-api/pets","headers":{"X-Token":"first"}}),
            json!({"url":"https://other.invalid/api/v3/pets","headers":{"X-Token":"first"}}),
        ] {
            assert!(!reuses_rest_schema(&previous, &next, &schema), "{next}");
        }
        assert!(reuses_rest_schema(
            &previous,
            &json!({"url":"https://fixture.invalid/api/v3/pets","headers":{"X-Token":"second"}}),
            &schema
        ));
        assert!(!reuses_rest_schema(
            &previous,
            &previous,
            &json!({"warning":"No schema"})
        ));
        assert!(!reuses_rest_schema(
            &previous,
            &previous,
            &json!({"basePath":"/api/v3","paths":[]})
        ));
        assert!(reuses_rest_schema(
            &previous,
            &previous,
            &json!({"basePath":"","paths":["/api/v3/pets"]})
        ));
    }

    #[test]
    fn discovery_keys_follow_resolved_connection_variables() {
        let mut variables = json!({"order":["catalog"],"profiles":{"catalog":{"HOST":"fixture.invalid","TOKEN":"first"}}});
        let params =
            json!({"url":"https://{{catalog:HOST}}/graphql","headers":{"X-Token":"{{:TOKEN}}"}});
        let first = key("graphql", &params, &variables).unwrap();
        assert_eq!(
            first,
            json!({"url":"https://fixture.invalid/graphql","headers":{"X-Token":"first"}})
        );
        assert_eq!(key("graphql", &json!({"url":" https://{{catalog:HOST}}/graphql ","headers":{"X-Token":"{{:TOKEN}}"}}), &variables).unwrap(), first);
        variables["profiles"]["catalog"]["TOKEN"] = json!("second");
        assert_ne!(key("graphql", &params, &variables).unwrap(), first);
        variables["profiles"]["catalog"]["HOST"] = json!("");
        assert!(key("graphql", &params, &variables).is_err());
        assert!(
            key(
                "graphql",
                &json!({"url":"https://fixture.invalid","headers":{"X-Token":"{{:unfinished"}}),
                &variables
            )
            .is_err()
        );
        let variables = json!({"order":["catalog"],"profiles":{"catalog":{"URL":"{{other:URL}}"}},"resolvedProfiles":{"catalog":{"URL":"https://fixture.invalid/graphql"}}});
        assert!(key("graphql", &json!({"url":"{{:URL}}"}), &variables).is_ok());
    }
}
