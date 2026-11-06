//! Discovery driven by module manifests: the `connection` sections hold what a
//! service discovers from, `console.discovery` says what it discovers.

use crate::profiles;
use serde_json::Value;
use services_contracts::manifest::{DiscoveryKind, Manifest, Shape};
use std::collections::{BTreeMap, BTreeSet};

/// The cursor is in a JSON section of a service that discovers per edited path.
pub fn body_position(service: &str, field: &str, source: &str, line: usize) -> bool {
    let Some(manifest) = crate::services::get(service) else {
        return false;
    };
    if !lazy(&manifest) {
        return false;
    }
    let json =
        |marker: &str| {
            manifest.console.sections.iter().any(|section| {
                section.language == "json" && section.name.eq_ignore_ascii_case(marker)
            })
        };
    if !field.is_empty() {
        return manifest
            .section(field)
            .is_some_and(|section| section.language == "json");
    }
    source
        .lines()
        .nth(line)
        .is_none_or(|line| !line.trim_start().starts_with('#'))
        && source
            .lines()
            .take(line)
            .filter_map(|line| line.trim().strip_prefix('#'))
            .last()
            .is_some_and(json)
}

fn lazy(manifest: &Manifest) -> bool {
    manifest
        .console
        .discovery
        .as_ref()
        .is_some_and(|discovery| discovery.kind.lazy())
}

/// The param of the `#endpoint` section: the address a service discovers from.
fn address_param(manifest: &Manifest) -> Option<&str> {
    manifest
        .console
        .discovery
        .as_ref()
        .and(manifest.section("endpoint"))?
        .param
        .as_deref()
}

/// The discovery address in `params` (or a discovery key) of `service`.
pub fn address(service: &str, params: &Value) -> String {
    crate::services::get(service)
        .and_then(|manifest| {
            address_param(&manifest).map(|param| params[param].as_str().unwrap_or("").to_owned())
        })
        .unwrap_or_default()
}

/// Sets the discovery address in `params` of `service`.
pub fn set_address(service: &str, params: &mut Value, address: &str) {
    if let Some(manifest) = crate::services::get(service)
        && let Some(param) = address_param(&manifest)
    {
        params[param] = Value::String(address.to_owned());
    }
}

/// Discovery params from the connection sections of a check; `None` without discovery.
pub fn params(service: &str, sections: &BTreeMap<String, String>) -> Option<Value> {
    let manifest = crate::services::get(service)?;
    manifest.console.discovery.as_ref()?;
    let mut params = serde_json::Map::new();
    for section in manifest
        .console
        .sections
        .iter()
        .filter(|section| section.connection)
    {
        let Some(param) = &section.param else {
            continue;
        };
        let text = sections.get(&section.name).map_or("", String::as_str);
        let value = if section.shape == Shape::Pairs {
            crate::console::pairs(text)
        } else if section.language == "http" {
            Value::String(text.split_whitespace().nth(1).unwrap_or("").to_owned())
        } else {
            Value::String(text.to_owned())
        };
        params.insert(param.clone(), value);
    }
    Some(Value::Object(params))
}

/// A new connection restarts discovery and drops the previous schema; SQL keeps its
/// schema until the next one arrives.
pub fn follows_connection(service: &str) -> bool {
    crate::services::discovery(service).is_some_and(|kind| kind != DiscoveryKind::Sql)
}

/// Schemas of a service that discovers per edited path, one per API base address.
/// Discovery runs once per address.
#[derive(Default)]
pub struct LazyCache {
    schemas: BTreeMap<String, Value>,
    attempted: BTreeSet<String>,
}
impl LazyCache {
    pub fn get(&self, address: &str) -> Option<&Value> {
        self.schemas
            .iter()
            .filter(|(base, schema)| reuses_schema(base, address, schema))
            .max_by_key(|(base, _)| base.len())
            .map(|(_, schema)| schema)
    }
    pub fn attempted(&self, address: &str) -> bool {
        self.attempted.contains(address)
    }
    pub fn attempt(&mut self, address: &str) {
        self.attempted.insert(address.to_owned());
    }
    pub fn insert(&mut self, address: &str, mut schema: Value) {
        let Some(base) = schema_base(address, &schema) else {
            return;
        };
        if let Some(schema) = schema.as_object_mut() {
            schema.remove("baseUrl");
        }
        self.schemas.insert(base, schema);
    }
}
fn schema_base(address: &str, schema: &Value) -> Option<String> {
    if schema["paths"].as_array()?.is_empty() {
        return None;
    }
    let mut url = url::Url::parse(address).ok()?;
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

/// The cursor is in a section discovery connects with.
pub fn connection_position(service: &str, field: &str, source: &str, line: usize) -> bool {
    let Some(manifest) = crate::services::get(service) else {
        return false;
    };
    if manifest.console.discovery.is_none() {
        return false;
    }
    if !field.is_empty() {
        return manifest
            .section(field)
            .is_some_and(|section| section.connection);
    }
    source
        .lines()
        .take(line + 1)
        .filter_map(|line| line.trim().strip_prefix('#'))
        .last()
        .is_some_and(|marker| {
            manifest
                .console
                .sections
                .iter()
                .any(|section| section.connection && section.name.eq_ignore_ascii_case(marker))
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
    let unsupported = || "This service does not support discovery".to_owned();
    let manifest = crate::services::get(service).ok_or_else(unsupported)?;
    let discovery = manifest
        .console
        .discovery
        .as_ref()
        .ok_or_else(unsupported)?;
    let field = address_param(&manifest).ok_or_else(unsupported)?;
    let params = if discovery.kind.lazy() {
        serde_json::json!({field: params[field]})
    } else {
        params.clone()
    };
    let mut key = resolve(&params, &profiles::hints(variables))?;
    let address = key[field].as_str().unwrap_or("").trim().to_owned();
    let (validation_address, expected) = if discovery.kind == DiscoveryKind::GrpcReflection {
        let target = discovery
            .schemes
            .iter()
            .find_map(|scheme| address.strip_prefix(&format!("{scheme}://")))
            .unwrap_or(&address);
        (format!("http://{target}"), vec!["http"])
    } else {
        (
            address.clone(),
            discovery.schemes.iter().map(String::as_str).collect(),
        )
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
    key[field] = if discovery.kind == DiscoveryKind::Openapi {
        let mut url = url::Url::parse(&address).map_err(|error| error.to_string())?;
        url.set_query(None);
        url.set_fragment(None);
        Value::String(url.to_string())
    } else {
        Value::String(address)
    };
    Ok(key)
}

/// A schema discovered at `previous` covers `next` within the same API base.
pub fn reuses_schema(previous: &str, next: &str, schema: &Value) -> bool {
    if schema["paths"]
        .as_array()
        .is_none_or(|paths| paths.is_empty())
    {
        return false;
    }
    let (Ok(previous_url), Ok(next_url)) = (url::Url::parse(previous), url::Url::parse(next))
    else {
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
