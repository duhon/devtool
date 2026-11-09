//! The Hints panel: completion hints of each service, shown for the service of the console.
//! A source's `hints/<service>.md` gives them; the user's own hints replace them for the
//! session until reset. Every service's hints go to the editors as a contribution.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use services_sdk::{Cmd, Event, Module, Subscriptions, cmd, ui};
use std::collections::BTreeMap;

/// The session kv key of the user's hints by service.
pub const OWN: &str = "hints";
/// The session kv key of the edits that are not valid yet, by service.
pub const DRAFTS: &str = "drafts";
const DOCUMENT: &str = "document";
const SOURCE: &str = "source:";

#[derive(Serialize, Deserialize)]
pub enum Msg {
    Edit,
    Reset,
}

#[derive(Default)]
pub struct Hints {
    service: String,
    /// Service ids by their tab titles.
    names: BTreeMap<String, String>,
    /// The highest-priority source's hints: `(source, text)`.
    sources: BTreeMap<String, (String, String)>,
    own: Map<String, Value>,
    drafts: Map<String, Value>,
    /// The level of the last status, shown as the title's light.
    light: String,
}

/// Checks hints the way completion reads them.
pub fn validate(text: &str) -> Result<(), String> {
    for (index, line) in text.lines().enumerate() {
        if line.trim().starts_with("##") {
            return Err(format!(
                "Line {}: ## service headers are managed automatically; use a single # for sections.",
                index + 1
            ));
        }
        if line.contains('\0') {
            return Err(format!(
                "Line {}: NUL characters are not supported.",
                index + 1
            ));
        }
        let mut rest = line;
        while let Some(start) = rest.find("{{") {
            let Some(end) = rest[start + 2..].find("}}") else {
                return Err(format!(
                    "Line {}: incomplete variable reference.",
                    index + 1
                ));
            };
            let reference = &rest[start + 2..start + 2 + end];
            if reference.contains(['{', '}']) || !reference.contains(':') {
                return Err(format!("Line {}: invalid variable reference.", index + 1));
            }
            rest = &rest[start + 2 + end + 2..];
        }
    }
    Ok(())
}

impl Hints {
    /// The hints of `service` that completion uses.
    pub fn effective(&self, service: &str) -> String {
        self.own
            .get(service)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| self.sources.get(service).map(|(_, text)| text.clone()))
            .unwrap_or_default()
    }

    fn shown(&self) -> String {
        self.drafts
            .get(&self.service)
            .and_then(Value::as_str)
            .map_or_else(|| self.effective(&self.service), str::to_owned)
    }

    fn contribution(&self) -> Cmd {
        let services: std::collections::BTreeSet<&String> =
            self.own.keys().chain(self.sources.keys()).collect();
        let hints: Map<String, Value> = services
            .into_iter()
            .map(|service| (service.clone(), json!(self.effective(service))))
            .collect();
        cmd::contribute(&json!({ "hints": hints }))
    }

    fn load() -> Vec<Cmd> {
        vec![
            cmd::document_read(DOCUMENT),
            cmd::kv_get(OWN, OWN),
            cmd::kv_get(DRAFTS, DRAFTS),
        ]
    }

    fn save(&mut self, text: String) -> Vec<Cmd> {
        let service = self.service.clone();
        if service.is_empty() {
            return Vec::new();
        }
        match validate(&text) {
            Ok(()) => {
                self.drafts.remove(&service);
                self.own.insert(service, json!(text));
                vec![
                    cmd::kv_put("save", OWN, &Value::Object(self.own.clone()), true),
                    self.put_drafts(),
                    self.contribution(),
                ]
            }
            Err(error) => {
                self.drafts.insert(service, json!(text));
                vec![
                    self.put_drafts(),
                    self.status(&format!("ERROR: Hints validation: {error}"), "error"),
                ]
            }
        }
    }

    fn put_drafts(&self) -> Cmd {
        if self.drafts.is_empty() {
            cmd::kv_delete("", DRAFTS)
        } else {
            cmd::kv_put("", DRAFTS, &Value::Object(self.drafts.clone()), true)
        }
    }
}

impl Module for Hints {
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
            Event::Init { .. } | Event::Workspace => Self::load(),
            Event::Shown => vec![cmd::document_read(DOCUMENT)],
            Event::Document(check) if check.service != self.service => {
                self.service = check.service;
                Vec::new()
            }
            Event::Reply { tag, result } => match (tag.as_str(), result) {
                (DOCUMENT, Ok(document)) => {
                    self.service = document["service"].as_str().unwrap_or_default().to_owned();
                    let mut commands = Vec::new();
                    for service in document["services"].as_array().into_iter().flatten() {
                        let id = service["id"].as_str().unwrap_or_default().to_owned();
                        let name = service["name"].as_str().unwrap_or(&id).to_owned();
                        if !self.names.contains_key(&id) {
                            commands.push(cmd::sources_read(
                                format!("{SOURCE}{id}"),
                                &format!("hints/{id}.md"),
                            ));
                        }
                        self.names.insert(id, name);
                    }
                    commands
                }
                (OWN, Ok(own)) => {
                    self.own = own.as_object().cloned().unwrap_or_default();
                    vec![self.contribution()]
                }
                (DRAFTS, Ok(drafts)) => {
                    self.drafts = drafts.as_object().cloned().unwrap_or_default();
                    Vec::new()
                }
                (tag, Ok(files)) if tag.starts_with(SOURCE) => {
                    let service = &tag[SOURCE.len()..];
                    match files.as_array().and_then(|files| files.last()) {
                        Some(file) => {
                            self.sources.insert(
                                service.to_owned(),
                                (
                                    file["source"].as_str().unwrap_or_default().to_owned(),
                                    file["text"].as_str().unwrap_or_default().to_owned(),
                                ),
                            );
                        }
                        None => {
                            self.sources.remove(service);
                        }
                    }
                    vec![self.contribution()]
                }
                ("save", Ok(_)) => vec![self.status("Hints save · OK", "ok")],
                ("reset", Ok(_)) => vec![self.status("Hints reset · OK", "ok")],
                (tag @ ("save" | "reset"), Err(failure)) => vec![self.status(
                    &format!("Hints {tag} · ERROR · {}", failure.message),
                    "error",
                )],
                (_, Err(failure)) => {
                    vec![self.status(&format!("ERROR: {}", failure.message), "error")]
                }
                _ => Vec::new(),
            },
            Event::Ui {
                msg: Msg::Edit,
                value: Some(text),
            } if text != self.shown() || self.drafts.contains_key(&self.service) => self.save(text),
            Event::Ui {
                msg: Msg::Reset, ..
            } => {
                self.own.remove(&self.service);
                self.drafts.remove(&self.service);
                vec![
                    cmd::kv_put("reset", OWN, &Value::Object(self.own.clone()), true),
                    self.put_drafts(),
                    self.contribution(),
                ]
            }
            _ => Vec::new(),
        }
    }

    fn view(&self) -> Option<ui::Node> {
        if self.service.is_empty() {
            return Some(ui::panel(
                "Hints",
                vec![ui::text("No service.").variant("muted")],
                Vec::new(),
            ));
        }
        let name = self.names.get(&self.service).unwrap_or(&self.service);
        let source = self.sources.get(&self.service).map(|(source, _)| source);
        let own = self.own.contains_key(&self.service);
        let mut origin = Vec::new();
        match (own, source) {
            (true, Some(source)) => {
                origin.push(
                    ui::badge(format!("Yours, over source {source}")).key("hints-origin-label"),
                );
                origin
                    .push(ui::button(format!("Reset to {source}"), &Msg::Reset).key("hints-reset"));
            }
            (true, None) => {
                origin.push(ui::badge("Yours").key("hints-origin-label"));
                origin.push(ui::button("Reset", &Msg::Reset).key("hints-reset"));
            }
            (false, Some(source)) => {
                origin.push(ui::badge(format!("From source {source}")).key("hints-origin-label"));
            }
            (false, None) => {}
        }
        let mut actions = Vec::new();
        if !origin.is_empty() {
            actions.push(ui::row(origin).key("hints-origin"));
        }
        let body = vec![
            ui::editor(format!("hints/{}", self.service), self.shown(), &Msg::Edit)
                .key("console-hints"),
        ];
        Some(
            ui::panel(format!("{name} completion hints"), body, Vec::new())
                .indicator(if self.light.is_empty() {
                    "idle"
                } else {
                    &self.light
                })
                .actions(actions),
        )
    }
}

impl Hints {
    fn status(&mut self, text: &str, level: &str) -> Cmd {
        self.light = level.to_owned();
        cmd::status(text, level)
    }
}

services_sdk::export!(Hints);
