//! The Variables panel: resolves the `{{profile:name}}` references of every request and of
//! `vars.resolve`. `{{:name}}` takes the first profile in order that has the name, then the
//! environment; `{{env:name}}` the environment only; a named profile its own value, whose
//! references resolve in turn. The profiles (the session `var`, the Dictionary, `env`) come
//! from the Host until the Dictionary is a module of its own.

use serde_json::{Map, Value};
use services_sdk::{Cmd, DriverCall, Event, Module, Resolution, cmd};
use std::collections::HashMap;

/// The kv key whose change means the profiles changed (a save, another session).
pub const PROFILES: &str = "profiles";
const LOAD: &str = "profiles";

/// Names whose values never show in history: they are masked wherever they appear.
pub fn sensitive_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "pwd",
        "token",
        "secret",
        "credential",
        "api_key",
        "apikey",
        "api-key",
        "authorization",
        "cookie",
        "private_key",
    ]
    .iter()
    .any(|part| name.contains(part))
}

/// The profiles references resolve from: `order` from the highest priority, the raw
/// `profiles` and the environment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profiles {
    pub order: Vec<String>,
    pub profiles: Map<String, Value>,
    pub env: Map<String, Value>,
}

impl Profiles {
    /// The Host's Variables `list` answer: its `context` holds every profile and `env`.
    pub fn from_list(value: &Value) -> Self {
        let context = value.get("context").unwrap_or(value);
        let mut profiles = context["profiles"].as_object().cloned().unwrap_or_default();
        let env = match profiles.remove("env") {
            Some(Value::Object(env)) => env,
            _ => Map::new(),
        };
        let order = context["order"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|name| *name != "env" && profiles.contains_key(*name))
            .map(str::to_owned)
            .collect();
        Self {
            order,
            profiles,
            env,
        }
    }
}

/// The profiles with every value resolved once, ready for each reference.
#[derive(Clone, Debug, Default)]
struct Prepared {
    profiles: Profiles,
    /// A highest-priority profile that does not resolve fails every resolution.
    error: Option<String>,
    /// Every name with the value it takes without a profile: the environment overridden
    /// by the profiles, the highest priority last.
    effective: Map<String, Value>,
    /// The profile each name of `effective` comes from.
    owners: HashMap<String, String>,
    resolved: HashMap<String, Map<String, Value>>,
    /// The sensitive values each `profile:name` takes in through its references.
    dependencies: HashMap<String, Vec<String>>,
}

fn string(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

/// Replaces each `{{profile:name}}` of `text` (profile `[A-Za-z0-9_-]*`, name
/// `[A-Za-z0-9_-]+`) with what `resolve` gives.
pub fn replace(
    text: &str,
    mut resolve: impl FnMut(&str, &str) -> Result<String, String>,
) -> Result<String, String> {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 2..];
        let reference = tail.find("}}").and_then(|end| {
            let (profile, name) = tail[..end].split_once(':')?;
            (profile.chars().all(word) && !name.is_empty() && name.chars().all(word))
                .then_some((profile, name, end))
        });
        match reference {
            Some((profile, name, end)) => {
                out.push_str(&resolve(profile, name)?);
                rest = &tail[end + 2..];
            }
            None => {
                out.push_str("{{");
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

impl Prepared {
    fn new(profiles: Profiles) -> Self {
        let mut prepared = Self {
            effective: profiles.env.clone(),
            profiles,
            ..Self::default()
        };
        let order = prepared.profiles.order.clone();
        for (index, profile) in order.iter().enumerate().rev() {
            let mut values = Map::new();
            let mut dependencies = Vec::new();
            let mut failed = None;
            let names: Vec<String> = prepared.profiles.profiles[profile]
                .as_object()
                .map(|vars| vars.keys().cloned().collect())
                .unwrap_or_default();
            for name in names {
                let mut sensitive = Vec::new();
                match prepared.named(profile, &name, &mut Vec::new(), &mut sensitive) {
                    Ok(value) => {
                        values.insert(name.clone(), value);
                        dependencies.push((format!("{profile}:{name}"), sensitive));
                    }
                    Err(error) => {
                        failed = Some(error);
                        break;
                    }
                }
            }
            match failed {
                Some(error) if index <= 1 => prepared.error = Some(error),
                Some(_) => {}
                None => {
                    for name in values.keys() {
                        prepared.owners.insert(name.clone(), profile.clone());
                    }
                    prepared.dependencies.extend(dependencies);
                    prepared.effective.extend(values.clone());
                    prepared.resolved.insert(profile.clone(), values);
                }
            }
        }
        prepared
    }

    /// The value of `name` in `profile`, its references resolved; the sensitive values it
    /// takes in are added to `sensitive`.
    fn named(
        &self,
        profile: &str,
        name: &str,
        chain: &mut Vec<String>,
        sensitive: &mut Vec<String>,
    ) -> Result<Value, String> {
        let start = sensitive.len();
        match self.named_value(profile, name, chain, sensitive) {
            Ok(value) => {
                if sensitive_name(name) {
                    sensitive.push(string(&value));
                }
                Ok(value)
            }
            Err(error) => {
                sensitive.truncate(start);
                Err(error)
            }
        }
    }

    fn circular(chain: &[String], key: &str) -> Result<(), String> {
        if chain.iter().any(|link| link == key) {
            return Err(format!(
                "Circular profile reference: {} -> {key}",
                chain.join(" -> ")
            ));
        }
        Ok(())
    }

    fn named_value(
        &self,
        profile: &str,
        name: &str,
        chain: &mut Vec<String>,
        sensitive: &mut Vec<String>,
    ) -> Result<Value, String> {
        let profiles = &self.profiles;
        if profile == "env" {
            return profiles
                .env
                .get(name)
                .cloned()
                .ok_or_else(|| format!("Unknown environment variable: {name}"));
        }
        if profile.is_empty() {
            let key = format!(":{name}");
            Self::circular(chain, &key)?;
            chain.push(key);
            for candidate in &profiles.order {
                if profiles.profiles[candidate].get(name).is_some()
                    && let Ok(value) = self.named(candidate, name, chain, sensitive)
                {
                    chain.pop();
                    return Ok(value);
                }
            }
            chain.pop();
            return profiles.env.get(name).cloned().ok_or_else(|| {
                format!(
                    "Unknown variable: {name} (not found in any profile or the real environment)"
                )
            });
        }
        let key = format!("{profile}:{name}");
        Self::circular(chain, &key)?;
        let vars = profiles
            .profiles
            .get(profile)
            .ok_or_else(|| format!("Unknown profile reference: {profile}"))?;
        let raw = vars
            .get(name)
            .ok_or_else(|| format!("Unknown variable \"{name}\" in profile \"{profile}\""))?;
        let Some(raw) = raw.as_str() else {
            return Ok(raw.clone());
        };
        chain.push(key);
        let out = replace(raw, |profile, name| {
            self.named(profile, name, chain, sensitive)
                .map(|value| string(&value))
        });
        chain.pop();
        out.map(Value::String)
    }

    /// `text` with its references resolved; in `json` mode each value is escaped as the
    /// content of a JSON string.
    fn resolve(&self, text: &str, json: bool) -> Result<Resolution, String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let mut cache: HashMap<String, String> = HashMap::new();
        let mut secrets = Vec::new();
        let text = replace(text, |profile, name| {
            let key = format!("{profile}:{name}");
            let value = match cache.get(&key) {
                Some(value) => value.clone(),
                None => {
                    let value = self.reference(profile, name, &mut secrets)?;
                    if sensitive_name(name) {
                        secrets.push(value.clone());
                    }
                    cache.insert(key, value.clone());
                    value
                }
            };
            Ok(if json {
                let quoted = Value::String(value).to_string();
                quoted[1..quoted.len() - 1].to_owned()
            } else {
                value
            })
        })?;
        secrets.retain(|secret| !secret.is_empty());
        secrets.sort();
        secrets.dedup();
        Ok(Resolution { text, secrets })
    }

    fn reference(
        &self,
        profile: &str,
        name: &str,
        secrets: &mut Vec<String>,
    ) -> Result<String, String> {
        let owner = if profile.is_empty() {
            self.owners.get(name).map(String::as_str)
        } else {
            Some(profile)
        };
        if let Some(values) =
            owner.and_then(|owner| self.dependencies.get(&format!("{owner}:{name}")))
        {
            secrets.extend(values.iter().cloned());
        }
        let missing = || format!("Missing environment variable(s): {name}");
        if profile.is_empty() {
            return self.effective.get(name).map(string).ok_or_else(missing);
        }
        if profile == "env" {
            return self.profiles.env.get(name).map(string).ok_or_else(missing);
        }
        if !self.profiles.profiles.contains_key(profile) {
            return Err(format!("Unknown profile: {profile}"));
        }
        if let Some(value) = self.resolved.get(profile).and_then(|vars| vars.get(name)) {
            return Ok(string(value));
        }
        self.named(profile, name, &mut Vec::new(), secrets)
            .map(|value| string(&value))
    }
}

/// Resolves every string in `value`.
fn resolve_strings(prepared: &Prepared, value: &Value) -> Result<Value, String> {
    Ok(match value {
        Value::String(text) => Value::String(prepared.resolve(text, false)?.text),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| resolve_strings(prepared, item))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, item)| Ok((key.clone(), resolve_strings(prepared, item)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

#[derive(Default)]
pub struct Variables {
    prepared: Prepared,
    /// Why the profiles could not be read; resolving fails with it.
    error: Option<String>,
}

impl Variables {
    /// Takes `profiles` directly (tests and a future Dictionary module).
    pub fn with(profiles: Profiles) -> Self {
        Self {
            prepared: Prepared::new(profiles),
            error: None,
        }
    }
    fn load() -> Cmd {
        cmd::vars_profiles(LOAD)
    }
}

impl Module for Variables {
    type Msg = ();

    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Init { .. } => vec![cmd::kv_watch(PROFILES), Self::load()],
            Event::Storage { key } if key == PROFILES => vec![Self::load()],
            Event::Reply { tag, result } if tag == LOAD => {
                match result {
                    Ok(value) => {
                        self.prepared = Prepared::new(Profiles::from_list(&value));
                        self.error = None;
                    }
                    Err(failure) => self.error = Some(failure.message),
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn before_request(&self, mut call: DriverCall) -> Result<DriverCall, String> {
        let request = call.request();
        let resolved = resolve_strings(&self.prepared, &request)?;
        if resolved != request {
            call.set_request(&resolved);
        }
        Ok(call)
    }

    fn resolve(&self, text: &str, json: bool) -> Option<Result<Resolution, String>> {
        if let Some(error) = &self.error
            && text.contains("{{")
        {
            return Some(Err(error.clone()));
        }
        Some(self.prepared.resolve(text, json))
    }
}

/// [`Profiles`] from JSON: `order` from the highest priority.
pub fn profiles(order: &[&str], profiles: Value, env: Value) -> Profiles {
    Profiles {
        order: order.iter().map(|name| (*name).to_owned()).collect(),
        profiles: profiles.as_object().cloned().unwrap_or_default(),
        env: env.as_object().cloned().unwrap_or_default(),
    }
}

services_sdk::export!(Variables);
