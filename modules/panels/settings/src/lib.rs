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

use services_sdk::Failure;

const MODULES: &str = "settings-tab-modules";
const SOURCES: &str = "settings-tab-sources";
const AGENT: &str = "settings-tab-agent";
const TABS: [&str; 3] = [MODULES, SOURCES, AGENT];
/// The kv key of the tab shown last.
const TAB: &str = "tab";
const CHECK: &str = "check";
const LIST: &str = "sources";
const MODELS: &str = "models";
const CHANGE: &str = "change";
const SYNC: &str = "sync";
const DIFF: &str = "diff";
const RESOLVE: &str = "resolve";
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
    /// Syncs a shared source with Local, opening Diff for each conflict.
    Sync(String),
    Model,
    Reasoning,
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
    /// The light of each tab: the state of its last operation.
    lights: [&'static str; 3],
    /// The tab a forced check was asked on.
    check_tab: &'static str,
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
    /// The conflicts of a sync still to resolve in Diff.
    resolving: Option<Resolving>,
    draft: Option<Draft>,
    token: String,
    catalog: Option<Catalog>,
    options: Options,
    loading_models: bool,
}

/// A sync whose conflicts are settled one by one in Diff.
struct Resolving {
    slug: String,
    name: String,
    /// The conflicts not opened yet: `{file, left, right}`.
    queue: Vec<Value>,
    /// The file being compared.
    file: String,
    /// A source changed while its conflict was resolved.
    stale: Vec<String>,
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
        if source["skills"] == true && (source["applied"] != true || changed) {
            updates.push(format!("skills of {name}"));
        }
        if ["source", "both", "conflict"].contains(&source["sync"]["state"].as_str().unwrap_or(""))
        {
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
    fn tab_name(&self) -> &'static str {
        TABS.into_iter()
            .find(|tab| *tab == self.tab)
            .unwrap_or(MODULES)
    }

    fn status(&self) -> Cmd {
        match &self.status {
            Some((text, indicator)) => cmd::status(text, indicator),
            None => cmd::status("", "idle"),
        }
    }

    /// Reports an operation on the status line and the light of the tab it belongs to.
    fn report(&mut self, tab: &'static str, text: String, indicator: &'static str) -> Cmd {
        if let Some(light) = TABS.iter().position(|known| *known == tab) {
            self.lights[light] = indicator;
        }
        self.status = Some((text, indicator));
        self.status()
    }

    fn running(&mut self, tab: &'static str, operation: &str) -> Cmd {
        self.report(tab, format!("{operation} · Running…"), "busy")
    }

    fn date(&self, seconds: &Value) -> String {
        time::date(seconds.as_f64().unwrap_or_default() * 1e3, self.offset)
    }

    fn check(&mut self, force: bool) -> Vec<Cmd> {
        self.generation += 1;
        self.checking = true;
        self.forced = force;
        if force {
            self.check_tab = self.tab_name();
        }
        let mut commands = vec![cmd::modules_check(
            format!("{CHECK}:{}", self.generation),
            force,
        )];
        if force {
            commands.push(self.running(self.check_tab, "Check sources"));
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
            // Automatic checks report source problems on Sources, updates on Modules.
            let tab = match (self.forced, indicator) {
                (true, _) => self.check_tab,
                (false, "warning" | "error") => SOURCES,
                (false, _) => MODULES,
            };
            vec![self.report(tab, text, indicator)]
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
            commands.push(self.running(AGENT, "Load Copilot models"));
        }
        commands
    }

    fn change(&mut self, operation: &str, command: Cmd, forgetting: Option<String>) -> Vec<Cmd> {
        self.busy = Some((operation.to_owned(), forgetting));
        vec![command, self.running(operation_tab(operation), operation)]
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
        commands.push(self.report(operation_tab(&operation), text, indicator));
        commands
    }

    /// Checks the sources again without announcing it.
    fn refresh(&mut self) -> Vec<Cmd> {
        self.generation += 1;
        self.checking = true;
        self.forced = false;
        vec![cmd::modules_check(
            format!("{CHECK}:{}", self.generation),
            true,
        )]
    }

    /// The sync applied what it could: the conflicts go to Diff one by one.
    fn synced(&mut self, result: Result<Value, Failure>) -> Vec<Cmd> {
        self.busy = None;
        let value = match result {
            Ok(value) => value,
            Err(failure) => {
                self.resolving = None;
                let (text, indicator) = outcome("Sync source", &Err(failure));
                return vec![self.report(SOURCES, text, indicator)];
            }
        };
        let conflicts = value["conflicts"].as_array().cloned().unwrap_or_default();
        let errors: Vec<&str> = value["errors"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if let Some(resolving) = &mut self.resolving {
            resolving.queue = conflicts;
            resolving.stale = errors.iter().map(|error| (*error).to_owned()).collect();
        }
        if self.resolving.as_ref().is_some_and(|r| !r.queue.is_empty()) {
            return self.next_conflict();
        }
        self.sync_done()
    }

    /// Opens the next conflict in Diff, Local on the left and the source on the right.
    fn next_conflict(&mut self) -> Vec<Cmd> {
        let Some(resolving) = &mut self.resolving else {
            return Vec::new();
        };
        let conflict = resolving.queue.remove(0);
        resolving.file = text(&conflict, "file");
        let language = if resolving.file.ends_with(".json") {
            "json"
        } else {
            ""
        };
        let open = cmd::diff_open(
            DIFF,
            conflict["left"].as_str().unwrap_or_default(),
            conflict["right"].as_str().unwrap_or_default(),
            "Local",
            &resolving.name,
            language,
        );
        let status = format!("Sync source · Resolve {} in Diff", resolving.file);
        vec![open, self.report(SOURCES, status, "busy")]
    }

    /// Diff answered: Done writes the settled text to both sides; Stop leaves the conflicts.
    fn compared(&mut self, result: Result<Value, Failure>) -> Vec<Cmd> {
        let Some(resolving) = &self.resolving else {
            return Vec::new();
        };
        match result {
            Ok(value) => {
                let command = cmd::sources_resolve(
                    RESOLVE,
                    &resolving.slug,
                    &resolving.file,
                    value["text"].as_str().unwrap_or_default(),
                );
                self.busy = Some(("Sync source".into(), None));
                vec![command]
            }
            Err(_) => {
                let name = resolving.name.clone();
                self.resolving = None;
                let mut commands = vec![self.report(
                    SOURCES,
                    format!("Sync source · Stopped · conflicts with {name} stay"),
                    "warning",
                )];
                commands.extend(self.refresh());
                commands
            }
        }
    }

    fn resolved(&mut self, result: Result<Value, Failure>) -> Vec<Cmd> {
        self.busy = None;
        let Some(resolving) = &mut self.resolving else {
            return Vec::new();
        };
        match result {
            Ok(value) => {
                if value["written"] == false {
                    resolving.stale.push(format!(
                        "{} changed in {} meanwhile; check and sync again",
                        resolving.file, resolving.name
                    ));
                }
            }
            Err(failure) => resolving.stale.push(failure.message),
        }
        if resolving.queue.is_empty() {
            self.sync_done()
        } else {
            self.next_conflict()
        }
    }

    fn sync_done(&mut self) -> Vec<Cmd> {
        let problems = self
            .resolving
            .take()
            .map(|resolving| resolving.stale)
            .unwrap_or_default();
        let (text, indicator) = if problems.is_empty() {
            ("Sync source · OK".to_owned(), "ok")
        } else {
            (
                format!("Sync source · WARNING · {}", problems.join("; ")),
                "warning",
            )
        };
        let mut commands = vec![self.report(SOURCES, text, indicator)];
        commands.extend(self.refresh());
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
                    self.running(AGENT, "New Agent chat settings"),
                ]
            }
            Err(error) => vec![self.report(
                AGENT,
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
            Ok(()) => self.report(AGENT, "Load Copilot models · OK".into(), "ok"),
            Err(error) => self.report(
                AGENT,
                format!("Load Copilot models · ERROR · {error}"),
                "error",
            ),
        }]
    }
}

/// The tab an operation belongs to.
fn operation_tab(operation: &str) -> &'static str {
    match operation {
        "Install module" | "Remove module" => MODULES,
        _ => SOURCES,
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
        let light = |tab: &str| {
            TABS.iter()
                .position(|known| *known == tab)
                .map_or("idle", |index| match self.lights[index] {
                    "" => "idle",
                    light => light,
                })
        };
        // Without the native host there are no modules and sources to manage.
        let native = !matches!(self.check, Some(Value::Null));
        let mut tabs = Vec::new();
        if native {
            tabs.push(ui::tab(MODULES, "Modules", self.view_modules()).indicator(light(MODULES)));
            tabs.push(ui::tab(SOURCES, "Sources", self.view_sources()).indicator(light(SOURCES)));
        }
        tabs.push(ui::tab(AGENT, "Agent", self.view_agent()).indicator(light(AGENT)));
        let active = if native {
            self.tab.clone()
        } else {
            AGENT.into()
        };
        Some(
            ui::tabbed_panel(
                ui::tabs(tabs, active, &Msg::Tab).key("settings-tabs"),
                Vec::new(),
                Vec::new(),
            )
            .variant("padded")
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
            SYNC => self.synced(result),
            DIFF => self.compared(result),
            RESOLVE => self.resolved(result),
            OPEN => match result {
                Ok(_) => Vec::new(),
                Err(failure) => vec![self.report(
                    MODULES,
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
                        Some(warning) => vec![self.report(
                            MODULES,
                            format!("Install update · ERROR · {warning}"),
                            "error",
                        )],
                        None => vec![self.report(
                            MODULES,
                            "Update installed. Reload the extension to finish.".into(),
                            "ok",
                        )],
                    }
                }
                Err(failure) => {
                    self.updating = false;
                    self.installed = None;
                    vec![self.report(
                        MODULES,
                        format!("Install update · ERROR · {}", failure.message),
                        "error",
                    )]
                }
            },
            MODELS => self.models_loaded(result),
            SAVE_OPTIONS => {
                let (text, indicator) = outcome("New Agent chat settings", &result);
                vec![self.report(AGENT, text, indicator)]
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
                        self.running(MODULES, "Install update"),
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
                "Apply source skills",
                cmd::sources_apply(CHANGE, &slug),
                None,
            ),
            Msg::Sync(slug) => {
                if self.busy.is_some() {
                    return Vec::new();
                }
                self.resolving = Some(Resolving {
                    name: self
                        .sources
                        .iter()
                        .find(|source| source["slug"] == slug.as_str())
                        .map(|source| text(source, "name"))
                        .unwrap_or_else(|| slug.clone()),
                    slug: slug.clone(),
                    queue: Vec::new(),
                    file: String::new(),
                    stale: Vec::new(),
                });
                self.busy = Some(("Sync source".into(), None));
                vec![
                    cmd::sources_sync(SYNC, &slug),
                    self.running(SOURCES, "Sync source"),
                ]
            }
            Msg::Model => self.choose(Options {
                model: Some(value),
                reasoning_effort: None,
            }),
            Msg::Reasoning => match value.parse::<f64>() {
                Ok(level) if level >= 0.0 => self.reasoning(level as usize),
                _ => Vec::new(),
            },
        }
    }

    fn view_modules(&self) -> Vec<ui::Node> {
        let mut body = vec![
            ui::row(vec![
                ui::heading("Modules").key("settings-modules-title"),
                ui::spacer().key("settings-modules-spacer"),
                ui::button("Check for updates", &Msg::Check)
                    .variant("tab")
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
        let table = rows
            .iter()
            .map(|row| {
                let (cells, details) = self.view_row(row, app_version);
                ui::TableRow::new(format!("settings-module-{}", row.key()), cells).detail(details)
            })
            .collect();
        body.push(
            ui::table(
                ["Name", "Source", "Version", "New version", ""]
                    .map(str::to_owned)
                    .to_vec(),
                table,
            )
            .widths(vec![26, 14, 11, 13, 0])
            .variant("roomy")
            .key("settings-modules"),
        );
        body
    }

    fn view_source_link(&self, row: &Row) -> ui::Node {
        let Some(source) = row.source else {
            return ui::text("—").key(format!("settings-module-source-{}", row.key()));
        };
        let name = text(source, "name");
        if source["kind"] == "folder" {
            let slug = row.slug.unwrap_or_default().to_owned();
            return ui::link(name, &Msg::OpenSource(slug))
                .title(text(source, "path"))
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
        ui::anchor(name, url).key(format!("settings-module-source-{}", row.key()))
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
            actions.push(
                ui::button("Reload extension", &Msg::Reload)
                    .variant("tab")
                    .key(id("reload")),
            );
        } else if newest.is_some() && row.installed.is_some() {
            actions.push(
                ui::button("Update", &install)
                    .variant("tab")
                    .key(id("update"))
                    .disabled(busy),
            );
        } else if newest.is_some() {
            actions.push(
                ui::button("Install", &install)
                    .variant("tab")
                    .key(id("install"))
                    .disabled(busy),
            );
        }
        if row.installed.is_some() {
            let remove = ui::button("Remove", &Msg::Remove(key.clone(), row.id.to_owned()))
                .variant("tab")
                .key(id("remove"))
                .disabled(self.busy.is_some() || confirming || row.app());
            actions.push(if row.app() {
                remove.title("The application can't be removed")
            } else {
                remove
            });
        }
        let version_cell = if row.installed.is_some() {
            let link = ui::link(
                version.unwrap_or("failed"),
                &Msg::Detail(key.clone(), Detail::Notes),
            )
            .title("Release notes")
            .key(id("version"));
            if error.is_some() {
                link.variant("link danger")
            } else {
                link
            }
        } else {
            ui::text("—").key(id("version"))
        };
        let new_cell = if newest.is_some() {
            ui::link(&new_version, &Msg::Detail(key.clone(), Detail::NewNotes))
                .title("Release notes")
                .key(id("new-version"))
        } else {
            ui::text("")
        };
        let cells = vec![
            ui::link(row.title(), &Msg::Detail(key.clone(), Detail::Description))
                .title("Description")
                .key(id("name")),
            self.view_source_link(row),
            version_cell,
            new_cell,
            ui::row(actions).variant("end nowrap"),
        ];
        let mut extra = Vec::new();
        if let Some(error) = error {
            extra.push(ui::text(error).variant("danger").key(id("error")));
        }
        if let Some(reason) = unsupported {
            extra.push(ui::text(reason).variant("muted").key(id("unsupported")));
        }
        let detail = |name: &str, text: &str| {
            let text = if text.trim().is_empty() {
                "No details."
            } else {
                text
            };
            ui::text(text).variant("muted").key(id(name))
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
                    ))
                    .variant("muted"),
                    ui::row(vec![
                        ui::checkbox(
                            "Delete them too, in every session",
                            self.forget,
                            &Msg::Forget,
                        )
                        .key("settings-module-forget"),
                        ui::spacer(),
                        ui::button("Remove", &Msg::RemoveConfirm)
                            .variant("tab")
                            .key("settings-module-remove-confirm")
                            .disabled(self.busy.is_some()),
                        ui::button("Cancel", &Msg::RemoveCancel)
                            .variant("tab")
                            .key("settings-module-remove-cancel"),
                    ]),
                ])
                .key("settings-module-confirm"),
            );
        }
        (cells, extra)
    }

    fn view_sources(&self) -> Vec<ui::Node> {
        let busy = self.busy.is_some();
        let mut body = vec![ui::heading("Sources"), ui::text(
            "Modules and skills come from these sources. Dictionary, Templates and Hints are kept in Local, saved as you edit, and synced with each shared source: sync copies changes both ways, and conflicts open in Diff.",
        )
        .variant("muted")];
        if let Some(warning) = &self.warning {
            body.push(
                ui::text(warning.clone())
                    .variant("warning")
                    .key("sources-warning"),
            );
        }
        body.push(
            ui::column(vec![
                ui::text("Local").variant("bold").key("source-local-name"),
                ui::text(
                    "Your own Dictionary, Templates and Hints; it can't be removed or configured.",
                )
                .variant("muted"),
            ])
            .key("source-local"),
        );
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
                    .variant("tab")
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
            .variant("tab")
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
                .variant("tab")
                .key("sources-token-save")
                .disabled(busy || self.token.trim().is_empty()),
        ];
        if self.token_set {
            token.push(
                ui::button("Clear", &Msg::Token(false))
                    .variant("tab")
                    .key("sources-token-clear")
                    .disabled(busy),
            );
        }
        body.push(ui::row(token));
        body.push(
            ui::text("For private repositories and higher GitHub limits. Without a saved token, the token of a signed-in GitHub CLI (gh auth token) is used.")
                .variant("muted"),
        );
        vec![ui::column(body).variant("loose").key("source-settings")]
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
                    .variant("tab")
                    .key(id("rename-save"))
                    .disabled(busy),
                ui::button("Cancel", &Msg::Rename(None))
                    .variant("tab")
                    .key(id("rename-cancel")),
            ])),
            None => lines.push(ui::row(vec![
                ui::column(vec![
                    ui::text(name.clone()).variant("bold").key(id("name")),
                    ui::text(location(source))
                        .variant("muted truncate")
                        .title(location(source)),
                ])
                .variant("grow"),
                ui::button("↑", &Msg::Move(index, true))
                    .variant("tab")
                    .title("Move up")
                    .label(format!("Move {name} up"))
                    .key(id("up"))
                    .disabled(busy || index == 0),
                ui::button("↓", &Msg::Move(index, false))
                    .variant("tab")
                    .title("Move down")
                    .label(format!("Move {name} down"))
                    .key(id("down"))
                    .disabled(busy || index + 1 == count),
                ui::button("Rename", &Msg::Rename(Some(index)))
                    .variant("tab")
                    .key(id("rename"))
                    .disabled(busy),
                ui::button("Remove", &Msg::RemoveSource(Some(index)))
                    .variant("tab")
                    .key(id("remove"))
                    .disabled(busy || self.removing == Some(index)),
            ])),
        }
        if self.removing == Some(index) {
            lines.push(ui::column(vec![
                ui::text(format!("Remove source \"{name}\"? Its applied settings and skills are removed; modules installed from it stay."))
                    .variant("muted"),
                ui::row(vec![
                    ui::button("Remove", &Msg::RemoveSourceConfirm)
                        .variant("tab")
                        .key(id("remove-confirm"))
                        .disabled(busy),
                    ui::button("Cancel", &Msg::RemoveSource(None))
                        .variant("tab")
                        .key(id("remove-cancel")),
                ]),
            ]));
        }
        let applied = &source["applied"];
        if applied.is_object() {
            let mut text = format!("Skills applied {}", self.date(&applied["at"]));
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
        let sync = &checked["sync"];
        let state = sync["state"].as_str().unwrap_or("same");
        let name = text(checked, "name");
        let (note, button) = match state {
            "local" => (format!("Changes in Local not in {name} yet"), "Sync"),
            "source" => (format!("Changes in {name} not in Local yet"), "Sync"),
            "both" => (format!("Changes in Local and in {name}"), "Sync"),
            "conflict" => (
                format!("Conflicts between Local and {name}"),
                "Resolve conflicts…",
            ),
            _ => (String::new(), ""),
        };
        if button.is_empty() {
            lines.push(
                ui::text("Local and this source are in sync")
                    .variant("muted")
                    .key(id("in-sync")),
            );
        } else {
            let files: Vec<String> = sync["files"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|file| text(file, "file"))
                .collect();
            lines.push(
                ui::row(vec![
                    ui::text(format!("{note}: {}", files.join(", ")))
                        .variant(if state == "conflict" {
                            "danger"
                        } else {
                            "muted"
                        })
                        .key(id("sync-state")),
                    ui::button(button, &Msg::Sync(slug.to_owned()))
                        .variant("tab")
                        .key(id("sync"))
                        .disabled(busy),
                ])
                .key(id("sync-row")),
            );
        }
        if checked["skills"] == true && (!applied || !changes.is_empty()) {
            let mut list = vec![ui::text(if applied {
                "Skills changed:"
            } else {
                "Skills not applied yet:"
            })];
            for change in &changes {
                let line = vec![ui::text(format!(
                    "{} {} {}",
                    text(change, "kind"),
                    text(change, "key"),
                    text(change, "change")
                ))];
                list.push(ui::column(vec![ui::row(line)]).variant("indent"));
            }
            list.push(ui::row(vec![
                ui::button("Apply", &Msg::Apply(slug.to_owned()))
                    .variant("tab")
                    .key(id("apply"))
                    .disabled(busy),
            ]));
            lines.push(ui::column(list).key(id("changes")));
        } else if checked["skills"] == true {
            lines.push(
                ui::text("Skills up to date")
                    .variant("muted")
                    .key(id("up-to-date")),
            );
        }
        let problems: Vec<ui::Node> = checked["problems"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|problem| ui::text(problem.as_str().unwrap_or_default()).variant("warning"))
            .collect();
        if !problems.is_empty() {
            lines.push(ui::column(problems).key(id("problems")));
        }
        lines
    }

    fn view_draft(&self, draft: &Draft, busy: bool) -> ui::Node {
        let field = |field: Field, key: &str, label: &str, value: &str, placeholder: &str| {
            ui::field(
                label,
                ui::input(Some(value.to_owned()), placeholder, &Msg::Field(field))
                    .on_input(&Msg::Field(field))
                    .key(format!("source-add-{key}")),
            )
            .variant("grid")
        };
        let github = draft.kind != "folder";
        let mut lines = vec![
            field(Field::Name, "name", "Name", &draft.name, "Team"),
            ui::field(
                "Kind",
                ui::select(
                    vec![
                        ui::Choice::new("github", "GitHub repository"),
                        ui::Choice::new("folder", "Folder"),
                    ],
                    draft.kind.clone(),
                    &Msg::Field(Field::Kind),
                )
                .key("source-add-kind"),
            )
            .variant("grid"),
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
                .variant("tab")
                .key("source-add-save")
                .disabled(busy),
            ui::button("Cancel", &Msg::Adding(false))
                .variant("tab")
                .key("source-add-cancel"),
        ]));
        ui::column(lines).variant("loose").key("source-add-form")
    }

    fn view_agent(&self) -> Vec<ui::Node> {
        let mut body = vec![ui::heading("New Agent chats")];
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
            return vec![ui::column(body).variant("loose").key("agent-settings")];
        };
        let current = self.model_id();
        let mut choices: Vec<ui::Choice> = catalog
            .models
            .iter()
            .filter(|model| model.enabled)
            .map(|model| {
                ui::Choice::new(model.id.clone(), model.name.clone()).detail(category(model))
            })
            .collect();
        if !choices.iter().any(|choice| choice.value == current) {
            let label = if current == FUSION_MODEL_ID {
                FUSION_MODEL_NAME.to_owned()
            } else {
                current.clone()
            };
            choices.insert(0, ui::Choice::new(current.clone(), label));
        }
        body.push(
            ui::combobox(choices, current, &Msg::Model)
                .empty_text("No matching models")
                .label("Model")
                .key("agent-model-input"),
        );
        let efforts = self.efforts();
        let level = self.reasoning_level();
        let value = match level {
            Some(index) => efforts[index].clone(),
            None if efforts.is_empty() => "Not supported".into(),
            None => "Default".into(),
        };
        let default_index = self
            .default_effort()
            .and_then(|default| efforts.iter().position(|effort| effort == default));
        let mut value = vec![ui::text(value).variant("bold")];
        if level.is_some() && level == default_index {
            value.push(ui::text(" · default").variant("muted small"));
        }
        body.push(ui::row(vec![
            ui::text("Reasoning"),
            ui::spacer(),
            ui::row(value)
                .variant("nowrap")
                .key("agent-reasoning-value"),
        ]));
        let mut slider = ui::slider(
            0.0,
            efforts.len().saturating_sub(1) as f64,
            1.0,
            level.unwrap_or_default() as f64,
            &Msg::Reasoning,
        )
        .labels(efforts.to_vec())
        .disabled(efforts.is_empty())
        .key("agent-reasoning");
        if let Some(index) = default_index {
            slider = slider.marker(index as u32, "Model default");
        }
        body.push(slider);
        vec![ui::column(body).variant("loose").key("agent-settings")]
    }
}
