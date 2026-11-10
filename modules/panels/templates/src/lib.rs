//! The Templates panel: per service and section, `@name description` templates that
//! completion inserts. They are the Local source's `templates.json`
//! (`{service: {section: text}}`), saved as they are edited; sources sync with it in
//! Settings. An edit that does not parse stays a draft.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use services_contracts::templates::{self, Library};
use services_sdk::{Cmd, Event, Module, Subscriptions, cmd, ui};
use std::collections::BTreeMap;

/// The kv key of the edits that do not parse, by `service/section`.
pub const DRAFTS: &str = "drafts";
const SOURCES: &str = "local";
const DOCUMENT: &str = "document";
const FILE: &str = "templates.json";
const TAB: &str = "templates-section-";
const HINT: &str = "@@ = @";

/// The status line text: what happened, then the escaping hint.
fn said(text: &str) -> String {
    format!("{text} · {HINT}")
}

#[derive(Serialize, Deserialize)]
pub enum Msg {
    Section(String),
    Edit,
    Discard,
}

#[derive(Default)]
pub struct Templates {
    service: String,
    sections: Vec<String>,
    /// The selected section of each service.
    selected: BTreeMap<String, String>,
    /// Every section, as saved.
    library: Library,
    drafts: Map<String, Value>,
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
        self.library
            .get(service)
            .and_then(|sections| sections.get(section))
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

    fn save(&self) -> Cmd {
        let text = serde_json::to_string_pretty(&self.library).unwrap_or_default();
        cmd::sources_write("save", FILE, &text)
    }

    fn edit(&mut self, text: String) -> Vec<Cmd> {
        let (service, section) = (self.service.clone(), self.section());
        if section.is_empty() {
            return Vec::new();
        }
        match templates::parse(&text) {
            Ok(_) => {
                self.drafts.remove(&self.key());
                self.library
                    .entry(service)
                    .or_default()
                    .insert(section, text);
                vec![self.save(), self.put_drafts(), self.contribution()]
            }
            Err(error) => {
                self.drafts.insert(self.key(), json!(text));
                vec![
                    self.put_drafts(),
                    cmd::status(&said(&format!("ERROR: {error}")), "error"),
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
            Some(Err(error)) => cmd::status(&said(&format!("ERROR: {error}")), "error"),
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
                    self.read(&files);
                    vec![self.contribution()]
                }
                (DRAFTS, Ok(drafts)) => {
                    self.drafts = drafts.as_object().cloned().unwrap_or_default();
                    Vec::new()
                }
                ("save", Ok(_)) => vec![cmd::status(&said("Templates saved · OK"), "ok")],
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
            } if text != self.shown() || self.drafts.contains_key(&self.key()) => self.edit(text),
            Event::Ui {
                msg: Msg::Discard, ..
            } => {
                self.drafts.remove(&self.key());
                vec![self.put_drafts(), cmd::status(HINT, "idle")]
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
        let mut header = Vec::new();
        if self.drafts.contains_key(&self.key()) {
            header.push(
                ui::button("Discard draft", &Msg::Discard)
                    .key("templates-discard")
                    .title("Discard draft and reload saved templates"),
            );
        }
        let actions = if header.is_empty() {
            Vec::new()
        } else {
            vec![ui::row(header).key("templates-origin")]
        };
        let body = vec![
            ui::editor(
                format!("{}/{section}", self.service),
                self.shown(),
                &Msg::Edit,
            )
            .key("templates-editor")
            .placeholder(if section == "check" {
                "@example Description\n#target\n...\n"
            } else {
                "@example Description\nText to insert\n"
            })
            .label(format!("Templates · {section}"))
            .debounce(500),
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
