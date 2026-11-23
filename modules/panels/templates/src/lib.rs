//! The Templates panel: per service and section, `@name(params) description` templates that
//! completion inserts. They are the Local source's `templates.json`
//! (`{service: {section: text}}`), saved as they are edited; sources sync with it in
//! Settings. Service and section `*` (the Global tab) hold the templates every live call
//! `{{@name(...)}}` may use. Every edit is saved; a definition with an error only shows it
//! in the status and is skipped until fixed.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use services_contracts::templates::{self, Library};
use services_sdk::{Cmd, Event, Module, Subscriptions, cmd, ui};
use std::collections::BTreeMap;

const SOURCES: &str = "local";
const DOCUMENT: &str = "document";
const FILE: &str = "templates.json";
const TAB: &str = "templates-section-";
/// The global templates' service and section.
const ALL: &str = "*";
/// The tab (and selection) of the global templates, stored under service and section `*`.
const GLOBAL: &str = "@global";
const HINT: &str = "\\@ = @";

/// The status line text: what happened, then the escaping hint.
fn said(text: &str) -> String {
    format!("{text} · {HINT}")
}

/// The tab and editor title of a section.
fn title(section: &str) -> String {
    match section {
        GLOBAL => "Global".to_owned(),
        _ => section.to_owned(),
    }
}

#[derive(Serialize, Deserialize)]
pub enum Msg {
    Section(String),
    Edit,
}

#[derive(Default)]
pub struct Templates {
    service: String,
    sections: Vec<String>,
    /// The selected section of each service.
    selected: BTreeMap<String, String>,
    /// Every section, as saved.
    library: Library,
}

impl Templates {
    /// Takes the service with its sections and the Local file (`[{source, text}]`) directly
    /// (tests).
    pub fn with(service: &str, sections: &[&str], files: Value) -> Self {
        let mut templates = Self {
            service: service.into(),
            sections: sections
                .iter()
                .map(|section| (*section).to_owned())
                .collect(),
            ..Self::default()
        };
        templates.read(&files);
        templates
    }

    fn read(&mut self, files: &Value) {
        self.library = files
            .as_array()
            .and_then(|files| files.last())
            .and_then(|file| serde_json::from_str(file["text"].as_str()?).ok())
            .unwrap_or_default();
    }

    /// Every section completion uses.
    pub fn library(&self) -> Library {
        self.library.clone()
    }

    /// The tabs: the service's sections, then the global one.
    fn tabs(&self) -> Vec<String> {
        let mut tabs = self.sections.clone();
        tabs.push(GLOBAL.to_owned());
        tabs
    }

    /// The selected tab.
    fn section(&self) -> String {
        let tabs = self.tabs();
        self.selected
            .get(&self.service)
            .filter(|section| tabs.contains(section))
            .or(tabs.first())
            .cloned()
            .unwrap_or_default()
    }

    /// The library service and section the selected tab edits.
    fn scope(&self) -> (String, String) {
        match self.section().as_str() {
            GLOBAL => (ALL.to_owned(), ALL.to_owned()),
            section => (self.service.clone(), section.to_owned()),
        }
    }

    fn saved(&self, service: &str, section: &str) -> String {
        self.library
            .get(service)
            .and_then(|sections| sections.get(section))
            .cloned()
            .unwrap_or_default()
    }

    fn shown(&self) -> String {
        let (service, section) = self.scope();
        self.saved(&service, &section)
    }

    fn contribution(&self) -> Cmd {
        cmd::contribute(&json!({ "templates": self.library() }))
    }

    fn save(&self) -> Cmd {
        let text = serde_json::to_string_pretty(&self.library).unwrap_or_default();
        cmd::sources_write("save", FILE, &text)
    }

    fn edit(&mut self, text: String) -> Vec<Cmd> {
        let (service, section) = self.scope();
        self.library
            .entry(service)
            .or_default()
            .insert(section, text);
        let mut commands = vec![self.save(), self.contribution()];
        commands.extend(self.problem());
        commands
    }

    /// The first error of the selected section, as the status.
    fn problem(&self) -> Option<Cmd> {
        let (_, errors) = templates::parse_partial(&self.shown());
        let error = errors.into_iter().next()?;
        Some(cmd::status(&said(&format!("ERROR: {error}")), "error"))
    }

    /// The status a section shows when nothing happened yet: its error, or the hint.
    fn idle(&self) -> Cmd {
        self.problem().unwrap_or_else(|| cmd::status(HINT, "idle"))
    }
}

impl Module for Templates {
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
                    self.sections = document["sections"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect();
                    Vec::new()
                }
                (SOURCES, Ok(files)) => {
                    self.read(&files);
                    vec![self.contribution()]
                }
                ("save", Ok(_)) => vec![
                    self.problem()
                        .unwrap_or_else(|| cmd::status(&said("Templates saved · OK"), "ok")),
                ],
                (_, Err(failure)) => {
                    vec![cmd::status(
                        &said(&format!("ERROR: {}", failure.message)),
                        "error",
                    )]
                }
                _ => Vec::new(),
            },
            Event::Ui {
                msg: Msg::Section(_),
                value: Some(tab),
            } => {
                let section = tab.trim_start_matches(TAB).to_owned();
                self.selected.insert(self.service.clone(), section);
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
        let section = self.section();
        let (service, scope) = self.scope();
        let body = vec![
            ui::editor(format!("{service}/{scope}"), self.shown(), &Msg::Edit)
                .key("templates-editor")
                .label(format!("Templates · {}", title(&section)))
                .debounce(500),
        ];
        let tabs = self
            .tabs()
            .iter()
            .map(|name| {
                let body = if *name == section {
                    body.clone()
                } else {
                    Vec::new()
                };
                ui::tab(format!("{TAB}{name}"), title(name), body)
            })
            .collect();
        let tabs = ui::tabs(
            tabs,
            format!("{TAB}{section}"),
            &Msg::Section(String::new()),
        )
        .key("templates-sections");
        Some(ui::tabbed_panel(tabs, Vec::new(), Vec::new()))
    }
}

services_sdk::export!(Templates);
