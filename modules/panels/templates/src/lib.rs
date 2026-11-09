//! The Templates panel: per service and section, `@name description` templates that
//! completion inserts. Sources bring them in `templates.json` (`{service: {section: text}}`);
//! the user's sections replace a source's and are kept in the module's kv, shared by all
//! sessions, until reset. An edit that does not parse stays a draft.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use services_contracts::templates::{self, Library};
use services_sdk::{Cmd, Event, Module, Subscriptions, cmd, ui};
use std::collections::BTreeMap;

/// The kv key of the user's sections.
pub const LIBRARY: &str = "library";
/// The kv key of the edits that do not parse, by `service/section`.
pub const DRAFTS: &str = "drafts";
const SOURCES: &str = "sources";
const DOCUMENT: &str = "document";
const FILE: &str = "templates.json";
const TAB: &str = "templates-section-";
const HINT: &str = "Templates · @name description, then the body; @@ = @ at a line start";

#[derive(Serialize, Deserialize)]
pub enum Msg {
    Section(String),
    Edit,
    Discard,
    Reset,
}

#[derive(Default)]
pub struct Templates {
    service: String,
    sections: Vec<String>,
    /// The selected section of each service.
    selected: BTreeMap<String, String>,
    /// The sources' sections and the source each comes from.
    base: Library,
    origins: Library,
    own: Library,
    drafts: Map<String, Value>,
}

impl Templates {
    /// Takes the service with its sections, the sources (`[{source, text}]`) and the user's
    /// sections directly (tests).
    pub fn with(service: &str, sections: &[&str], sources: Value, own: Value) -> Self {
        let mut templates = Self {
            service: service.into(),
            sections: sections
                .iter()
                .map(|section| (*section).to_owned())
                .collect(),
            ..Self::default()
        };
        templates.read_sources(&sources);
        templates.own = serde_json::from_value(own).unwrap_or_default();
        templates
    }

    fn read_sources(&mut self, files: &Value) {
        self.base.clear();
        self.origins.clear();
        for file in files.as_array().into_iter().flatten() {
            let source = file["source"].as_str().unwrap_or_default();
            let Ok(library) =
                serde_json::from_str::<Library>(file["text"].as_str().unwrap_or("{}"))
            else {
                continue;
            };
            for (service, sections) in library {
                for (section, text) in sections {
                    self.origins
                        .entry(service.clone())
                        .or_default()
                        .insert(section.clone(), source.to_owned());
                    self.base
                        .entry(service.clone())
                        .or_default()
                        .insert(section, text);
                }
            }
        }
    }

    /// Every section completion uses.
    pub fn library(&self) -> Library {
        let mut library = self.base.clone();
        for (service, sections) in &self.own {
            for (section, text) in sections {
                library
                    .entry(service.clone())
                    .or_default()
                    .insert(section.clone(), text.clone());
            }
        }
        library
    }

    fn section(&self) -> String {
        self.selected
            .get(&self.service)
            .filter(|section| self.sections.contains(section))
            .or(self.sections.first())
            .cloned()
            .unwrap_or_default()
    }

    fn key(&self) -> String {
        format!("{}/{}", self.service, self.section())
    }

    fn saved(&self, service: &str, section: &str) -> String {
        self.own
            .get(service)
            .and_then(|sections| sections.get(section))
            .or_else(|| self.base.get(service)?.get(section))
            .cloned()
            .unwrap_or_default()
    }

    fn shown(&self) -> String {
        self.drafts
            .get(&self.key())
            .and_then(Value::as_str)
            .map_or_else(|| self.saved(&self.service, &self.section()), str::to_owned)
    }

    fn contribution(&self) -> Cmd {
        cmd::contribute(&json!({ "templates": self.library() }))
    }

    fn put_drafts(&self) -> Cmd {
        if self.drafts.is_empty() {
            cmd::kv_delete("", DRAFTS)
        } else {
            cmd::kv_put("", DRAFTS, &Value::Object(self.drafts.clone()), false)
        }
    }

    fn put_own(&self, operation: &str) -> Cmd {
        cmd::kv_put(operation, LIBRARY, &json!(self.own), false)
    }

    fn edit(&mut self, text: String) -> Vec<Cmd> {
        let (service, section) = (self.service.clone(), self.section());
        if section.is_empty() {
            return Vec::new();
        }
        match templates::parse(&text) {
            Ok(_) => {
                self.drafts.remove(&self.key());
                self.own.entry(service).or_default().insert(section, text);
                vec![self.put_own("save"), self.put_drafts(), self.contribution()]
            }
            Err(error) => {
                self.drafts.insert(self.key(), json!(text));
                vec![
                    self.put_drafts(),
                    cmd::status(&format!("ERROR: {error}"), "error"),
                ]
            }
        }
    }

    /// The status a section shows when nothing happened yet: its draft's error, or the hint.
    fn idle(&self) -> Cmd {
        match self
            .drafts
            .get(&self.key())
            .and_then(Value::as_str)
            .map(templates::parse)
        {
            Some(Err(error)) => cmd::status(&format!("ERROR: {error}"), "error"),
            _ => cmd::status(HINT, "idle"),
        }
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
                cmd::kv_get(LIBRARY, LIBRARY),
                cmd::kv_get(DRAFTS, DRAFTS),
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
                    self.read_sources(&files);
                    vec![self.contribution()]
                }
                (LIBRARY, Ok(own)) => {
                    self.own = serde_json::from_value(own).unwrap_or_default();
                    vec![self.contribution()]
                }
                (DRAFTS, Ok(drafts)) => {
                    self.drafts = drafts.as_object().cloned().unwrap_or_default();
                    Vec::new()
                }
                ("save", Ok(_)) => vec![cmd::status("Templates saved · OK", "ok")],
                ("reset", Ok(_)) => vec![cmd::status("Templates reset · OK", "ok")],
                (_, Err(failure)) => {
                    vec![cmd::status(&format!("ERROR: {}", failure.message), "error")]
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
            } if text != self.shown() || self.drafts.contains_key(&self.key()) => self.edit(text),
            Event::Ui {
                msg: Msg::Discard, ..
            } => {
                self.drafts.remove(&self.key());
                vec![self.put_drafts(), cmd::status(HINT, "idle")]
            }
            Event::Ui {
                msg: Msg::Reset, ..
            } => {
                let section = self.section();
                self.drafts.remove(&self.key());
                if let Some(sections) = self.own.get_mut(&self.service) {
                    sections.remove(&section);
                    if sections.is_empty() {
                        self.own.remove(&self.service);
                    }
                }
                vec![
                    self.put_own("reset"),
                    self.put_drafts(),
                    self.contribution(),
                ]
            }
            _ => Vec::new(),
        }
    }

    fn view(&self) -> Option<ui::Node> {
        if self.sections.is_empty() {
            let text = ui::text("This service has no template sections.").variant("muted");
            return Some(ui::panel("Templates", vec![text], Vec::new()));
        }
        let section = self.section();
        let source = self
            .origins
            .get(&self.service)
            .and_then(|sections| sections.get(&section));
        let own = self
            .own
            .get(&self.service)
            .is_some_and(|sections| sections.contains_key(&section));
        let mut header = Vec::new();
        match (own, source) {
            (true, Some(source)) => {
                header.push(
                    ui::text(format!("Yours, over source {source}")).key("templates-origin-label"),
                );
                header.push(
                    ui::button(format!("Reset to {source}"), &Msg::Reset).key("templates-reset"),
                );
            }
            (false, Some(source)) => {
                header
                    .push(ui::text(format!("From source {source}")).key("templates-origin-label"));
            }
            _ => {}
        }
        if self.drafts.contains_key(&self.key()) {
            header.push(ui::button("Discard draft", &Msg::Discard).key("templates-discard"));
        }
        let mut actions = Vec::new();
        if !header.is_empty() {
            actions.push(ui::row(header).variant("muted").key("templates-origin"));
        }
        let body = vec![
            ui::editor(
                format!("{}/{section}", self.service),
                self.shown(),
                &Msg::Edit,
            )
            .key("templates-editor"),
        ];
        let tabs = self
            .sections
            .iter()
            .map(|name| {
                let body = if *name == section {
                    body.clone()
                } else {
                    Vec::new()
                };
                ui::tab(format!("{TAB}{name}"), name.clone(), body)
            })
            .collect();
        let tabs = ui::tabs(
            tabs,
            format!("{TAB}{section}"),
            &Msg::Section(String::new()),
        )
        .key("templates-sections");
        Some(ui::tabbed_panel(tabs, Vec::new(), Vec::new()).actions(actions))
    }
}

services_sdk::export!(Templates);
