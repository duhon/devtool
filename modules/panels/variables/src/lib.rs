//! The Variables panel: the session's own variables (`var`, kept as a session kv key) and the
//! environment of the user's login shell (`env`, read-only). It resolves `{{var:name}}`,
//! `{{env:NAME}}` and `{{:name}}` when `var` has the name; `{{:name}}` that no panel owns
//! falls back to the environment in the last pass.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use services_sdk::{Cmd, Event, Module, Resolution, cmd, refs, ui};

/// The session kv key of the variables.
pub const VAR: &str = "var";
/// The session kv key of a draft that does not parse yet.
pub const DRAFT: &str = "draft";
const ENV: &str = "env";

#[derive(Serialize, Deserialize)]
pub enum Msg {
    Tab(String),
    Edit,
}

#[derive(Default)]
pub struct Variables {
    vars: Map<String, Value>,
    env: Map<String, Value>,
    /// The editor text when it differs from `vars` (it does not parse).
    draft: Option<String>,
    error: Option<String>,
    env_error: Option<String>,
    tab: String,
    /// Bumped when the module replaces the editor text itself.
    shown: String,
}

impl Variables {
    /// Takes the variables and the environment directly (tests).
    pub fn with(vars: Value, env: Value) -> Self {
        let mut variables = Self {
            vars: vars.as_object().cloned().unwrap_or_default(),
            env: env.as_object().cloned().unwrap_or_default(),
            ..Self::default()
        };
        variables.shown = variables.text();
        variables
    }

    fn text(&self) -> String {
        self.draft
            .clone()
            .unwrap_or_else(|| refs::format_lines(&self.vars))
    }

    fn load() -> Vec<Cmd> {
        vec![
            cmd::kv_watch(VAR),
            cmd::kv_get(VAR, VAR),
            cmd::kv_get(DRAFT, DRAFT),
            cmd::env(ENV),
        ]
    }

    /// What the editors complete and show on hover.
    fn contribution(&self) -> Cmd {
        let mut hints: Vec<(String, String)> = Vec::new();
        for (name, value) in self.vars.iter().filter(|(name, _)| !name.starts_with('#')) {
            hints.push((format!("{{{{var:{name}}}}}"), refs::text(value)));
            hints.push((format!("{{{{:{name}}}}}"), refs::text(value)));
        }
        for (name, value) in &self.env {
            hints.push((format!("{{{{env:{name}}}}}"), refs::text(value)));
        }
        cmd::contribute(&json!({ "variables": hints }))
    }

    fn save(&mut self, text: String) -> Vec<Cmd> {
        self.shown = text.clone();
        match refs::parse_lines(&text) {
            Ok(vars) => {
                self.error = None;
                self.draft = None;
                self.vars = vars;
                vec![
                    cmd::kv_put("save", VAR, &Value::Object(self.vars.clone()), true),
                    cmd::kv_delete("", DRAFT),
                    cmd::status("Variables save · OK", "ok"),
                    self.contribution(),
                ]
            }
            Err(error) => {
                let message = format!("ERROR: {error}");
                self.error = Some(message.clone());
                self.draft = Some(text.clone());
                vec![
                    cmd::kv_put("", DRAFT, &json!(text), true),
                    cmd::status(&message, "error"),
                ]
            }
        }
    }

    /// The value of `name` in `var` with the references `var` and `env` own resolved.
    fn own(&self, name: &str, chain: &mut Vec<String>) -> Result<Option<String>, String> {
        let Some(value) = self.vars.get(name) else {
            return Ok(None);
        };
        if chain.iter().any(|link| link == name) {
            return Err(format!(
                "Circular variable reference: {} -> {name}",
                chain.join(" -> ")
            ));
        }
        chain.push(name.to_owned());
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
        match set {
            "var" => match self.own(name, chain)? {
                Some(value) => Ok(Some(value)),
                None => Err(format!("Unknown variable \"{name}\" in var")),
            },
            "" => self.own(name, chain),
            ENV => Ok(self.env.get(name).map(refs::text)),
            _ => Ok(None),
        }
    }
}

impl Module for Variables {
    type Msg = Msg;

    fn subscriptions(&self) -> services_sdk::Subscriptions {
        services_sdk::Subscriptions {
            workspace: true,
            ..Default::default()
        }
    }

    fn update(&mut self, event: Event<Msg>) -> Vec<Cmd> {
        match event {
            Event::Init { .. } | Event::Workspace => Self::load(),
            Event::Storage { key } if key == VAR => vec![cmd::kv_get(VAR, VAR)],
            Event::Reply { tag, result } => match (tag.as_str(), result) {
                (VAR, Ok(value)) => {
                    self.vars = value.as_object().cloned().unwrap_or_default();
                    // The editor keeps the text as typed unless the variables changed elsewhere.
                    if refs::parse_lines(&self.shown).ok().as_ref() != Some(&self.vars) {
                        self.shown = self.text();
                    }
                    vec![self.contribution()]
                }
                (DRAFT, Ok(Value::String(text))) => {
                    self.error = refs::parse_lines(&text)
                        .err()
                        .map(|error| format!("ERROR: {error}"));
                    self.draft = Some(text);
                    self.shown = self.text();
                    Vec::new()
                }
                (DRAFT, _) => {
                    self.draft = None;
                    self.shown = self.text();
                    Vec::new()
                }
                (ENV, Ok(value)) => {
                    self.env = value.as_object().cloned().unwrap_or_default();
                    self.env_error = None;
                    vec![self.contribution()]
                }
                (ENV, Err(failure)) => {
                    self.env_error = Some(failure.message);
                    Vec::new()
                }
                ("save", Err(failure)) => {
                    let message = format!("ERROR: {}", failure.message);
                    vec![cmd::status(&message, "error")]
                }
                (VAR, Err(failure)) => {
                    vec![cmd::status(&format!("ERROR: {}", failure.message), "error")]
                }
                _ => Vec::new(),
            },
            Event::Ui {
                msg: Msg::Tab(_),
                value: Some(tab),
            } => {
                self.tab = tab.trim_start_matches("variables-tab-").to_owned();
                Vec::new()
            }
            Event::Ui {
                msg: Msg::Edit,
                value: Some(text),
            } if text != self.text() || self.draft.is_some() => self.save(text),
            _ => Vec::new(),
        }
    }

    fn view(&self) -> Option<ui::Node> {
        let active = if self.tab == ENV { ENV } else { VAR };
        let var = ui::editor("headers", self.shown.clone(), &Msg::Edit).key("console-variables");
        let mut env =
            vec![ui::viewer("headers", refs::format_lines(&self.env)).key("console-variables")];
        if let Some(error) = &self.env_error {
            env.insert(0, ui::text(error.clone()).variant("danger"));
        }
        let tabs = ui::tabs(
            vec![
                ui::tab("variables-tab-var", "var", vec![var]),
                ui::tab("variables-tab-env", "env (read-only)", env),
            ],
            format!("variables-tab-{active}"),
            &Msg::Tab(String::new()),
        )
        .key("variables-tabs");
        Some(ui::tabbed_panel(tabs, Vec::new(), Vec::new()))
    }

    fn resolve(&self, text: &str, json: bool, last: bool) -> Option<Result<Resolution, String>> {
        let mut secrets = Vec::new();
        let mut missing = Vec::new();
        let result = refs::replace(text, |set, name| {
            let value = match self.reference(set, name, &mut Vec::new())? {
                Some(value) => Some(value),
                None if last && set.is_empty() => self.env.get(name).map(refs::text),
                None => None,
            };
            if value.is_none() && last && (set.is_empty() || set == ENV) {
                missing.push(name.to_owned());
            }
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
        Some(result.and_then(|text| {
            if missing.is_empty() {
                Ok(Resolution { text, secrets })
            } else {
                Err(format!(
                    "Missing environment variable(s): {}",
                    missing.join(", ")
                ))
            }
        }))
    }
}

services_sdk::export!(Variables);
