use super::error::{Error, Result, string, text};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use services_contracts::ParamConfig;
use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Clone)]
pub struct VariableStore {
    pub path: PathBuf,
    lock: Arc<Mutex<()>>,
    dictionary: bool,
}
pub type HistoryResolution = (Value, Result<(String, Vec<String>)>);
#[derive(Clone, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    version: u32,
    order: Vec<String>,
    profiles: Map<String, Value>,
}
fn refs() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{\{([A-Za-z0-9_-]*):([A-Za-z0-9_-]+)\}\}").unwrap())
}
impl VariableStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Arc::new(Mutex::new(())),
            dictionary: false,
        }
    }
    pub fn dictionary(&self) -> Self {
        Self {
            dictionary: true,
            ..self.clone()
        }
    }
    fn dictionary_path(&self) -> PathBuf {
        self.path.with_file_name(".dictionary-profiles.json")
    }
    fn file_lock(&self) -> Result<File> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.path.with_file_name(".profile-stores.lock"))?;
        file.lock()?;
        Ok(file)
    }
    fn read_file(path: &Path) -> Result<Option<State>> {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let v: Value = serde_json::from_str(&content)?;
        if v["version"].as_u64().is_some_and(|version| version > 2) {
            return Err(Error::new("Unsupported profile store version"));
        }
        let profiles = v["profiles"]
            .as_object()
            .ok_or_else(|| Error::new(format!("Invalid profile store: {}", path.display())))?;
        if profiles.values().any(|value| !value.is_object()) {
            return Err(Error::new(format!(
                "Profile values must be objects: {}",
                path.display()
            )));
        }
        let mut order: Vec<String> = v["order"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if !v["order"].is_array()
            && let Some(active) = v["activeProfile"].as_str()
            && profiles.contains_key(active)
        {
            order.push(active.to_string());
        }
        let mut seen = HashSet::new();
        order.retain(|s| profiles.contains_key(s) && s != "env" && seen.insert(s.clone()));
        for name in profiles.keys() {
            if name != "env" && !order.contains(name) {
                order.push(name.clone());
            }
        }
        let mut profiles = profiles.clone();
        profiles.remove("env");
        Ok(Some(State {
            version: v["version"].as_u64().unwrap_or(0) as u32,
            order,
            profiles,
        }))
    }
    fn write_file(path: &Path, state: &State) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // A sibling rename makes readers in other native connections see a complete state.
        let pending = path.with_extension(format!("json.{}.pending", std::process::id()));
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&pending)?;
        file.write_all(&serde_json::to_vec_pretty(state)?)?;
        file.sync_all()?;
        std::fs::rename(&pending, path)?;
        Ok(())
    }
    fn read(&self) -> Result<State> {
        let variables = Self::read_file(&self.path)?;
        let dictionary_path = self.dictionary_path();
        let mut dictionary = Self::read_file(&dictionary_path)?.unwrap_or(State {
            version: 2,
            order: vec![],
            profiles: Map::new(),
        });
        if dictionary.version != 2
            || dictionary
                .profiles
                .keys()
                .any(|name| name == "var" || name == "env")
        {
            return Err(Error::new(
                "Invalid Dictionary store: var and env belong to Variables",
            ));
        }
        let variables = match variables {
            Some(state) if state.version == 2 => {
                if state.profiles.len() != 1 || !state.profiles.contains_key("var") {
                    return Err(Error::new(
                        "Variables store must contain only the var profile",
                    ));
                }
                if !dictionary_path.exists() {
                    Self::write_file(&dictionary_path, &dictionary)?;
                }
                state
            }
            legacy => {
                if legacy.is_some() {
                    let backup = self
                        .path
                        .with_file_name(".variable-profiles.json.pre-split.bak");
                    match OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&backup)
                    {
                        Ok(mut file) => {
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                            }
                            file.write_all(&std::fs::read(&self.path)?)?;
                            file.sync_all()?;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            if std::fs::read(&backup)? != std::fs::read(&self.path)? {
                                return Err(Error::new(
                                    "Existing migration backup differs from the legacy Variables file; migration stopped without replacing either file",
                                ));
                            }
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                let mut vars = Map::new();
                vars.insert(
                    "var".into(),
                    legacy
                        .as_ref()
                        .and_then(|state| state.profiles.get("var"))
                        .cloned()
                        .unwrap_or(json!({})),
                );
                if let Some(legacy) = legacy {
                    for name in legacy.order.into_iter().filter(|name| name != "var") {
                        let value = legacy.profiles[&name].clone();
                        if let Some(existing) = dictionary.profiles.get(&name)
                            && existing != &value
                        {
                            return Err(Error::new(format!(
                                "Dictionary migration conflict for profile {name}; original files were preserved"
                            )));
                        }
                        dictionary.profiles.insert(name.clone(), value);
                        if !dictionary.order.contains(&name) {
                            dictionary.order.push(name);
                        }
                    }
                }
                let state = State {
                    version: 2,
                    order: vec!["var".into()],
                    profiles: vars,
                };
                Self::write_file(&dictionary_path, &dictionary)?;
                Self::write_file(&self.path, &state)?;
                state
            }
        };
        let mut merged = variables;
        merged.order.extend(dictionary.order);
        merged.profiles.extend(dictionary.profiles);
        Ok(merged)
    }
    fn named_tracking(
        state: &State,
        profile: &str,
        name: &str,
        chain: &mut Vec<String>,
        sensitive: &mut Vec<String>,
    ) -> Result<Value> {
        let start = sensitive.len();
        match Self::named_value(state, profile, name, chain, sensitive) {
            Ok(value) => {
                if crate::agent::Redactor::sensitive_name(name) {
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
    fn named_value(
        state: &State,
        profile: &str,
        name: &str,
        chain: &mut Vec<String>,
        sensitive: &mut Vec<String>,
    ) -> Result<Value> {
        if profile == "env" {
            return std::env::var(name)
                .map(Value::String)
                .map_err(|_| Error::new(format!("Unknown environment variable: {name}")));
        }
        if profile.is_empty() {
            let key = format!(":{name}");
            if chain.contains(&key) {
                return Err(Error::new(format!(
                    "Circular profile reference: {} -> {key}",
                    chain.join(" -> ")
                )));
            }
            chain.push(key);
            for profile in &state.order {
                if state.profiles[profile].get(name).is_some()
                    && let Ok(v) = Self::named_tracking(state, profile, name, chain, sensitive)
                {
                    chain.pop();
                    return Ok(v);
                }
            }
            chain.pop();
            return std::env::var(name).map(Value::String).map_err(|_| {
                Error::new(format!(
                    "Unknown variable: {name} (not found in any profile or the real environment)"
                ))
            });
        }
        let key = format!("{profile}:{name}");
        if chain.contains(&key) {
            return Err(Error::new(format!(
                "Circular profile reference: {} -> {key}",
                chain.join(" -> ")
            )));
        }
        let vars = state
            .profiles
            .get(profile)
            .ok_or_else(|| Error::new(format!("Unknown profile reference: {profile}")))?;
        let raw = vars.get(name).ok_or_else(|| {
            Error::new(format!(
                "Unknown variable \"{name}\" in profile \"{profile}\""
            ))
        })?;
        let Some(raw) = raw.as_str() else {
            return Ok(raw.clone());
        };
        chain.push(key);
        let out = replace(raw, |profile, name| {
            Self::named_tracking(state, profile, name, chain, sensitive).map(|v| string(&v))
        });
        chain.pop();
        out.map(Value::String)
    }
    fn profile(state: &State, name: &str, tolerant: bool) -> Result<Value> {
        Self::profile_tracking(state, name, tolerant, &mut std::collections::HashMap::new())
    }
    fn profile_tracking(
        state: &State,
        name: &str,
        tolerant: bool,
        dependencies: &mut std::collections::HashMap<String, Vec<String>>,
    ) -> Result<Value> {
        let mut out = Map::new();
        if let Some(vars) = state.profiles[name].as_object() {
            for key in vars.keys() {
                let mut sensitive = Vec::new();
                match Self::named_tracking(state, name, key, &mut vec![], &mut sensitive) {
                    Ok(v) => {
                        out.insert(key.clone(), v);
                        dependencies.insert(key.clone(), sensitive);
                    }
                    Err(e) if tolerant => {
                        out.insert(key.clone(), json!(format!("⚠ {}", e.message)));
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(Value::Object(out))
    }
    fn validation(state: &State) -> Value {
        let mut out = Map::new();
        for name in state.profiles.keys() {
            if let Err(e) = Self::profile(state, name, false) {
                out.insert(name.clone(), json!(e.message));
            }
        }
        Value::Object(out)
    }
    fn view(&self, state: &State) -> Value {
        let mut profiles = state.profiles.clone();
        let env: Map<String, Value> = std::env::vars().map(|(k, v)| (k, json!(v))).collect();
        profiles.insert("env".into(), json!(env));
        let mut resolved = Map::new();
        for name in state.profiles.keys() {
            resolved.insert(name.clone(), Self::profile(state, name, true).unwrap());
        }
        resolved.insert("env".into(), json!(env));
        json!({"order":state.order,"profiles":profiles,"resolvedProfiles":resolved,"validation":Self::validation(state)})
    }
    fn scoped(&self, state: &State) -> Value {
        let context = self.view(state);
        let belongs = |name: &str| {
            if self.dictionary {
                name != "var" && name != "env"
            } else {
                name == "var" || name == "env"
            }
        };
        let mut value = context.clone();
        for field in ["profiles", "resolvedProfiles", "validation"] {
            value[field]
                .as_object_mut()
                .unwrap()
                .retain(|name, _| belongs(name));
        }
        value["order"] = json!(
            state
                .order
                .iter()
                .filter(|name| belongs(name))
                .collect::<Vec<_>>()
        );
        value["context"] = context;
        value
    }
    pub fn list(&self) -> Result<Value> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| Error::new("Profile store lock poisoned"))?;
        let _file = self.file_lock()?;
        Ok(self.scoped(&self.read()?))
    }
    pub fn session_dispatch(&self, message: &Value, variables: &Value) -> Result<Value> {
        if !variables.is_object() {
            return Err(Error::new("Session variables must be an object"));
        }
        let variables = if self.dictionary {
            if message["op"] != "list" {
                self.dispatch(message)?;
            }
            variables
        } else {
            match message["op"].as_str().unwrap_or("list") {
                "list" => variables,
                "save" if message["name"] == "var" && message["variables"].is_object() => {
                    &message["variables"]
                }
                _ => {
                    return Err(Error::new(
                        "Session Variables supports only list and saving var",
                    ));
                }
            }
        };
        let _guard = self
            .lock
            .lock()
            .map_err(|_| Error::new("Variable store lock poisoned"))?;
        let _file = self.file_lock()?;
        let mut state = self.read()?;
        state.profiles.insert("var".into(), variables.clone());
        Ok(self.scoped(&state))
    }
    pub fn dispatch(&self, message: &Value) -> Result<Value> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| Error::new("Variable store lock poisoned"))?;
        let _file = self.file_lock()?;
        let op = text(message, "op");
        if op == "list" {
            return Ok(self.scoped(&self.read()?));
        }
        let mut state = self.read()?;
        let name = text(message, "name");
        if !self.dictionary && (op != "save" || name != "var") {
            return Err(Error::new(
                "Variables supports only saving var; var and env cannot be created, renamed, deleted or reordered",
            ));
        }
        if self.dictionary
            && (name == "var"
                || name == "env"
                || message["newName"] == "var"
                || message["newName"] == "env")
        {
            return Err(Error::new(
                "var and env are reserved for the Variables service",
            ));
        }
        state.order.retain(|name| {
            if self.dictionary {
                name != "var"
            } else {
                name == "var"
            }
        });
        state.profiles.retain(|name, _| {
            if self.dictionary {
                name != "var"
            } else {
                name == "var"
            }
        });
        if ["save", "delete", "rename"].contains(&op.as_str()) && name == "env" {
            return Err(Error::new(
                "\"env\" is reserved for the real environment and can't be used as a stored profile name",
            ));
        }
        match op.as_str() {
            "save" => {
                if name.trim().is_empty() {
                    return Err(Error::new("Profile name is required"));
                }
                let vars = message.get("variables").cloned().unwrap_or(json!({}));
                if !vars.is_object() {
                    return Err(Error::new("variables must be an object"));
                }
                state.profiles.insert(name.clone(), vars);
                if !state.order.contains(&name) {
                    state.order.push(name);
                }
            }
            "delete" => {
                if !state.profiles.contains_key(&name) {
                    return Err(Error::new(format!("Unknown profile: {name}")));
                }
                state.profiles.remove(&name);
                state.order.retain(|n| n != &name);
            }
            "reorder" => {
                let order: Vec<String> = message["order"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let distinct: HashSet<_> = order.iter().collect();
                if order.len() != state.profiles.len()
                    || distinct.len() != order.len()
                    || order.iter().any(|n| !state.profiles.contains_key(n))
                {
                    return Err(Error::new(
                        "Order must contain exactly the current set of profile names, each once",
                    ));
                }
                state.order = order;
            }
            "rename" => {
                let new = text(message, "newName").trim().to_string();
                if new.is_empty() {
                    return Err(Error::new("Profile name is required"));
                }
                if new == "env" {
                    return Err(Error::new(
                        "\"env\" is reserved for the real environment and can't be used as a stored profile name",
                    ));
                }
                if !state.profiles.contains_key(&name) {
                    return Err(Error::new(format!("Unknown profile: {name}")));
                }
                if new != name && state.profiles.contains_key(&new) {
                    return Err(Error::new(format!(
                        "A profile named \"{new}\" already exists"
                    )));
                }
                state.profiles = state
                    .profiles
                    .into_iter()
                    .map(|(k, v)| (if k == name { new.clone() } else { k }, v))
                    .collect();
                for n in &mut state.order {
                    if n == &name {
                        *n = new.clone();
                    }
                }
            }
            _ => return Err(Error::new(format!("Unknown vars op: {op}"))),
        }
        let path = if self.dictionary {
            self.dictionary_path()
        } else {
            self.path.clone()
        };
        Self::write_file(&path, &state)?;
        Ok(self.scoped(&self.read()?))
    }
    /// Resolves the declared `config` params of a request; undeclared ones are dropped.
    pub fn resolve_params(&self, config: &[ParamConfig], params: &Value) -> Result<Value> {
        self.resolve_for_history(config, params, None)
            .map(|value| value.0)
    }
    pub fn resolve_for_history(
        &self,
        config: &[ParamConfig],
        params: &Value,
        source: Option<&str>,
    ) -> Result<HistoryResolution> {
        use std::cell::RefCell;
        let _file = self.file_lock()?;
        let mut state = self.read()?;
        if let Some(variables) = params.get("__consoleVariables") {
            if !variables.is_object() {
                return Err(Error::new("Session variables must be an object"));
            }
            state.profiles.insert("var".into(), variables.clone());
        }
        // Match the original eager highest-priority validation, even if the request uses no variables.
        let mut effective: Map<String, Value> =
            std::env::vars().map(|(k, v)| (k, json!(v))).collect();
        let environment = effective.clone();
        let mut resolved_profiles = Map::new();
        let mut dependencies = std::collections::HashMap::new();
        let mut effective_profiles = std::collections::HashMap::new();
        for (index, profile) in state.order.iter().enumerate().rev() {
            let mut profile_dependencies = std::collections::HashMap::new();
            match Self::profile_tracking(&state, profile, false, &mut profile_dependencies) {
                Ok(Value::Object(vars)) => {
                    for name in vars.keys() {
                        effective_profiles.insert(name.clone(), profile.clone());
                    }
                    for (name, values) in profile_dependencies {
                        dependencies.insert(format!("{profile}:{name}"), values);
                    }
                    resolved_profiles.insert(profile.clone(), json!(vars));
                    effective.extend(vars);
                }
                Err(e) if index <= 1 => return Err(e),
                _ => {}
            }
        }
        let cache = RefCell::new(std::collections::HashMap::<String, String>::new());
        let sensitive = RefCell::new(Vec::new());
        let resolve = |raw: &Value| -> Result<Value> {
            let Some(s) = raw.as_str() else {
                return Ok(raw.clone());
            };
            replace(s, |profile, var| {
                let key = format!("{profile}:{var}");
                if let Some(value) = cache.borrow().get(&key) {
                    return Ok(value.clone());
                }
                let dependency_profile = if profile.is_empty() {
                    effective_profiles.get(var).map(String::as_str)
                } else {
                    Some(profile)
                };
                if let Some(values) = dependency_profile
                    .and_then(|profile| dependencies.get(&format!("{profile}:{var}")))
                {
                    sensitive.borrow_mut().extend(values.iter().cloned());
                }
                let value = if profile.is_empty() {
                    effective.get(var).map(string).ok_or_else(|| {
                        Error::new(format!("Missing environment variable(s): {var}"))
                    })
                } else if profile == "env" {
                    environment.get(var).map(string).ok_or_else(|| {
                        Error::new(format!("Missing environment variable(s): {var}"))
                    })
                } else {
                    if !state.profiles.contains_key(profile) {
                        return Err(Error::new(format!("Unknown profile: {profile}")));
                    }
                    if let Some(value) = resolved_profiles.get(profile).and_then(|p| p.get(var)) {
                        Ok(string(value))
                    } else {
                        Self::named_tracking(
                            &state,
                            profile,
                            var,
                            &mut vec![],
                            &mut sensitive.borrow_mut(),
                        )
                        .map(|v| string(&v))
                    }
                }?;
                if crate::agent::Redactor::sensitive_name(var) {
                    sensitive.borrow_mut().push(value.clone());
                }
                cache.borrow_mut().insert(key, value.clone());
                Ok(value)
            })
            .map(Value::String)
        };
        let mut out = Map::new();
        for p in config {
            let Some(raw) = params.get(&p.name) else {
                continue;
            };
            let mode =
                p.resolve
                    .as_deref()
                    .unwrap_or(if p.kind == "json" { "object" } else { "text" });
            let v = match mode {
                "none" => raw.clone(),
                "text" => resolve_value(raw, &resolve)?,
                "object" => {
                    if let Some(text) = raw.as_str().filter(|text| !text.is_empty()) {
                        let value: Value = serde_json::from_str(text).map_err(|error| {
                            Error::new(format!("{} must be valid JSON: {error}", p.name))
                        })?;
                        Value::String(serde_json::to_string(&resolve_value(&value, &resolve)?)?)
                    } else {
                        resolve_value(raw, &resolve)?
                    }
                }
                _ => {
                    return Err(Error::new(format!(
                        "Unknown resolve mode \"{mode}\" for param \"{}\"",
                        p.name
                    )));
                }
            };
            out.insert(p.name.clone(), v);
        }
        let snapshot = resolve(&json!(source.unwrap_or("")))
            .map(|source| (string(&source), sensitive.into_inner()));
        Ok((Value::Object(out), snapshot))
    }
}
fn resolve_value(value: &Value, resolve: &impl Fn(&Value) -> Result<Value>) -> Result<Value> {
    match value {
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| resolve_value(value, resolve).map(|value| (key.clone(), value)))
            .collect::<Result<Map<_, _>>>()
            .map(Value::Object),
        Value::Array(values) => values
            .iter()
            .map(|value| resolve_value(value, resolve))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        _ => resolve(value),
    }
}
fn replace(input: &str, mut resolve: impl FnMut(&str, &str) -> Result<String>) -> Result<String> {
    let mut out = String::new();
    let mut previous = 0;
    for cap in refs().captures_iter(input) {
        let m = cap.get(0).unwrap();
        out.push_str(&input[previous..m.start()]);
        out.push_str(&resolve(&cap[1], &cap[2])?);
        previous = m.end();
    }
    out.push_str(&input[previous..]);
    Ok(out)
}
