//! The Functions panel: the jq definitions `{{$name(args)}}` calls, per service and global.
//! They are the Local source's `functions.json` (`{service: text}`, the Global tab under `*`),
//! saved as they are edited. A call in a service sees the Global definitions followed by the
//! service's. Every edit is saved; one that does not compile only shows its error in the status.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use services_sdk::{Cmd, Event, Module, Subscriptions, cmd, ui};
use std::collections::BTreeMap;

const SOURCES: &str = "local";
const DOCUMENT: &str = "document";
const FILE: &str = "functions.json";
const TAB: &str = "functions-tab-";
/// The service of the global definitions.
const ALL: &str = "*";
/// The tab (and selection) of the global definitions, stored under `*`.
const GLOBAL: &str = "@global";

/// The definitions of each service; `*` holds the global ones.
pub type Library = BTreeMap<String, String>;

#[derive(Serialize, Deserialize)]
pub enum Msg {
    Tab(String),
    Edit,
}

#[derive(Default)]
pub struct Functions {
    service: String,
    /// The selected tab of each service.
    selected: BTreeMap<String, String>,
    /// Every service's definitions, as saved.
    library: Library,
}

impl Functions {
    /// Takes the service and the Local file (`[{source, text}]`) directly (tests).
    pub fn with(service: &str, files: Value) -> Self {
        let mut functions = Self {
            service: service.into(),
            ..Self::default()
        };
        functions.read(&files);
        functions
    }

    fn read(&mut self, files: &Value) {
        self.library = files
            .as_array()
            .and_then(|files| files.last())
            .and_then(|file| serde_json::from_str(file["text"].as_str()?).ok())
            .unwrap_or_default();
    }

    /// Every service's definitions.
    pub fn library(&self) -> Library {
        self.library.clone()
    }

    /// The tabs: the service, then the global one.
    fn tabs(&self) -> Vec<String> {
        let mut tabs = Vec::new();
        if !self.service.is_empty() {
            tabs.push(self.service.clone());
        }
        tabs.push(GLOBAL.to_owned());
        tabs
    }

    fn title(tab: &str) -> String {
        match tab {
            GLOBAL => "Global".to_owned(),
            _ => tab.to_owned(),
        }
    }

    /// The selected tab.
    fn tab(&self) -> String {
        let tabs = self.tabs();
        self.selected
            .get(&self.service)
            .filter(|tab| tabs.contains(tab))
            .or(tabs.first())
            .cloned()
            .unwrap_or_default()
    }

    /// The library service the selected tab edits.
    fn scope(&self) -> String {
        match self.tab().as_str() {
            GLOBAL => ALL.to_owned(),
            tab => tab.to_owned(),
        }
    }

    fn saved(&self, service: &str) -> &str {
        self.library.get(service).map_or("", String::as_str)
    }

    fn shown(&self) -> String {
        self.saved(&self.scope()).to_owned()
    }

    fn contribution(&self) -> Cmd {
        cmd::contribute(&json!({ "functions": self.library }))
    }

    fn save(&self) -> Cmd {
        let text = serde_json::to_string_pretty(&self.library).unwrap_or_default();
        cmd::sources_write("save", FILE, &text)
    }

    fn edit(&mut self, text: String) -> Vec<Cmd> {
        let scope = self.scope();
        self.library.insert(scope, text);
        let mut commands = vec![self.save(), self.contribution()];
        commands.extend(self.problem());
        commands
    }

    /// The first error of the Global definitions, or of the service's following them, with
    /// the tab and the line in it.
    fn error(&self) -> Option<String> {
        let global = self.saved(ALL);
        if let Err(error) = services_jq::check(global) {
            return Some(format!("Global: {error}"));
        }
        let own = self.saved(&self.service);
        if self.service.is_empty() || own.trim().is_empty() {
            return None;
        }
        let offset = global.matches('\n').count() + 1;
        let error = services_jq::check(&format!("{global}\n{own}")).err()?;
        Some(format!("{}: {}", self.service, shift_lines(&error, offset)))
    }

    fn problem(&self) -> Option<Cmd> {
        let error = self.error()?;
        Some(cmd::status(&format!("ERROR: {error}"), "error"))
    }

    /// The status of the panel when nothing happened yet: its error, or how many functions
    /// the current service sees.
    fn idle(&self) -> Cmd {
        self.problem().unwrap_or_else(|| {
            let mut defs = self.saved(ALL).to_owned();
            if !self.service.is_empty() {
                defs = format!("{defs}\n{}", self.saved(&self.service));
            }
            let count = services_jq::names(&defs).len();
            let noun = if count == 1 { "function" } else { "functions" };
            cmd::status(&format!("{count} {noun}"), "idle")
        })
    }
}

/// `Line N` of the definitions after the first `offset` lines becomes `Line N - offset`.
fn shift_lines(error: &str, offset: usize) -> String {
    let mut out = String::new();
    let mut rest = error;
    while let Some(at) = rest.find("Line ") {
        let (before, after) = rest.split_at(at + "Line ".len());
        out.push_str(before);
        let digits = after.chars().take_while(char::is_ascii_digit).count();
        match after[..digits].parse::<usize>() {
            Ok(line) if line > offset => out.push_str(&(line - offset).to_string()),
            _ => out.push_str(&after[..digits]),
        }
        rest = &after[digits..];
    }
    out.push_str(rest);
    out
}

impl Module for Functions {
    type Msg = Msg;

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            document: true,
            workspace: true,
            ..Default::default()
        }
    }

    fn update(&mut self, event: Event<Msg>) -> Vec<Cmd> {
        match event {
            Event::Init { .. } | Event::Workspace => vec![
                cmd::document_read(DOCUMENT),
                cmd::sources_read(SOURCES, FILE),
            ],
            Event::Shown => vec![cmd::document_read(DOCUMENT), self.idle()],
            Event::Document(check) if check.service != self.service => {
                vec![cmd::document_read(DOCUMENT)]
            }
            Event::Reply { tag, result } => match (tag.as_str(), result) {
                (DOCUMENT, Ok(document)) => {
                    self.service = document["service"].as_str().unwrap_or_default().to_owned();
                    Vec::new()
                }
                (SOURCES, Ok(files)) => {
                    self.read(&files);
                    vec![self.contribution()]
                }
                ("save", Ok(_)) => vec![
                    self.problem()
                        .unwrap_or_else(|| cmd::status("Functions saved · OK", "ok")),
                ],
                (_, Err(failure)) => {
                    vec![cmd::status(&format!("ERROR: {}", failure.message), "error")]
                }
                _ => Vec::new(),
            },
            Event::Ui {
                msg: Msg::Tab(_),
                value: Some(tab),
            } => {
                let tab = tab.trim_start_matches(TAB).to_owned();
                self.selected.insert(self.service.clone(), tab);
                vec![self.idle()]
            }
            Event::Ui {
                msg: Msg::Edit,
                value: Some(text),
            } if text != self.shown() => self.edit(text),
            _ => Vec::new(),
        }
    }

    fn view(&self) -> Option<ui::Node> {
        let selected = self.tab();
        let body = vec![
            ui::editor("", self.shown(), &Msg::Edit)
                .key("functions-editor")
                .label(format!("Functions · {}", Self::title(&selected)))
                .debounce(500),
        ];
        let tabs = self
            .tabs()
            .iter()
            .map(|name| {
                let body = if *name == selected {
                    body.clone()
                } else {
                    Vec::new()
                };
                ui::tab(format!("{TAB}{name}"), Self::title(name), body)
            })
            .collect();
        let tabs = ui::tabs(tabs, format!("{TAB}{selected}"), &Msg::Tab(String::new()))
            .key("functions-tabs");
        Some(ui::tabbed_panel(tabs, Vec::new(), Vec::new()))
    }
}

services_sdk::export!(Functions);
