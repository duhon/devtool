//! The Dictionary panel: named sets of variables. Sources bring sets in `dictionary.json`
//! (`{"sets": {"<set>": {"<VAR>": "value"}}}`, the old `{"profiles": …}` too); the user's
//! layer over them is kept in the module's kv: sets of the user's own whole, of a source's
//! set only the variables the user changed (reset brings back the source's value). It
//! resolves `{{set:NAME}}` of its sets and `{{:NAME}}` from the first set, in order, that
//! has it.
//!
//! Every window of the App edits the same layer, so changes go through a transaction under
//! a shared lock on the freshly read layer; a draft started before another window saved
//! the set conflicts instead of overwriting it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use services_sdk::{Cmd, Event, Module, Resolution, Subscriptions, cmd, refs, ui};

/// The kv key of the user's layer: `{"order": [..], "sets": {"<set>": {..}}}`.
pub const LOCAL: &str = "sets";
/// The kv key of the edits not saved yet: `{"texts": {"<set>": ..}, "bases": {"<set>": ..}}`.
pub const DRAFTS: &str = "drafts";
const SOURCES: &str = "sources";
const FILE: &str = "dictionary.json";
const TAB: &str = "dictionary-set-";
/// The lock shared by the windows that change the layer.
const LOCK: &str = "dictionary";
const FRESH: &str = "fresh";
const SAVE: &str = "save";

#[derive(Serialize, Deserialize)]
pub enum Msg {
    Tab(String),
    Edit,
    Add,
    Delete,
    /// Resets the whole set, or one variable of it, to its source.
    Reset(Option<String>),
    /// The set renamed in place (the value is the new name).
    Rename(String),
    /// Tabs dragged: the value is `["<tab>","<tab>"]`.
    Reorder,
    /// Drops a conflicting draft and shows what was saved.
    Discard,
}

/// The sets with their variables, in order.
#[derive(Clone, Default, PartialEq)]
struct State {
    order: Vec<String>,
    sets: Map<String, Value>,
}

impl State {
    fn vars(&self, name: &str) -> Map<String, Value> {
        self.sets
            .get(name)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    }

    fn read(value: &Value) -> Self {
        let order = value["order"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let sets = value["sets"].as_object().cloned().unwrap_or_default();
        Self { order, sets }
    }

    fn json(&self) -> Value {
        json!({"order": self.order, "sets": self.sets})
    }
}

/// The sets of the sources.
#[derive(Default)]
struct Base {
    order: Vec<String>,
    sets: Map<String, Value>,
    /// The highest-priority source of each set.
    sources: BTreeMap<String, String>,
    /// The source of each variable: set → variable → source.
    origins: BTreeMap<String, BTreeMap<String, String>>,
}

impl Base {
    fn read(files: &Value) -> Self {
        let mut base = Self::default();
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
            for name in sets.keys() {
                if !names.contains(name) {
                    names.push(name.clone());
                }
            }
            for name in names {
                let Some(vars) = sets[&name].as_object() else {
                    continue;
                };
                if reserved(&name) {
                    continue;
                }
                if !base.sets.contains_key(&name) {
                    base.order.push(name.clone());
                }
                let set = base.sets.entry(name.clone()).or_insert_with(|| json!({}));
                let origins = base.origins.entry(name.clone()).or_default();
                for (variable, value) in vars {
                    set[variable] = value.clone();
                    origins.insert(variable.clone(), source.clone());
                }
                base.sources.insert(name, source.clone());
            }
        }
        base
    }

    /// The sets of the sources with the user's layer over them.
    fn layered(&self, local: &State) -> State {
        let mut sets = self.sets.clone();
        for (name, vars) in &local.sets {
            let set = sets.entry(name.clone()).or_insert_with(|| json!({}));
            if let (Some(set), Some(vars)) = (set.as_object_mut(), vars.as_object()) {
                for (variable, value) in vars {
                    set.insert(variable.clone(), value.clone());
                }
            }
        }
        let mut order: Vec<String> = local
            .order
            .iter()
            .filter(|name| sets.contains_key(*name))
            .cloned()
            .collect();
        for name in self.order.iter().chain(sets.keys()) {
            if !order.contains(name) {
                order.push(name.clone());
            }
        }
        State { order, sets }
    }

    /// The user's layer of `state`: own sets and the variables that differ from the sources.
    fn local(&self, state: &State) -> Result<State, String> {
        for name in self.sets.keys() {
            if !state.sets.contains_key(name) {
                return Err(self.fixed(name));
            }
        }
        let mut sets = Map::new();
        for (name, vars) in &state.sets {
            let Some(source) = self.sets.get(name).and_then(Value::as_object) else {
                sets.insert(name.clone(), vars.clone());
                continue;
            };
            let vars = vars.as_object().cloned().unwrap_or_default();
            if let Some(missing) = source.keys().find(|key| !vars.contains_key(*key)) {
                return Err(format!(
                    "{name}:{missing} comes from source {}; reset it instead of deleting it",
                    self.origins[name][missing]
                ));
            }
            let overrides: Map<String, Value> = vars
                .into_iter()
                .filter(|(key, value)| source.get(key) != Some(value))
                .collect();
            if !overrides.is_empty() {
                sets.insert(name.clone(), Value::Object(overrides));
            }
        }
        Ok(State {
            order: state.order.clone(),
            sets,
        })
    }

    fn fixed(&self, name: &str) -> String {
        format!(
            "Set {name} comes from source {}; it can't be deleted or renamed",
            self.sources
                .get(name)
                .map(String::as_str)
                .unwrap_or_default()
        )
    }
}

fn reserved(name: &str) -> bool {
    matches!(name, "var" | "env")
}

/// A set name must work in references (`{{name:VAR}}`).
fn valid(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        || reserved(name)
    {
        return Err(format!(
            "Invalid set name \"{name}\": use letters, digits, _ and -."
        ));
    }
    Ok(())
}

/// A change of the user's layer.
#[derive(Clone, Debug, PartialEq)]
enum Op {
    /// `base` is the set the edit started from (`Null` for a new set).
    Save {
        name: String,
        vars: Map<String, Value>,
        base: Value,
        add: bool,
    },
    Delete(String),
    Rename(String, String),
    Reorder(Vec<String>),
    Reset(String, Option<String>),
}

impl Op {
    fn label(&self) -> &'static str {
        match self {
            Self::Save { add: true, .. } => "add",
            Self::Save { .. } => "save",
            Self::Delete(_) => "delete",
            Self::Rename(..) => "rename",
            Self::Reorder(_) => "reorder",
            Self::Reset(..) => "reset",
        }
    }

    fn apply(&self, state: &mut State, base: &Base) -> Result<(), String> {
        match self {
            Self::Save {
                name,
                vars,
                base: from,
                ..
            } => {
                let current = state.sets.get(name).unwrap_or(&Value::Null);
                let vars = Value::Object(vars.clone());
                if current != from && *current != vars {
                    return Err(if from.is_null() {
                        format!("CONFLICT: set {name} was created in another window.")
                    } else {
                        format!(
                            "CONFLICT: set {name} changed in another window. Your draft is preserved; discard it to load the saved version."
                        )
                    });
                }
                state.sets.insert(name.clone(), vars);
                if !state.order.contains(name) {
                    state.order.push(name.clone());
                }
            }
            Self::Delete(name) | Self::Rename(name, _) if base.sets.contains_key(name) => {
                return Err(base.fixed(name));
            }
            Self::Delete(name) => {
                if state.sets.remove(name).is_none() {
                    return Err(format!("Unknown set: {name}"));
                }
                state.order.retain(|set| set != name);
            }
            Self::Rename(name, new) => {
                valid(new)?;
                if !state.sets.contains_key(name) {
                    return Err(format!("Unknown set: {name}"));
                }
                if new != name && state.sets.contains_key(new) {
                    return Err(format!("A set named \"{new}\" already exists"));
                }
                state.sets = std::mem::take(&mut state.sets)
                    .into_iter()
                    .map(|(set, vars)| (if set == *name { new.clone() } else { set }, vars))
                    .collect();
                for set in &mut state.order {
                    if set == name {
                        set.clone_from(new);
                    }
                }
            }
            Self::Reorder(order) => {
                let distinct: BTreeSet<&String> = order.iter().collect();
                if order.len() != state.sets.len()
                    || distinct.len() != order.len()
                    || order.iter().any(|name| !state.sets.contains_key(name))
                {
                    return Err(
                        "Order must contain exactly the current set names, each once".into(),
                    );
                }
                state.order.clone_from(order);
            }
            Self::Reset(name, variable) => {
                let Some(source) = base.sets.get(name).and_then(Value::as_object) else {
                    return Err(format!("Set {name} doesn't come from a source"));
                };
                let set = state.sets.entry(name.clone()).or_insert_with(|| json!({}));
                match variable {
                    Some(variable) => match (source.get(variable), set.as_object_mut()) {
                        (Some(value), Some(set)) => {
                            set.insert(variable.clone(), value.clone());
                        }
                        (None, Some(set)) => {
                            set.remove(variable);
                        }
                        _ => {}
                    },
                    None => *set = Value::Object(source.clone()),
                }
            }
        }
        Ok(())
    }
}

/// Applies `op` if the result is a valid layer.
fn apply(op: &Op, state: &mut State, base: &Base) -> Result<(), String> {
    let mut next = state.clone();
    op.apply(&mut next, base)?;
    base.local(&next)?;
    *state = next;
    Ok(())
}

#[derive(Default)]
pub struct Dictionary {
    base: Base,
    /// The sources were read: changes wait for them.
    ready: bool,
    /// The user's layer as last read or saved.
    saved: State,
    /// What the panel shows and resolves: the saved layer with the queued changes.
    state: State,
    queue: Vec<Op>,
    /// The changes of the running transaction with their outcomes.
    applied: Vec<(Op, Result<(), String>)>,
    /// A transaction runs.
    busy: bool,
    /// Texts of sets not saved yet: invalid ones, conflicting ones and those being saved.
    drafts: Map<String, Value>,
    /// The set each draft started from: a save conflicts if another window changed it since.
    bases: Map<String, Value>,
    conflicts: BTreeSet<String>,
    /// What the user typed in a set since it was last loaded: the editor keeps it as typed.
    typed: Map<String, Value>,
    tab: String,
}

impl Dictionary {
    /// Takes the sets of the sources (`[{source, text}]`) and the user's layer directly (tests).
    pub fn with(sources: Value, local: Value) -> Self {
        let mut dictionary = Self {
            base: Base::read(&sources),
            ready: true,
            saved: State::read(&local),
            ..Self::default()
        };
        dictionary.merge();
        dictionary
    }

    /// Shows the saved layer with the queued changes that still apply.
    fn merge(&mut self) {
        let mut state = self.base.layered(&self.saved);
        for op in &self.queue {
            let _ = apply(op, &mut state, &self.base);
        }
        self.state = state;
    }

    fn active(&self) -> Option<&str> {
        self.state
            .order
            .iter()
            .find(|name| **name == self.tab)
            .or(self.state.order.first())
            .map(String::as_str)
    }

    /// The editor's text of a set.
    fn shown(&self, name: &str) -> String {
        self.typed
            .get(name)
            .or_else(|| self.drafts.get(name))
            .and_then(Value::as_str)
            .map_or_else(|| refs::format_lines(&self.state.vars(name)), str::to_owned)
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
        for name in &self.state.order {
            for (variable, value) in self.state.vars(name) {
                if !variable.starts_with('#') {
                    hints.push((format!("{{{{{name}:{variable}}}}}"), refs::text(&value)));
                }
            }
        }
        for name in &self.state.order {
            for (variable, value) in self.state.vars(name) {
                let bare = format!("{{{{:{variable}}}}}");
                if !variable.starts_with('#') && !hints.iter().any(|(key, _)| *key == bare) {
                    hints.push((bare, refs::text(&value)));
                }
            }
        }
        cmd::contribute(&json!({ "variables": hints }))
    }

    fn put_drafts(&self) -> Cmd {
        if self.drafts.is_empty() {
            cmd::kv_delete("", DRAFTS)
        } else {
            let bases: Map<String, Value> = self
                .bases
                .iter()
                .filter(|(name, _)| self.drafts.contains_key(*name))
                .map(|(name, base)| (name.clone(), base.clone()))
                .collect();
            cmd::kv_put(
                "",
                DRAFTS,
                &json!({"texts": self.drafts, "bases": bases}),
                false,
            )
        }
    }

    /// Queues a change: it shows at once and is saved by the next transaction.
    fn enqueue(&mut self, op: Op) -> Vec<Cmd> {
        self.queue.push(op);
        self.merge();
        let mut commands = vec![self.contribution()];
        commands.extend(self.start());
        commands
    }

    fn start(&mut self) -> Vec<Cmd> {
        if self.busy || !self.ready || self.queue.is_empty() {
            return Vec::new();
        }
        self.busy = true;
        vec![cmd::lock(LOCK, LOCK)]
    }

    /// The fresh layer under the lock: the queued changes apply to it.
    fn transact(&mut self, local: &Value) -> Vec<Cmd> {
        self.saved = State::read(local);
        let mut state = self.base.layered(&self.saved);
        for op in std::mem::take(&mut self.queue) {
            let outcome = apply(&op, &mut state, &self.base);
            self.applied.push((op, outcome));
        }
        let mut commands = Vec::new();
        match self.base.local(&state) {
            Ok(local) if self.base.layered(&local) != self.base.layered(&self.saved) => {
                commands.push(cmd::kv_put(SAVE, LOCAL, &local.json(), false));
                self.saved = local;
            }
            _ => commands.extend(self.finish(Ok(()))),
        }
        self.merge();
        commands.push(self.contribution());
        commands
    }

    /// The transaction ended: the lock goes, its outcomes show and the next one starts.
    fn finish(&mut self, saved: Result<(), String>) -> Vec<Cmd> {
        self.busy = false;
        let mut commands = vec![cmd::unlock(LOCK)];
        let mut status = None;
        for (op, outcome) in std::mem::take(&mut self.applied) {
            let outcome = saved.clone().and(outcome);
            if let Op::Save {
                name, vars, base, ..
            } = &op
            {
                match &outcome {
                    Ok(()) => {
                        let current = self
                            .drafts
                            .get(name)
                            .and_then(Value::as_str)
                            .and_then(|text| refs::parse_lines(text).ok());
                        self.conflicts.remove(name);
                        if current.as_ref() == Some(vars) {
                            self.drafts.remove(name);
                            self.bases.remove(name);
                        }
                    }
                    Err(error) if error.starts_with("CONFLICT") => {
                        // The draft stays and is retried against the set it started from.
                        if self.conflicts.insert(name.clone()) {
                            self.bases.insert(name.clone(), base.clone());
                        }
                    }
                    Err(_) => {}
                }
            }
            let text = match &outcome {
                Ok(()) => (format!("Dictionary {} · OK", op.label()), "ok"),
                Err(error) => (
                    format!("Dictionary {} · ERROR · {error}", op.label()),
                    "error",
                ),
            };
            if outcome.is_err()
                || status
                    .as_ref()
                    .is_none_or(|(_, indicator)| *indicator != "error")
            {
                status = Some(text);
            }
        }
        if let Some((text, indicator)) = status {
            commands.push(cmd::status(&text, indicator));
        }
        commands.push(self.put_drafts());
        if saved.is_err() {
            commands.push(cmd::kv_get(LOCAL, LOCAL));
        }
        commands.extend(self.start());
        commands
    }

    fn edit(&mut self, text: String) -> Vec<Cmd> {
        let Some(name) = self.active().map(str::to_owned) else {
            return Vec::new();
        };
        self.typed.insert(name.clone(), json!(text));
        let base = self
            .bases
            .get(&name)
            .cloned()
            .unwrap_or_else(|| self.state.sets.get(&name).cloned().unwrap_or(Value::Null));
        self.drafts.insert(name.clone(), json!(text));
        match refs::parse_lines(&text) {
            Ok(vars) => {
                // Later edits start from what this one saves; a conflicting draft is retried
                // against the set it started from.
                if !self.conflicts.contains(&name) {
                    self.bases.insert(name.clone(), Value::Object(vars.clone()));
                }
                let mut commands = self.enqueue(Op::Save {
                    name,
                    vars,
                    base,
                    add: false,
                });
                commands.push(self.put_drafts());
                commands
            }
            Err(error) => {
                self.bases.insert(name, base);
                vec![
                    self.put_drafts(),
                    cmd::status(&format!("ERROR: {error}"), "error"),
                ]
            }
        }
    }

    /// Forgets what the user has not saved of a set.
    fn forget(&mut self, name: &str) {
        self.drafts.remove(name);
        self.typed.remove(name);
        self.bases.remove(name);
        self.conflicts.remove(name);
        self.queue
            .retain(|op| !matches!(op, Op::Save { name: set, .. } if set == name));
    }

    fn add(&mut self, name: &str) -> Vec<Cmd> {
        let name = name.trim();
        if let Err(error) = valid(name) {
            return vec![cmd::status(&format!("ERROR: {error}"), "error")];
        }
        if self.state.sets.contains_key(name) {
            return vec![cmd::status(
                &format!("ERROR: Set \"{name}\" already exists."),
                "error",
            )];
        }
        self.tab = name.to_owned();
        self.enqueue(Op::Save {
            name: name.to_owned(),
            vars: Map::new(),
            base: Value::Null,
            add: true,
        })
    }

    fn rename(&mut self, name: String, new: &str) -> Vec<Cmd> {
        let new = new.trim().to_owned();
        if new == name {
            return Vec::new();
        }
        if let Err(error) = valid(&new) {
            return vec![cmd::status(
                &format!("Dictionary rename · ERROR · {error}"),
                "error",
            )];
        }
        if self.state.sets.contains_key(&new) {
            return vec![cmd::status(
                &format!("Dictionary rename · ERROR · A set named \"{new}\" already exists"),
                "error",
            )];
        }
        for map in [&mut self.drafts, &mut self.bases, &mut self.typed] {
            if let Some(value) = map.remove(&name) {
                map.insert(new.clone(), value);
            }
        }
        if self.conflicts.remove(&name) {
            self.conflicts.insert(new.clone());
        }
        for op in &mut self.queue {
            if let Op::Save { name: set, .. } = op
                && *set == name
            {
                set.clone_from(&new);
            }
        }
        if self.tab == name || self.tab.is_empty() {
            self.tab.clone_from(&new);
        }
        let mut commands = self.enqueue(Op::Rename(name, new));
        commands.push(self.put_drafts());
        commands
    }

    /// The dragged tab goes before the one it was dropped on.
    fn reorder(&mut self, value: &str) -> Vec<Cmd> {
        let Ok([from, to]) = serde_json::from_str::<[String; 2]>(value) else {
            return Vec::new();
        };
        let (from, to) = (from.trim_start_matches(TAB), to.trim_start_matches(TAB));
        let mut order = self.state.order.clone();
        let Some(at) = order.iter().position(|name| name == from) else {
            return Vec::new();
        };
        let moved = order.remove(at);
        let Some(at) = order.iter().position(|name| name == to) else {
            return Vec::new();
        };
        order.insert(at, moved);
        self.enqueue(Op::Reorder(order))
    }

    /// The value of `name` in `set` with the references the Dictionary owns resolved.
    fn own(
        &self,
        set: &str,
        name: &str,
        chain: &mut Vec<String>,
    ) -> Result<Option<String>, String> {
        let Some(value) = self.state.sets.get(set).and_then(|vars| vars.get(name)) else {
            return Ok(None);
        };
        let link = format!("{set}:{name}");
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
            for owner in &self.state.order {
                if let Some(value) = self.own(owner, name, chain)? {
                    return Ok(Some(value));
                }
            }
            return Ok(None);
        }
        if !self.state.sets.contains_key(set) {
            return Ok(None);
        }
        match self.own(set, name, chain)? {
            Some(value) => Ok(Some(value)),
            None => Err(format!("Unknown variable \"{name}\" in {set}")),
        }
    }

    fn view_set(&self, name: &str) -> Vec<ui::Node> {
        let mut header = Vec::new();
        match self.base.sources.get(name) {
            Some(source) => {
                header.push(
                    ui::text(format!("From source {source}"))
                        .variant("muted")
                        .key("dictionary-origin-label"),
                );
                let from = self.base.sets.get(name).and_then(Value::as_object);
                let yours: Vec<String> = self
                    .state
                    .vars(name)
                    .into_iter()
                    .filter(|(variable, value)| {
                        from.and_then(|from| from.get(variable)) != Some(value)
                    })
                    .map(|(variable, _)| variable)
                    .collect();
                if !yours.is_empty() {
                    header.push(ui::text("Yours:").variant("muted"));
                    for variable in yours {
                        header.push(
                            ui::button(
                                format!("{variable} ↺"),
                                &Msg::Reset(Some(variable.clone())),
                            )
                            .key(format!("dictionary-reset-variable-{variable}")),
                        );
                    }
                    header.push(
                        ui::button(format!("Reset all to {source}"), &Msg::Reset(None))
                            .key("dictionary-reset"),
                    );
                }
            }
            None => {
                header.push(
                    ui::text("Your own set")
                        .variant("muted")
                        .key("dictionary-origin-label"),
                );
                header.push(
                    ui::button("Delete set", &Msg::Delete)
                        .variant("danger")
                        .key("dictionary-delete"),
                );
            }
        }
        if self.conflicts.contains(name) && self.drafts.contains_key(name) {
            header
                .push(ui::button("Discard draft", &Msg::Discard).key("console-dictionary-discard"));
        }
        vec![
            ui::row(header).key("dictionary-origin"),
            ui::editor("headers", self.shown(name), &Msg::Edit).key("console-dictionary"),
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
            // The Kernel released the lock and dropped the pending replies: the queued changes
            // run when shown again; a save already sent is read back then.
            Event::Hidden => {
                self.busy = false;
                self.applied.clear();
                Vec::new()
            }
            Event::Storage { key } if key == LOCAL => vec![cmd::kv_get(LOCAL, LOCAL)],
            Event::Reply { tag, result } => match (tag.as_str(), result) {
                (SOURCES, Ok(files)) => {
                    self.base = Base::read(&files);
                    self.ready = true;
                    self.merge();
                    let mut commands = vec![self.contribution()];
                    commands.extend(self.start());
                    commands
                }
                (LOCAL, Ok(local)) => {
                    self.saved = State::read(&local);
                    self.merge();
                    // Text typed here stays unless another window saved the set differently.
                    let state = &self.state;
                    self.typed.retain(|name, text| {
                        match refs::parse_lines(text.as_str().unwrap_or_default()) {
                            Ok(vars) => state.sets.get(name) == Some(&Value::Object(vars)),
                            Err(_) => true,
                        }
                    });
                    vec![self.contribution()]
                }
                (DRAFTS, Ok(drafts)) => {
                    self.drafts = drafts["texts"].as_object().cloned().unwrap_or_default();
                    self.bases = drafts["bases"].as_object().cloned().unwrap_or_default();
                    Vec::new()
                }
                (LOCK, Ok(_)) => vec![cmd::kv_get(FRESH, LOCAL)],
                (LOCK, Err(failure)) => {
                    self.busy = false;
                    vec![cmd::status(
                        &format!("Dictionary save · ERROR · {}", failure.message),
                        "error",
                    )]
                }
                (FRESH, Ok(local)) => self.transact(&local),
                (FRESH, Err(failure)) => {
                    self.busy = false;
                    vec![
                        cmd::unlock(LOCK),
                        cmd::status(
                            &format!("Dictionary save · ERROR · {}", failure.message),
                            "error",
                        ),
                    ]
                }
                (SAVE, result) => {
                    self.finish(result.map(|_| ()).map_err(|failure| failure.message))
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
                let unchanged = self.active().is_some_and(|name| self.shown(name) == text);
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
            } => {
                let Some(name) = self.active().map(str::to_owned) else {
                    return Vec::new();
                };
                self.forget(&name);
                let mut commands = self.enqueue(Op::Delete(name));
                commands.push(self.put_drafts());
                commands
            }
            Event::Ui {
                msg: Msg::Reset(variable),
                ..
            } => {
                let Some(name) = self.active().map(str::to_owned) else {
                    return Vec::new();
                };
                self.forget(&name);
                let mut commands = self.enqueue(Op::Reset(name, variable));
                commands.push(self.put_drafts());
                commands
            }
            Event::Ui {
                msg: Msg::Rename(name),
                value: Some(new),
            } => self.rename(name, &new),
            Event::Ui {
                msg: Msg::Reorder,
                value: Some(value),
            } => self.reorder(&value),
            Event::Ui {
                msg: Msg::Discard, ..
            } => {
                let Some(name) = self.active().map(str::to_owned) else {
                    return Vec::new();
                };
                self.forget(&name);
                self.merge();
                vec![self.put_drafts(), cmd::kv_get(LOCAL, LOCAL)]
            }
            _ => Vec::new(),
        }
    }

    fn view(&self) -> Option<ui::Node> {
        let mut body = Vec::new();
        match self.active() {
            Some(active) => {
                let tabs = self
                    .state
                    .order
                    .iter()
                    .map(|name| {
                        let body = if name == active {
                            self.view_set(name)
                        } else {
                            Vec::new()
                        };
                        let tab = ui::tab(format!("{TAB}{name}"), name.clone(), body);
                        if self.base.sets.contains_key(name) {
                            tab
                        } else {
                            tab.renamed(&Msg::Rename(name.clone()))
                        }
                    })
                    .collect();
                body.push(
                    ui::tabs(tabs, format!("{TAB}{active}"), &Msg::Tab(String::new()))
                        .on_reorder(&Msg::Reorder)
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
