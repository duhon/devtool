//! The Dictionary panel: named sets of variables. Sources bring sets in `dictionary.json`
//! (`{"sets": {"<set>": {"<VAR>": "value"}}}`, the old `{"profiles": …}` too); the user's
//! edits of a set replace it whole and are kept in the module's kv until reset. It resolves
//! `{{set:NAME}}` of its sets and `{{:NAME}}` from the first set, in order, that has it.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use services_sdk::{Cmd, Event, Module, Resolution, Subscriptions, cmd, refs, ui};

/// The kv key of the user's sets: `{"order": [..], "sets": {"<set>": {..}}}`.
pub const LOCAL: &str = "sets";
/// The kv key of the edits that do not parse yet, by set.
pub const DRAFTS: &str = "drafts";
const SOURCES: &str = "sources";
const FILE: &str = "dictionary.json";
const TAB: &str = "dictionary-set-";

#[derive(Serialize, Deserialize)]
pub enum Msg {
    Tab(String),
    Edit,
    Add,
    Delete,
    Reset,
}

/// A set as the panel shows and resolves it.
struct Set {
    name: String,
    vars: Map<String, Value>,
    /// The source that brings the set, if any.
    source: Option<String>,
    /// The user changed (or created) it.
    own: bool,
}

#[derive(Default)]
pub struct Dictionary {
    /// The sets of the sources, lowest priority first: `(source, set, vars)`.
    sources: Vec<(String, String, Map<String, Value>)>,
    local: Map<String, Value>,
    order: Vec<String>,
    drafts: Map<String, Value>,
    /// What the user typed in a set since it was last loaded: the editor keeps it as typed.
    typed: Map<String, Value>,
    tab: String,
    sets: Vec<Set>,
}

impl Dictionary {
    /// Takes the sets of the sources (`[{source, text}]`) and the user's sets directly (tests).
    pub fn with(sources: Value, local: Value) -> Self {
        let mut dictionary = Self::default();
        dictionary.read_sources(&sources);
        dictionary.read_local(&local);
        dictionary
    }

    fn read_sources(&mut self, files: &Value) {
        self.sources.clear();
        for file in files.as_array().into_iter().flatten() {
            let source = file["source"].as_str().unwrap_or_default().to_owned();
            let Ok(document) = serde_json::from_str::<Value>(file["text"].as_str().unwrap_or("{}"))
            else {
                continue;
            };
            let sets = document
                .get("sets")
                .or_else(|| document.get("profiles"))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let mut names: Vec<String> = document["order"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|name| sets.contains_key(*name))
                .map(str::to_owned)
                .collect();
            let rest: Vec<String> = sets
                .keys()
                .filter(|name| !names.contains(name))
                .cloned()
                .collect();
            names.extend(rest);
            for name in names {
                let vars = sets[&name].as_object().cloned().unwrap_or_default();
                self.sources.push((source.clone(), name, vars));
            }
        }
        self.merge();
    }

    fn read_local(&mut self, value: &Value) {
        self.local = value["sets"].as_object().cloned().unwrap_or_default();
        self.order = value["order"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        self.merge();
    }

    /// The sets in order: the sources' first, then the user's own new ones.
    fn merge(&mut self) {
        let mut sets: Vec<Set> = Vec::new();
        for (source, name, vars) in &self.sources {
            match sets.iter_mut().find(|set| &set.name == name) {
                Some(set) => {
                    set.vars = vars.clone();
                    set.source = Some(source.clone());
                }
                None => sets.push(Set {
                    name: name.clone(),
                    vars: vars.clone(),
                    source: Some(source.clone()),
                    own: false,
                }),
            }
        }
        let mut names: Vec<String> = self
            .order
            .iter()
            .filter(|name| self.local.contains_key(*name))
            .cloned()
            .collect();
        let rest: Vec<String> = self
            .local
            .keys()
            .filter(|name| !names.contains(name))
            .cloned()
            .collect();
        names.extend(rest);
        for name in names {
            let vars = self.local[&name].as_object().cloned().unwrap_or_default();
            match sets.iter_mut().find(|set| set.name == name) {
                Some(set) => {
                    set.vars = vars;
                    set.own = true;
                }
                None => sets.push(Set {
                    name,
                    vars,
                    source: None,
                    own: true,
                }),
            }
        }
        self.sets = sets;
    }

    fn active(&self) -> Option<&Set> {
        self.sets
            .iter()
            .find(|set| set.name == self.tab)
            .or(self.sets.first())
    }

    fn text(&self, set: &Set) -> String {
        self.drafts
            .get(&set.name)
            .and_then(Value::as_str)
            .map_or_else(|| refs::format_lines(&set.vars), str::to_owned)
    }

    /// The editor's text of `set`.
    fn shown(&self, set: &Set) -> String {
        self.typed
            .get(&set.name)
            .and_then(Value::as_str)
            .map_or_else(|| self.text(set), str::to_owned)
    }

    fn load(&mut self) -> Vec<Cmd> {
        self.typed.clear();
        vec![
            cmd::kv_watch(LOCAL),
            cmd::sources_read(SOURCES, FILE),
            cmd::kv_get(LOCAL, LOCAL),
            cmd::kv_get(DRAFTS, DRAFTS),
        ]
    }

    fn contribution(&self) -> Cmd {
        let mut hints: Vec<(String, String)> = Vec::new();
        for set in &self.sets {
            for (name, value) in set.vars.iter().filter(|(name, _)| !name.starts_with('#')) {
                hints.push((format!("{{{{{}:{name}}}}}", set.name), refs::text(value)));
            }
        }
        for set in &self.sets {
            for (name, value) in set.vars.iter().filter(|(name, _)| !name.starts_with('#')) {
                let bare = format!("{{{{:{name}}}}}");
                if !hints.iter().any(|(key, _)| *key == bare) {
                    hints.push((bare, refs::text(value)));
                }
            }
        }
        cmd::contribute(&json!({ "variables": hints }))
    }

    fn store(&self, operation: &str) -> Vec<Cmd> {
        vec![
            cmd::kv_put(
                operation,
                LOCAL,
                &json!({"order": self.order, "sets": self.local}),
                false,
            ),
            self.contribution(),
        ]
    }

    fn put_drafts(&self) -> Cmd {
        if self.drafts.is_empty() {
            cmd::kv_delete("", DRAFTS)
        } else {
            cmd::kv_put("", DRAFTS, &Value::Object(self.drafts.clone()), false)
        }
    }

    fn edit(&mut self, text: String) -> Vec<Cmd> {
        let Some(name) = self.active().map(|set| set.name.clone()) else {
            return Vec::new();
        };
        self.typed.insert(name.clone(), json!(text));
        match refs::parse_lines(&text) {
            Ok(vars) => {
                self.drafts.remove(&name);
                self.local.insert(name.clone(), Value::Object(vars));
                if !self.order.contains(&name) {
                    self.order.push(name);
                }
                self.merge();
                let mut commands = self.store("save");
                commands.push(self.put_drafts());
                commands
            }
            Err(error) => {
                self.drafts.insert(name, json!(text));
                vec![
                    self.put_drafts(),
                    cmd::status(&format!("ERROR: {error}"), "error"),
                ]
            }
        }
    }

    fn add(&mut self, name: &str) -> Vec<Cmd> {
        let name = name.trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            || matches!(name, "var" | "env")
        {
            return vec![cmd::status(
                &format!("ERROR: Invalid set name \"{name}\": use letters, digits, _ and -."),
                "error",
            )];
        }
        if self.sets.iter().any(|set| set.name == name) {
            return vec![cmd::status(
                &format!("ERROR: Set \"{name}\" already exists."),
                "error",
            )];
        }
        self.local.insert(name.to_owned(), json!({}));
        self.order.push(name.to_owned());
        self.tab = name.to_owned();
        self.merge();
        self.store("add")
    }

    /// Forgets the user's version of the active set: deletes a set of the user's own,
    /// restores the source's version of an edited one.
    fn forget(&mut self, operation: &str) -> Vec<Cmd> {
        let Some(name) = self.active().map(|set| set.name.clone()) else {
            return Vec::new();
        };
        self.local.remove(&name);
        self.order.retain(|set| *set != name);
        self.drafts.remove(&name);
        self.typed.remove(&name);
        self.merge();
        let mut commands = self.store(operation);
        commands.push(self.put_drafts());
        commands
    }

    /// The value of `name` in `set` with the references the Dictionary owns resolved.
    fn own(
        &self,
        set: &Set,
        name: &str,
        chain: &mut Vec<String>,
    ) -> Result<Option<String>, String> {
        let Some(value) = set.vars.get(name) else {
            return Ok(None);
        };
        let link = format!("{}:{name}", set.name);
        if chain.contains(&link) {
            return Err(format!(
                "Circular variable reference: {} -> {link}",
                chain.join(" -> ")
            ));
        }
        chain.push(link);
        let out = refs::replace(&refs::text(value), |set, name| {
            self.reference(set, name, chain)
        });
        chain.pop();
        out.map(Some)
    }

    fn reference(
        &self,
        set: &str,
        name: &str,
        chain: &mut Vec<String>,
    ) -> Result<Option<String>, String> {
        if set.is_empty() {
            for owner in &self.sets {
                if let Some(value) = self.own(owner, name, chain)? {
                    return Ok(Some(value));
                }
            }
            return Ok(None);
        }
        match self.sets.iter().find(|owner| owner.name == set) {
            Some(owner) => match self.own(owner, name, chain)? {
                Some(value) => Ok(Some(value)),
                None => Err(format!("Unknown variable \"{name}\" in {set}")),
            },
            None => Ok(None),
        }
    }

    fn view_set(&self, set: &Set) -> Vec<ui::Node> {
        let origin = match (&set.source, set.own) {
            (Some(source), false) => format!("From source {source}"),
            (Some(source), true) => format!("Changed; from source {source}"),
            (None, _) => "Your own set".to_owned(),
        };
        let mut header = vec![
            ui::text(origin)
                .variant("muted")
                .key("dictionary-origin-label"),
        ];
        if set.own && set.source.is_some() {
            header.push(ui::button("Reset to source", &Msg::Reset).key("dictionary-reset"));
        }
        if set.source.is_none() {
            header.push(
                ui::button("Delete set", &Msg::Delete)
                    .variant("danger")
                    .key("dictionary-delete"),
            );
        }
        vec![
            ui::row(header).key("dictionary-origin"),
            ui::editor("headers", self.shown(set), &Msg::Edit).key("console-dictionary"),
        ]
    }
}

impl Module for Dictionary {
    type Msg = Msg;

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            workspace: true,
            ..Default::default()
        }
    }

    fn update(&mut self, event: Event<Msg>) -> Vec<Cmd> {
        match event {
            Event::Init { .. } | Event::Workspace | Event::Shown => self.load(),
            Event::Storage { key } if key == LOCAL => vec![cmd::kv_get(LOCAL, LOCAL)],
            Event::Reply { tag, result } => match (tag.as_str(), result) {
                (SOURCES, Ok(files)) => {
                    self.read_sources(&files);
                    vec![self.contribution()]
                }
                (LOCAL, Ok(local)) => {
                    self.read_local(&local);
                    // Text typed here stays unless another window saved the set differently.
                    let sets = &self.sets;
                    self.typed.retain(|name, text| {
                        match refs::parse_lines(text.as_str().unwrap_or_default()) {
                            Ok(vars) => {
                                sets.iter().any(|set| set.name == *name && set.vars == vars)
                            }
                            Err(_) => true,
                        }
                    });
                    vec![self.contribution()]
                }
                (DRAFTS, Ok(drafts)) => {
                    self.drafts = drafts.as_object().cloned().unwrap_or_default();
                    Vec::new()
                }
                ("save" | "add" | "delete" | "reset", Ok(_)) => {
                    let label = format!("Dictionary {tag} · OK");
                    vec![cmd::status(&label, "ok")]
                }
                (_, Err(failure)) => {
                    vec![cmd::status(&format!("ERROR: {}", failure.message), "error")]
                }
                _ => Vec::new(),
            },
            Event::Ui {
                msg: Msg::Tab(_),
                value: Some(tab),
            } => {
                self.tab = tab.trim_start_matches(TAB).to_owned();
                Vec::new()
            }
            Event::Ui {
                msg: Msg::Edit,
                value: Some(text),
            } => {
                let unchanged = self.active().is_some_and(|set| self.shown(set) == text);
                if unchanged {
                    Vec::new()
                } else {
                    self.edit(text)
                }
            }
            Event::Ui {
                msg: Msg::Add,
                value: Some(name),
            } => self.add(&name),
            Event::Ui {
                msg: Msg::Delete, ..
            } => self.forget("delete"),
            Event::Ui {
                msg: Msg::Reset, ..
            } => self.forget("reset"),
            _ => Vec::new(),
        }
    }

    fn view(&self) -> Option<ui::Node> {
        let mut body = Vec::new();
        match self.active() {
            Some(active) => {
                let tabs = self
                    .sets
                    .iter()
                    .map(|set| {
                        let body = if set.name == active.name {
                            self.view_set(set)
                        } else {
                            Vec::new()
                        };
                        ui::tab(format!("{TAB}{}", set.name), set.name.clone(), body)
                    })
                    .collect();
                body.push(
                    ui::tabs(
                        tabs,
                        format!("{TAB}{}", active.name),
                        &Msg::Tab(String::new()),
                    )
                    .key("dictionary-tabs"),
                );
            }
            None => body.push(ui::text("No sets yet: add one below.").variant("muted")),
        }
        let add = ui::input(None, "New set name", &Msg::Add).key("dictionary-add");
        Some(ui::panel("Dictionary", body, vec![add]))
    }

    fn resolve(&self, text: &str, json: bool, _last: bool) -> Option<Result<Resolution, String>> {
        let mut secrets = Vec::new();
        let result = refs::replace(text, |set, name| {
            let value = self.reference(set, name, &mut Vec::new())?;
            if let Some(value) = &value
                && refs::sensitive(name)
                && refs::find(value).is_empty()
            {
                secrets.push(value.clone());
            }
            Ok(value.map(|value| {
                if json {
                    refs::escape_json(&value)
                } else {
                    value
                }
            }))
        });
        Some(result.map(|text| Resolution { text, secrets }))
    }
}

services_sdk::export!(Dictionary);
