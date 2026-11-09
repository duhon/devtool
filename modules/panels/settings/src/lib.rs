//! The Settings panel, built into the App: the modules and the App itself with the sources
//! they come from (install, update, remove), the sources of modules and shared settings,
//! and the model new Agent chats get. Only this module may manage modules and the App.
//!
//! The App checks the sources (once per Host process unless forced); a check that finds
//! updates or failing sources puts them on the Settings status.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use services_contracts::{
    agent::{Catalog, FUSION_MODEL_ID, FUSION_MODEL_NAME, Model, Options},
    package::APPLICATION_ID,
};
use services_sdk::{Cmd, Event, Module, cmd, time, ui};
use std::collections::BTreeSet;

const MODULES: &str = "settings-tab-modules";
const SOURCES: &str = "settings-tab-sources";
const AGENT: &str = "settings-tab-agent";
/// The kv key of the tab shown last.
const TAB: &str = "tab";
const CHECK: &str = "check";
const LIST: &str = "sources";
const MODELS: &str = "models";
const CHANGE: &str = "change";
const OPEN: &str = "open";
const UPDATE: &str = "update";
const SAVE_OPTIONS: &str = "options";

#[derive(Serialize, Deserialize)]
pub enum Msg {
    Tab,
    Check,
    /// Shows or hides a detail of a module row: `(row key, detail)`.
    Detail(String, Detail),
    /// Asks to confirm removing a module: `(row key, module)`.
    Remove(String, String),
    Forget,
    RemoveCancel,
    RemoveConfirm,
    /// Installs `(source, module, version)`; the App row updates the App.
    Install(String, String, String),
    Reload,
    /// Opens a folder source.
    OpenSource(String),
    Move(usize, bool),
    Rename(Option<usize>),
    RenameInput,
    RenameSave,
    RemoveSource(Option<usize>),
    RemoveSourceConfirm,
    Adding(bool),
    Field(Field),
    Add,
    TokenInput,
    /// Saves the entered token, or clears the saved one.
    Token(bool),
    Apply(String),
    Model,
    Reasoning,
    ReasoningLevel(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Detail {
    Description,
    Notes,
    NewNotes,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum Field {
    Name,
    Kind,
    Repository,
    Branch,
    Path,
}

/// A source being added.
#[derive(Clone, Default)]
struct Draft {
    name: String,
    /// `github` or `folder`.
    kind: String,
    repository: String,
    branch: String,
    path: String,
}

#[derive(Default)]
pub struct Settings {
    offset: f64,
    tab: String,
    /// The last check of the sources; `Null` where the Host can't check (no native host).
    check: Option<Value>,
    checking: bool,
    /// The newest check asked for; older answers are dropped.
    generation: u64,
    /// The operation the next check result belongs to: `true` when the user asked.
    forced: bool,
    /// What the last automatic check reported, so a repeat stays quiet.
    notice: String,
    /// The last operation, for the status: `(text, indicator)`.
    status: Option<(String, &'static str)>,
    /// A module change or source operation runs: `(operation, module removed with data)`.
    busy: Option<(String, Option<String>)>,
    open: BTreeSet<(String, Detail)>,
    confirming: Option<(String, String)>,
    forget: bool,
    /// The App is being updated, or is installed and waits for the reload.
    updating: bool,
    installed: Option<String>,
    sources: Vec<Value>,
    sources_loaded: bool,
    token_set: bool,
    warning: Option<String>,
    renaming: Option<(usize, String)>,
    removing: Option<usize>,
    draft: Option<Draft>,
    token: String,
    catalog: Option<Catalog>,
    options: Options,
    loading_models: bool,
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}

/// Whether version `a` is newer than `b`; versions are dotted numbers.
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |version: &str| -> Vec<u64> {
        version
            .split(['.', '-', '+'])
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    parts(a) > parts(b)
}

/// What a check found for the user: the sources that failed, and the settings and installed
/// modules that can be updated.
pub fn summary(check: &Value) -> (Vec<String>, Vec<String>) {
    let sources = check["sources"].as_array().cloned().unwrap_or_default();
    let mut errors = Vec::new();
    let mut updates = Vec::new();
    for source in &sources {
        let name = text(source, "name");
        if let Some(error) = source["error"].as_str() {
            errors.push(format!("{name}: {error}"));
        }
        let changed = source["changes"]
            .as_array()
            .is_some_and(|changes| !changes.is_empty());
        if source["configs"] == true && (source["applied"] != true || changed) {
            updates.push(format!("settings of {name}"));
        }
    }
    for (id, installed) in check["installed"].as_object().into_iter().flatten() {
        let (Some(slug), Some(version)) =
            (installed["source"].as_str(), installed["version"].as_str())
        else {
            continue;
        };
        let newest = sources
            .iter()
            .filter(|source| source["slug"] == slug)
            .flat_map(|source| source["modules"].as_array().cloned().unwrap_or_default())
            .filter(|offer| offer["id"] == id.as_str() && offer.get("unsupported").is_none())
            .filter_map(|offer| offer["version"].as_str().map(str::to_owned))
            .find(|offered| newer(offered, version));
        if let Some(newest) = newest {
            updates.push(format!("{id} {newest}"));
        }
    }
    (errors, updates)
}

/// A module with one source: what the source offers and what is installed from it. A module
/// installed from no source, or from one that no longer offers it, has a row of its own.
struct Row<'a> {
    id: &'a str,
    slug: Option<&'a str>,
    source: Option<&'a Value>,
    /// The versions the source offers, newest first.
    offers: Vec<&'a Value>,
    installed: Option<&'a Value>,
}

impl Row<'_> {
    fn key(&self) -> String {
        format!("{}/{}", self.slug.unwrap_or_default(), self.id)
    }

    fn app(&self) -> bool {
        self.id == APPLICATION_ID
    }

    fn title(&self) -> &str {
        self.installed
            .and_then(|installed| installed["name"].as_str())
            .or_else(|| self.offers.first()?["name"].as_str())
            .unwrap_or(self.id)
    }

    fn description(&self) -> &str {
        self.offers
            .first()
            .and_then(|offer| offer["description"].as_str())
            .or_else(|| self.installed?["description"].as_str())
            .unwrap_or_default()
    }

    /// The newest version the source offers that can be installed here.
    fn newest(&self) -> Option<&Value> {
        self.offers
            .iter()
            .copied()
            .find(|offer| offer.get("unsupported").is_none())
    }
}

fn rows(check: &Value) -> Vec<Row<'_>> {
    let sources: Vec<&Value> = check["sources"].as_array().into_iter().flatten().collect();
    let mut rows: Vec<Row> = Vec::new();
    for source in &sources {
        let slug = source["slug"].as_str();
        for offer in source["modules"].as_array().into_iter().flatten() {
            let id = offer["id"].as_str().unwrap_or_default();
            match rows.iter_mut().find(|row| row.id == id && row.slug == slug) {
                Some(row) => row.offers.push(offer),
                None => rows.push(Row {
                    id,
                    slug,
                    source: Some(source),
                    offers: vec![offer],
                    installed: None,
                }),
            }
        }
    }
    for (id, installed) in check["installed"].as_object().into_iter().flatten() {
        let slug = installed["source"].as_str();
        match rows
            .iter_mut()
            .find(|row| slug.is_some() && row.id == id && row.slug == slug)
        {
            Some(row) => row.installed = Some(installed),
            None => rows.push(Row {
                id,
                slug,
                source: sources
                    .iter()
                    .copied()
                    .find(|source| slug.is_some() && source["slug"].as_str() == slug),
                offers: Vec::new(),
                installed: Some(installed),
            }),
        }
    }
    rows.sort_by(|a, b| (!a.app(), a.id).cmp(&(!b.app(), b.id)));
    rows
}

fn location(source: &Value) -> String {
    if source["kind"] == "github" {
        let mut out = text(source, "repository");
        if let Some(branch) = source["branch"].as_str() {
            out.push_str(&format!(" @ {branch}"));
        }
        if let Some(path) = source["path"].as_str() {
            out.push_str(&format!(" / {path}"));
        }
        out
    } else {
        text(source, "path")
    }
}

fn category(model: &Model) -> &str {
    match model.category.as_deref().unwrap_or("") {
        "lightweight" => "Lightweight",
        "versatile" => "Versatile",
        "powerful" => "Powerful",
        "automatic" => "Automatic",
        other => other,
    }
}

/// The status of an operation that finished: `{operation} · OK`, `· WARNING · …` or
/// `· ERROR · …`.
fn outcome(
    operation: &str,
    result: &Result<Value, services_sdk::Failure>,
) -> (String, &'static str) {
    match result {
        Ok(value) => match value["warning"].as_str() {
            Some(warning) => (format!("{operation} · WARNING · {warning}"), "warning"),
            None => (format!("{operation} · OK"), "ok"),
        },
        Err(failure) => (
            format!("{operation} · ERROR · {}", failure.message),
            "error",
        ),
    }
}

impl Settings {
    fn status(&self) -> Cmd {
        match &self.status {
            Some((text, indicator)) => cmd::status(text, indicator),
            None => cmd::status("", "idle"),
        }
    }

    fn report(&mut self, text: String, indicator: &'static str) -> Cmd {
        self.status = Some((text, indicator));
        self.status()
    }

    fn running(&mut self, operation: &str) -> Cmd {
        self.report(format!("{operation} · Running…"), "busy")
    }

    fn date(&self, seconds: &Value) -> String {
        time::date(seconds.as_f64().unwrap_or_default() * 1e3, self.offset)
    }

    fn check(&mut self, force: bool) -> Vec<Cmd> {
        self.generation += 1;
        self.checking = true;
        self.forced = force;
        let mut commands = vec![cmd::modules_check(
            format!("{CHECK}:{}", self.generation),
            force,
        )];
        if force {
            commands.push(self.running("Check sources"));
        }
        commands
    }

    fn checked(&mut self, result: Result<Value, services_sdk::Failure>) -> Vec<Cmd> {
        self.checking = false;
        let (text, indicator) = match result {
            Ok(Value::Null) => {
                self.check = Some(Value::Null);
                return Vec::new();
            }
            Ok(value) => {
                let (errors, updates) = summary(&value);
                self.check = Some(value);
                let mut status = match errors.is_empty() {
                    true => ("Check sources · OK".to_owned(), "ok"),
                    false => (
                        format!("Check sources · WARNING · {}", errors.join("; ")),
                        "warning",
                    ),
                };
                if !updates.is_empty() {
                    let updates = format!("Updates available: {}", updates.join(", "));
                    status.0 = match errors.is_empty() {
                        true => format!("Check sources · {updates}"),
                        false => format!("{} · {updates}", status.0),
                    };
                    if errors.is_empty() {
                        status.1 = "warning";
                    }
                }
                status
            }
            Err(failure) => (
                format!("Check sources · ERROR · {}", failure.message),
                "error",
            ),
        };
        // Automatic checks speak up only about something new.
        let notice = if text.ends_with(" · OK") {
            String::new()
        } else {
            text.clone()
        };
        let speak = self.forced || (!notice.is_empty() && notice != self.notice);
        self.notice = notice;
        if speak {
            vec![self.report(text, indicator)]
        } else {
            Vec::new()
        }
    }

    /// Loads what the tab shown needs and has not loaded yet.
    fn load(&mut self) -> Vec<Cmd> {
        let mut commands = Vec::new();
        if self.tab == SOURCES && !self.sources_loaded && self.busy.is_none() {
            self.busy = Some(("Load sources".into(), None));
            commands.push(cmd::sources_list(LIST));
        }
        if self.tab == AGENT && self.catalog.is_none() && !self.loading_models {
            self.loading_models = true;
            commands.push(cmd::agent_models(MODELS));
            commands.push(self.running("Load Copilot models"));
        }
        commands
    }

    fn change(&mut self, operation: &str, command: Cmd, forgetting: Option<String>) -> Vec<Cmd> {
        self.busy = Some((operation.to_owned(), forgetting));
        vec![command, self.running(operation)]
    }

    /// Saves `list`; configured sources are sent with their slug so they keep their data.
    fn save(&mut self, operation: &str, list: Vec<Value>) -> Vec<Cmd> {
        let entries: Vec<Value> = list
            .into_iter()
            .map(|mut source| {
                if let Some(slug) = source.get("slug").cloned() {
                    source["id"] = slug;
                }
                source
            })
            .collect();
        self.change(operation, cmd::sources_save(CHANGE, &entries), None)
    }

    fn load_sources(&mut self, value: &Value) {
        self.sources = value["sources"].as_array().cloned().unwrap_or_default();
        self.token_set = value["tokenSet"] == true;
        self.warning = value["warning"].as_str().map(str::to_owned);
        self.sources_loaded = true;
    }

    /// A module change or source operation finished.
    fn changed(&mut self, result: Result<Value, services_sdk::Failure>) -> Vec<Cmd> {
        let Some((operation, _)) = self.busy.take() else {
            return Vec::new();
        };
        let (mut text, indicator) = outcome(&operation, &result);
        let mut commands = Vec::new();
        if let Ok(value) = &result {
            match operation.as_str() {
                "Install module" => {
                    let module = &value["installed"];
                    let replaced = value["replaced"]
                        .as_str()
                        .map(|version| format!(" (was {version})"))
                        .unwrap_or_default();
                    text = format!(
                        "Installed {} {}{replaced}",
                        text_of(module, "name"),
                        text_of(module, "version")
                    );
                }
                "Remove module" => {
                    let removed = text_of(value, "removed");
                    text = if let Some(warning) = value["warning"].as_str() {
                        format!("Removed {removed} · WARNING · {warning}")
                    } else if value["forgotten"] == true {
                        format!("Removed {removed} and its data")
                    } else {
                        format!("Removed {removed}")
                    };
                }
                "Load sources" => {
                    self.load_sources(value);
                    return Vec::new();
                }
                _ => {
                    self.load_sources(value);
                    match operation.as_str() {
                        "Add source" => self.draft = None,
                        "Rename source" => self.renaming = None,
                        "Remove source" => self.removing = None,
                        "Save GitHub token" | "Clear GitHub token" => self.token.clear(),
                        _ => {}
                    }
                }
            }
            commands.extend(self.check(false));
        } else if operation == "Load sources" {
            self.sources_loaded = true;
        }
        commands.push(self.report(text, indicator));
        commands
    }

    fn current(&self) -> Option<&Model> {
        let catalog = self.catalog.as_ref()?;
        let id = self
            .options
            .model
            .as_ref()
            .or(catalog.current.model.as_ref())?;
        catalog.models.iter().find(|model| &model.id == id)
    }

    fn efforts(&self) -> &[String] {
        self.current()
            .map(|model| model.reasoning_efforts.as_slice())
            .unwrap_or_default()
    }

    fn default_effort(&self) -> Option<&str> {
        self.catalog
            .as_ref()
            .filter(|_| self.options.model.is_none())
            .and_then(|catalog| catalog.current.reasoning_effort.as_deref())
            .or_else(|| {
                self.current()
                    .and_then(|model| model.default_reasoning_effort.as_deref())
            })
    }

    /// The saved level, otherwise the level new chats get by default.
    fn reasoning_level(&self) -> Option<usize> {
        let efforts = self.efforts();
        self.options
            .reasoning_effort
            .as_deref()
            .or(self.default_effort())
            .and_then(|effort| efforts.iter().position(|level| level == effort))
    }

    fn model_id(&self) -> String {
        self.options
            .model
            .clone()
            .or_else(|| self.catalog.as_ref()?.current.model.clone())
            .unwrap_or_else(|| FUSION_MODEL_ID.into())
    }

    fn choose(&mut self, options: Options) -> Vec<Cmd> {
        let result = self
            .catalog
            .as_ref()
            .ok_or_else(|| "Load Copilot models first".to_owned())
            .and_then(|catalog| options.validate(&catalog.models));
        match result {
            Ok(()) => {
                let value = serde_json::to_value(&options).unwrap_or_default();
                self.options = options;
                vec![
                    cmd::agent_options(SAVE_OPTIONS, Some(&value)),
                    self.running("New Agent chat settings"),
                ]
            }
            Err(error) => vec![self.report(
                format!("New Agent chat settings · ERROR · {error}"),
                "error",
            )],
        }
    }

    fn reasoning(&mut self, level: usize) -> Vec<Cmd> {
        let Some(model) = self.current() else {
            return Vec::new();
        };
        let Some(effort) = model.reasoning_efforts.get(level).cloned() else {
            return Vec::new();
        };
        // Choosing the model's own default stores no level, so new chats keep following it.
        let options = Options {
            model: Some(model.id.clone()),
            reasoning_effort: (model.default_reasoning_effort.as_ref() != Some(&effort))
                .then_some(effort),
        };
        self.choose(options)
    }

    fn models_loaded(&mut self, result: Result<Value, services_sdk::Failure>) -> Vec<Cmd> {
        self.loading_models = false;
        let loaded = result.map_err(|failure| failure.message).and_then(|value| {
            let catalog = serde_json::from_value::<Catalog>(value["catalog"].clone())
                .map_err(|error| format!("Invalid Copilot model catalog: {error}"))?;
            if !catalog.models.iter().any(|model| model.enabled) {
                return Err("No Copilot chat models are available for this account".into());
            }
            let options = serde_json::from_value::<Options>(value["options"].clone())
                .map_err(|error| format!("Invalid Agent model settings: {error}"));
            self.catalog = Some(catalog);
            let options = options?;
            options.validate(&self.catalog.as_ref().expect("loaded").models)?;
            self.options = options;
            Ok(())
        });
        vec![match loaded {
            Ok(()) => self.report("Load Copilot models · OK".into(), "ok"),
            Err(error) => self.report(format!("Load Copilot models · ERROR · {error}"), "error"),
        }]
    }
}

fn text_of(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}

impl Module for Settings {
    type Msg = Msg;

    fn update(&mut self, event: Event<Msg>) -> Vec<Cmd> {
        match event {
            Event::Init { settings } => {
                self.offset = settings["timezoneOffset"].as_f64().unwrap_or_default();
                self.tab = MODULES.into();
                let mut commands = vec![cmd::kv_get(TAB, TAB)];
                commands.extend(self.check(false));
                commands
            }
            Event::Shown => {
                let mut commands = Vec::new();
                // The Host checks once per process; later asks get its saved results.
                if !self.checking {
                    commands.extend(self.check(false));
                }
                commands.extend(self.load());
                commands
            }
            Event::Modules => {
                if self.checking {
                    Vec::new()
                } else {
                    self.check(false)
                }
            }
            Event::Reply { tag, result } => self.reply(&tag, result),
            Event::Ui { msg, value } => self.ui(msg, value.unwrap_or_default()),
            _ => Vec::new(),
        }
    }

    fn view(&self) -> Option<ui::Node> {
        let tabs = vec![
            ui::tab(MODULES, "Modules", self.view_modules()),
            ui::tab(SOURCES, "Sources", self.view_sources()),
            ui::tab(AGENT, "Agent", self.view_agent()),
        ];
        Some(
            ui::tabbed_panel(
                ui::tabs(tabs, self.tab.clone(), &Msg::Tab).key("settings-tabs"),
                Vec::new(),
                Vec::new(),
            )
            .key("settings"),
        )
    }
}

impl Settings {
    fn reply(&mut self, tag: &str, result: Result<Value, services_sdk::Failure>) -> Vec<Cmd> {
        match tag {
            TAB => {
                if let Some(tab) = result
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    && [MODULES, SOURCES, AGENT].contains(&tab.as_str())
                {
                    self.tab = tab;
                }
                Vec::new()
            }
            tag if tag.starts_with(CHECK) => {
                if tag != format!("{CHECK}:{}", self.generation) {
                    return Vec::new();
                }
                self.checked(result)
            }
            LIST | CHANGE => self.changed(result),
            OPEN => match result {
                Ok(_) => Vec::new(),
                Err(failure) => vec![self.report(
                    format!("Open source · ERROR · {}", failure.message),
                    "error",
                )],
            },
            UPDATE => match result {
                Ok(value) => {
                    self.installed = self.installed.take().or_else(|| Some(String::new()));
                    self.updating = false;
                    // Installed, but the App could not reload itself: the Reload action retries.
                    match value["warning"].as_str() {
                        Some(warning) => vec![
                            self.report(format!("Install update · ERROR · {warning}"), "error"),
                        ],
                        None => vec![self.report("Install update · OK".into(), "ok")],
                    }
                }
                Err(failure) => {
                    self.updating = false;
                    self.installed = None;
                    vec![self.report(
                        format!("Install update · ERROR · {}", failure.message),
                        "error",
                    )]
                }
            },
            MODELS => self.models_loaded(result),
            SAVE_OPTIONS => {
                let (text, indicator) = outcome("New Agent chat settings", &result);
                vec![self.report(text, indicator)]
            }
            _ => Vec::new(),
        }
    }

    fn ui(&mut self, msg: Msg, value: String) -> Vec<Cmd> {
        match msg {
            Msg::Tab => {
                self.tab = value;
                let mut commands = vec![cmd::kv_put("", TAB, &json!(self.tab), false)];
                commands.extend(self.load());
                commands
            }
            Msg::Check => self.check(true),
            Msg::Detail(key, detail) => {
                if !self.open.remove(&(key.clone(), detail)) {
                    self.open.insert((key, detail));
                }
                Vec::new()
            }
            Msg::Remove(key, id) => {
                self.confirming = Some((key, id));
                self.forget = false;
                Vec::new()
            }
            Msg::Forget => {
                self.forget = value == "true";
                Vec::new()
            }
            Msg::RemoveCancel => {
                self.confirming = None;
                Vec::new()
            }
            Msg::RemoveConfirm => {
                let Some((_, id)) = self.confirming.take() else {
                    return Vec::new();
                };
                let forgetting = self.forget.then(|| id.clone());
                self.change(
                    "Remove module",
                    cmd::modules_remove(CHANGE, &id, self.forget),
                    forgetting,
                )
            }
            Msg::Install(source, id, version) => {
                if id == APPLICATION_ID {
                    self.updating = true;
                    self.installed = Some(source.clone());
                    return vec![
                        cmd::app_update(UPDATE, &source, &version),
                        self.running("Install update"),
                    ];
                }
                self.change(
                    "Install module",
                    cmd::modules_install(CHANGE, &source, &id, &version),
                    None,
                )
            }
            Msg::Reload => vec![cmd::app_reload()],
            Msg::OpenSource(slug) => vec![cmd::sources_open(OPEN, &slug)],
            Msg::Move(index, up) => {
                let other = if up {
                    index.checked_sub(1)
                } else {
                    Some(index + 1)
                };
                let Some(other) = other.filter(|other| *other < self.sources.len()) else {
                    return Vec::new();
                };
                let mut list = self.sources.clone();
                list.swap(index, other);
                self.save("Reorder sources", list)
            }
            Msg::Rename(index) => {
                self.renaming =
                    index.and_then(|index| Some((index, text(self.sources.get(index)?, "name"))));
                Vec::new()
            }
            Msg::RenameInput => {
                if let Some((_, name)) = &mut self.renaming {
                    *name = value;
                }
                Vec::new()
            }
            Msg::RenameSave => {
                let Some((index, name)) = self.renaming.clone() else {
                    return Vec::new();
                };
                let mut list = self.sources.clone();
                let Some(source) = list.get_mut(index) else {
                    return Vec::new();
                };
                source["name"] = json!(name);
                self.save("Rename source", list)
            }
            Msg::RemoveSource(index) => {
                self.removing = index;
                Vec::new()
            }
            Msg::RemoveSourceConfirm => {
                let Some(index) = self.removing.filter(|index| *index < self.sources.len()) else {
                    return Vec::new();
                };
                let mut list = self.sources.clone();
                list.remove(index);
                self.save("Remove source", list)
            }
            Msg::Adding(adding) => {
                self.draft = adding.then(|| Draft {
                    kind: "github".into(),
                    ..Draft::default()
                });
                Vec::new()
            }
            Msg::Field(field) => {
                if let Some(draft) = &mut self.draft {
                    match field {
                        Field::Name => draft.name = value,
                        Field::Kind => draft.kind = value,
                        Field::Repository => draft.repository = value,
                        Field::Branch => draft.branch = value,
                        Field::Path => draft.path = value,
                    }
                }
                Vec::new()
            }
            Msg::Add => {
                let Some(draft) = self.draft.clone() else {
                    return Vec::new();
                };
                let entry = if draft.kind == "folder" {
                    json!({"name":draft.name,"kind":"folder","path":draft.path})
                } else {
                    json!({"name":draft.name,"kind":"github","repository":draft.repository,
                        "branch":draft.branch,"path":draft.path})
                };
                let mut list = self.sources.clone();
                list.push(entry);
                self.save("Add source", list)
            }
            Msg::TokenInput => {
                self.token = value;
                Vec::new()
            }
            Msg::Token(save) => {
                let token = save.then(|| self.token.trim().to_owned());
                let operation = if save {
                    "Save GitHub token"
                } else {
                    "Clear GitHub token"
                };
                self.change(
                    operation,
                    cmd::sources_token(CHANGE, token.as_deref()),
                    None,
                )
            }
            Msg::Apply(slug) => self.change(
                "Apply source settings",
                cmd::sources_apply(CHANGE, &slug),
                None,
            ),
            Msg::Model => self.choose(Options {
                model: Some(value),
                reasoning_effort: None,
            }),
            Msg::Reasoning => match value.parse::<f64>() {
                Ok(level) if level >= 0.0 => self.reasoning(level as usize),
                _ => Vec::new(),
            },
            Msg::ReasoningLevel(level) => self.reasoning(level),
        }
    }

    fn view_modules(&self) -> Vec<ui::Node> {
        let mut body = vec![
            ui::row(vec![
                ui::text("Modules").variant("primary"),
                ui::button("Check for updates", &Msg::Check)
                    .key("settings-modules-check")
                    .disabled(self.checking),
            ])
            .key("settings-modules-head"),
        ];
        let Some(check) = self.check.as_ref().filter(|check| !check.is_null()) else {
            let hint = match &self.check {
                Some(_) => "Modules and their sources need the native host.",
                None if self.checking => "Checking the sources…",
                None => "Check the sources to see the modules.",
            };
            body.push(
                ui::text(hint)
                    .variant("muted")
                    .key("settings-modules-empty"),
            );
            return body;
        };
        let app_version = check["installed"][APPLICATION_ID]["version"].as_str();
        let rows = rows(check);
        if rows.is_empty() {
            body.push(
                ui::text("Check the sources to see the modules.")
                    .variant("muted")
                    .key("settings-modules-empty"),
            );
            return body;
        }
        let mut table = Vec::new();
        let mut details = Vec::new();
        for row in &rows {
            let (cells, extra) = self.view_row(row, app_version);
            table.push(ui::TableRow {
                key: format!("settings-module-{}", row.key()),
                cells,
            });
            details.extend(extra);
        }
        body.push(
            ui::table(
                ["Name", "Source", "Version", "New version", ""]
                    .map(str::to_owned)
                    .to_vec(),
                table,
            )
            .key("settings-modules"),
        );
        body.extend(details);
        body
    }

    fn view_source_link(&self, row: &Row) -> ui::Node {
        let Some(source) = row.source else {
            return ui::text("—").key(format!("settings-module-source-{}", row.key()));
        };
        let name = text(source, "name");
        if source["kind"] == "folder" {
            let slug = row.slug.unwrap_or_default().to_owned();
            return ui::button(name, &Msg::OpenSource(slug))
                .variant("muted")
                .key(format!("settings-module-source-{}", row.key()));
        }
        let repository = text(source, "repository");
        let branch = source["branch"].as_str();
        let url = match source["path"].as_str() {
            Some(path) => format!(
                "https://github.com/{repository}/tree/{}/{path}",
                branch.unwrap_or("HEAD")
            ),
            None => match branch {
                Some(branch) => format!("https://github.com/{repository}/tree/{branch}"),
                None => format!("https://github.com/{repository}"),
            },
        };
        ui::markdown(format!("[{name}]({url})"))
            .key(format!("settings-module-source-{}", row.key()))
    }

    /// A module row's cells, and what shows under the table for it.
    fn view_row(&self, row: &Row, app_version: Option<&str>) -> (Vec<ui::Node>, Vec<ui::Node>) {
        let key = row.key();
        let id = |name: &str| format!("settings-module-{name}-{key}");
        let shown = |detail| self.open.contains(&(key.clone(), detail));
        let slug = row.slug.unwrap_or_default().to_owned();
        let version = row
            .installed
            .and_then(|installed| installed["version"].as_str());
        let error = row
            .installed
            .and_then(|installed| installed["error"].as_str());
        let base = if row.app() { app_version } else { version };
        let newest = row.newest().filter(|offer| {
            let offered = offer["version"].as_str().unwrap_or_default();
            match base {
                Some(base) if row.app() || row.installed.is_some() => newer(offered, base),
                _ => true,
            }
        });
        let new_version = newest
            .and_then(|offer| offer["version"].as_str())
            .unwrap_or_default()
            .to_owned();
        let unsupported = row
            .newest()
            .is_none()
            .then(|| row.offers.first()?["unsupported"].as_str())
            .flatten();
        let confirming = self
            .confirming
            .as_ref()
            .is_some_and(|(confirming, _)| *confirming == key);
        let reload = row.app() && self.installed.as_deref() == row.slug && !self.updating;
        let installed_app = row.app() && self.installed.is_some();
        let busy = self.busy.is_some() || self.updating || installed_app;
        let mut actions = Vec::new();
        let install = Msg::Install(slug.clone(), row.id.to_owned(), new_version.clone());
        if reload || (installed_app && self.installed.as_deref() == Some("")) {
            actions.push(ui::button("Reload extension", &Msg::Reload).key(id("reload")));
        } else if newest.is_some() && row.installed.is_some() {
            actions.push(
                ui::button("Update", &install)
                    .key(id("update"))
                    .disabled(busy),
            );
        } else if newest.is_some() {
            actions.push(
                ui::button("Install", &install)
                    .key(id("install"))
                    .disabled(busy),
            );
        }
        if row.installed.is_some() {
            actions.push(
                ui::button("Remove", &Msg::Remove(key.clone(), row.id.to_owned()))
                    .key(id("remove"))
                    .variant("danger")
                    .disabled(self.busy.is_some() || confirming || row.app()),
            );
        }
        let version_cell = if row.installed.is_some() {
            let button = ui::button(
                version.unwrap_or("failed"),
                &Msg::Detail(key.clone(), Detail::Notes),
            )
            .key(id("version"));
            if error.is_some() {
                button.variant("danger")
            } else {
                button
            }
        } else {
            ui::text("—").key(id("version"))
        };
        let new_cell = if newest.is_some() {
            ui::button(&new_version, &Msg::Detail(key.clone(), Detail::NewNotes))
                .key(id("new-version"))
        } else {
            ui::text("")
        };
        let cells = vec![
            ui::button(row.title(), &Msg::Detail(key.clone(), Detail::Description))
                .variant("muted")
                .key(id("name")),
            self.view_source_link(row),
            version_cell,
            new_cell,
            ui::row(actions),
        ];
        let mut extra = Vec::new();
        if let Some(error) = error {
            extra.push(
                ui::text(format!("{}: {error}", row.title()))
                    .variant("danger")
                    .key(id("error")),
            );
        }
        if let Some(reason) = unsupported {
            extra.push(
                ui::text(format!("{}: {reason}", row.title()))
                    .variant("muted")
                    .key(id("unsupported")),
            );
        }
        let detail = |name: &str, text: &str| {
            let text = if text.trim().is_empty() {
                "No details."
            } else {
                text
            };
            ui::viewer("text", text).key(id(name))
        };
        if shown(Detail::Description) {
            extra.push(detail("description", row.description()));
        }
        if shown(Detail::Notes) {
            let notes = row
                .installed
                .and_then(|installed| installed["notes"].as_str())
                .unwrap_or_default();
            extra.push(detail("notes", notes));
        }
        if shown(Detail::NewNotes) {
            let notes = newest
                .and_then(|offer| offer["notes"].as_str())
                .unwrap_or_default();
            extra.push(detail("new-notes", notes));
        }
        if confirming {
            extra.push(
                ui::column(vec![
                    ui::text(format!(
                        "Remove module \"{}\"? Its consoles, checks, history, templates and hints are kept and come back when it is installed again.",
                        row.title()
                    )),
                    ui::checkbox("Delete them too, in every session", self.forget, &Msg::Forget)
                        .key("settings-module-forget"),
                    ui::row(vec![
                        ui::button("Remove", &Msg::RemoveConfirm)
                            .key("settings-module-remove-confirm")
                            .variant("danger")
                            .disabled(self.busy.is_some()),
                        ui::button("Cancel", &Msg::RemoveCancel).key("settings-module-remove-cancel"),
                    ]),
                ])
                .key("settings-module-confirm"),
            );
        }
        (cells, extra)
    }

    fn view_sources(&self) -> Vec<ui::Node> {
        let busy = self.busy.is_some();
        let mut body = vec![ui::text(
            "Modules and shared settings (dictionary, templates, hints, skills) come from these sources. Later sources override earlier ones; your own changes override them all.",
        )
        .variant("muted")];
        if let Some(warning) = &self.warning {
            body.push(
                ui::text(warning.clone())
                    .variant("danger")
                    .key("sources-warning"),
            );
        }
        let count = self.sources.len();
        for (index, source) in self.sources.iter().enumerate() {
            body.push(self.view_source(index, source, count, busy));
        }
        if let Some(draft) = &self.draft {
            body.push(self.view_draft(draft, busy));
        }
        let mut actions = Vec::new();
        if self.draft.is_none() {
            actions.push(
                ui::button("Add source…", &Msg::Adding(true))
                    .key("source-add")
                    .disabled(busy || !self.sources_loaded),
            );
        }
        actions.push(
            ui::button(
                if self.checking {
                    "Checking…"
                } else {
                    "Check now"
                },
                &Msg::Check,
            )
            .key("sources-check")
            .disabled(self.checking),
        );
        body.push(ui::row(actions));
        body.push(ui::text("GitHub token"));
        let mut token = vec![
            ui::input(
                Some(self.token.clone()),
                if self.token_set {
                    "Saved; enter another to replace it"
                } else {
                    "Optional"
                },
                &Msg::TokenInput,
            )
            .on_input(&Msg::TokenInput)
            .variant("secret")
            .key("sources-token"),
            ui::button("Save", &Msg::Token(true))
                .key("sources-token-save")
                .disabled(busy || self.token.trim().is_empty()),
        ];
        if self.token_set {
            token.push(
                ui::button("Clear", &Msg::Token(false))
                    .key("sources-token-clear")
                    .disabled(busy),
            );
        }
        body.push(ui::row(token));
        body.push(
            ui::text("For private repositories and higher GitHub limits. Without a saved token, the token of a signed-in GitHub CLI (gh auth token) is used.")
                .variant("muted"),
        );
        vec![ui::column(body).key("source-settings")]
    }

    fn view_source(&self, index: usize, source: &Value, count: usize, busy: bool) -> ui::Node {
        let slug = text(source, "slug");
        let name = text(source, "name");
        let id = |part: &str| format!("source-{part}-{slug}");
        let mut lines = Vec::new();
        match self.renaming.as_ref().filter(|(at, _)| *at == index) {
            Some((_, input)) => lines.push(ui::row(vec![
                ui::input(Some(input.clone()), "Name", &Msg::RenameSave)
                    .on_input(&Msg::RenameInput)
                    .key(id("name-input")),
                ui::button("Save", &Msg::RenameSave)
                    .key(id("rename-save"))
                    .disabled(busy),
                ui::button("Cancel", &Msg::Rename(None)).key(id("rename-cancel")),
            ])),
            None => lines.push(ui::row(vec![
                ui::text(name.clone()).variant("primary").key(id("name")),
                ui::text(location(source)).variant("muted"),
                ui::button("↑", &Msg::Move(index, true))
                    .key(id("up"))
                    .disabled(busy || index == 0),
                ui::button("↓", &Msg::Move(index, false))
                    .key(id("down"))
                    .disabled(busy || index + 1 == count),
                ui::button("Rename", &Msg::Rename(Some(index)))
                    .key(id("rename"))
                    .disabled(busy),
                ui::button("Remove", &Msg::RemoveSource(Some(index)))
                    .key(id("remove"))
                    .variant("danger")
                    .disabled(busy || self.removing == Some(index)),
            ])),
        }
        if self.removing == Some(index) {
            lines.push(ui::row(vec![
                ui::text(format!("Remove source \"{name}\"? Its applied settings and skills are removed; modules installed from it stay.")),
                ui::button("Remove", &Msg::RemoveSourceConfirm)
                    .key(id("remove-confirm"))
                    .variant("danger")
                    .disabled(busy),
                ui::button("Cancel", &Msg::RemoveSource(None)).key(id("remove-cancel")),
            ]));
        }
        let applied = &source["applied"];
        if applied.is_object() {
            let mut text = format!("Settings applied {}", self.date(&applied["at"]));
            if let Some(revision) = applied["revision"]
                .as_str()
                .filter(|revision| !revision.is_empty())
            {
                text.push_str(&format!(
                    " · {}",
                    revision.chars().take(7).collect::<String>()
                ));
            }
            lines.push(ui::text(text).variant("muted").key(id("applied")));
        }
        lines.extend(self.view_check(&slug, busy));
        ui::column(lines).key(format!("source-{slug}"))
    }

    fn view_check(&self, slug: &str, busy: bool) -> Vec<ui::Node> {
        let id = |part: &str| format!("source-{part}-{slug}");
        let checked = self
            .check
            .as_ref()
            .and_then(|check| check["sources"].as_array())
            .and_then(|sources| sources.iter().find(|source| source["slug"] == slug));
        let Some(checked) = checked else {
            let state = if self.checking {
                "Checking…"
            } else {
                "Not checked yet"
            };
            return vec![ui::text(state).variant("muted")];
        };
        let mut lines = vec![
            ui::text(format!("Checked {}", self.date(&checked["checkedAt"])))
                .variant("muted")
                .key(id("checked")),
        ];
        if let Some(error) = checked["error"].as_str() {
            lines.push(ui::text(error).variant("danger").key(id("error")));
        }
        let changes = checked["changes"].as_array().cloned().unwrap_or_default();
        let applied = checked["applied"] == true;
        if checked["configs"] == true && (!applied || !changes.is_empty()) {
            let mut list = vec![ui::text(if applied {
                "Settings changed:"
            } else {
                "Settings not applied yet:"
            })];
            for change in &changes {
                list.push(ui::text(format!(
                    "{} {} {}",
                    text(change, "kind"),
                    text(change, "key"),
                    text(change, "change")
                )));
            }
            list.push(
                ui::button("Apply", &Msg::Apply(slug.to_owned()))
                    .key(id("apply"))
                    .variant("primary")
                    .disabled(busy),
            );
            lines.push(ui::column(list).key(id("changes")));
        } else if checked["configs"] == true {
            lines.push(
                ui::text("Settings up to date")
                    .variant("muted")
                    .key(id("up-to-date")),
            );
        }
        let problems: Vec<ui::Node> = checked["problems"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|problem| ui::text(problem.as_str().unwrap_or_default()).variant("danger"))
            .collect();
        if !problems.is_empty() {
            lines.push(ui::column(problems).key(id("problems")));
        }
        lines
    }

    fn view_draft(&self, draft: &Draft, busy: bool) -> ui::Node {
        let field = |field: Field, key: &str, label: &str, value: &str, placeholder: &str| {
            ui::row(vec![
                ui::text(label),
                ui::input(Some(value.to_owned()), placeholder, &Msg::Field(field))
                    .on_input(&Msg::Field(field))
                    .key(format!("source-add-{key}")),
            ])
        };
        let github = draft.kind != "folder";
        let mut lines = vec![
            field(Field::Name, "name", "Name", &draft.name, "Team"),
            ui::row(vec![
                ui::text("Kind"),
                ui::select(
                    vec![
                        ui::Choice {
                            value: "github".into(),
                            label: "GitHub repository".into(),
                        },
                        ui::Choice {
                            value: "folder".into(),
                            label: "Folder".into(),
                        },
                    ],
                    draft.kind.clone(),
                    &Msg::Field(Field::Kind),
                )
                .key("source-add-kind"),
            ]),
        ];
        if github {
            lines.push(field(
                Field::Repository,
                "repository",
                "Repository",
                &draft.repository,
                "owner/repo",
            ));
            lines.push(field(
                Field::Branch,
                "branch",
                "Branch",
                &draft.branch,
                "default branch",
            ));
            lines.push(field(
                Field::Path,
                "path",
                "Folder in it",
                &draft.path,
                "repository root",
            ));
        } else {
            lines.push(field(
                Field::Path,
                "path",
                "Folder",
                &draft.path,
                "/path/to/folder",
            ));
        }
        lines.push(ui::row(vec![
            ui::button("Add", &Msg::Add)
                .key("source-add-save")
                .variant("primary")
                .disabled(busy),
            ui::button("Cancel", &Msg::Adding(false)).key("source-add-cancel"),
        ]));
        ui::column(lines).key("source-add-form")
    }

    fn view_agent(&self) -> Vec<ui::Node> {
        let mut body = vec![ui::text("New Agent chats").variant("primary")];
        let Some(catalog) = &self.catalog else {
            body.push(
                ui::text(if self.loading_models {
                    "Loading Copilot models…"
                } else {
                    "Copilot models are not loaded."
                })
                .variant("muted")
                .key("agent-models-loading"),
            );
            return vec![ui::column(body).key("agent-settings")];
        };
        let current = self.model_id();
        let mut choices: Vec<ui::Choice> = catalog
            .models
            .iter()
            .filter(|model| model.enabled)
            .map(|model| ui::Choice {
                value: model.id.clone(),
                label: format!("{} · {}", model.name, category(model)),
            })
            .collect();
        if !choices.iter().any(|choice| choice.value == current) {
            choices.insert(
                0,
                ui::Choice {
                    value: current.clone(),
                    label: if current == FUSION_MODEL_ID {
                        FUSION_MODEL_NAME.into()
                    } else {
                        current.clone()
                    },
                },
            );
        }
        body.push(ui::text("Copilot model"));
        let mut model = vec![ui::select(choices, current, &Msg::Model).key("agent-model-input")];
        if let Some(model_info) = self.current() {
            model.push(
                ui::text(category(model_info))
                    .variant("muted")
                    .key("agent-model-category"),
            );
        }
        body.push(ui::row(model));
        let efforts = self.efforts();
        let level = self.reasoning_level();
        let value = match level {
            Some(index) => efforts[index].clone(),
            None if efforts.is_empty() => "Not supported".into(),
            None => "Default".into(),
        };
        let is_default =
            level.is_some_and(|index| Some(efforts[index].as_str()) == self.default_effort());
        body.push(ui::row(vec![
            ui::text("Reasoning"),
            ui::text(if is_default {
                format!("{value} · default")
            } else {
                value
            })
            .key("agent-reasoning-value"),
        ]));
        if !efforts.is_empty() {
            body.push(
                ui::slider(
                    0.0,
                    efforts.len().saturating_sub(1) as f64,
                    1.0,
                    level.unwrap_or_default() as f64,
                    &Msg::Reasoning,
                )
                .key("agent-reasoning"),
            );
            body.push(ui::row(
                efforts
                    .iter()
                    .enumerate()
                    .map(|(index, effort)| {
                        let button = ui::button(effort, &Msg::ReasoningLevel(index))
                            .key(format!("agent-reasoning-level-{effort}"));
                        if level == Some(index) {
                            button.variant("primary")
                        } else {
                            button
                        }
                    })
                    .collect(),
            ));
        }
        vec![ui::column(body).key("agent-settings")]
    }
}
