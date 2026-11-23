//! The Variables panel: the session's own variables (`var`, kept as a session kv key) and the
//! environment of the user's login shell (`env`, read-only). It only holds the data: it
//! contributes `var` first and `env` last to the variable tree the Kernel resolves
//! (`{{.var.x}}`, `{{:x}}`, `{{.env.X}}`). `var` values may be strings or nested hashes.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use services_sdk::{Cmd, Event, Module, cmd, refs, ui};

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
    /// The last operation of each tab: its status line text and light.
    var_status: Option<(String, &'static str)>,
    env_status: Option<(String, &'static str)>,
    /// The saved variables could not be read.
    unloaded: bool,
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

    fn active(&self) -> &'static str {
        if self.tab == ENV { ENV } else { VAR }
    }

    /// Records the status of `tab` and shows it when that tab is the one on screen.
    fn note(&mut self, tab: &'static str, text: &str, light: &'static str) -> Vec<Cmd> {
        let status = Some((text.to_owned(), light));
        if tab == ENV {
            self.env_status = status;
        } else {
            self.var_status = status;
        }
        if tab == self.active() {
            vec![cmd::status(text, light)]
        } else {
            Vec::new()
        }
    }

    fn load() -> Vec<Cmd> {
        vec![
            cmd::kv_watch(VAR),
            cmd::kv_get(VAR, VAR),
            cmd::kv_get(DRAFT, DRAFT),
            cmd::env(ENV),
        ]
    }

    /// The panel's part of the variable tree: `var` first, `env` last.
    fn contribution(&self) -> Cmd {
        cmd::contribute(&json!({"tree": {
            "first": [[VAR, self.vars]],
            "last": [[ENV, self.env]],
        }}))
    }

    fn save(&mut self, text: String) -> Vec<Cmd> {
        self.shown = text.clone();
        match refs::parse_lines(&text) {
            Ok(vars) => {
                self.error = None;
                self.draft = None;
                self.vars = vars;
                let mut commands = vec![
                    cmd::kv_put("save", VAR, &Value::Object(self.vars.clone()), true),
                    cmd::kv_delete("", DRAFT),
                ];
                commands.extend(self.note(VAR, "Variables save · OK", "ok"));
                commands.push(self.contribution());
                commands
            }
            Err(error) => {
                let message = format!("Variables validation · ERROR · {error}");
                self.error = Some(message.clone());
                self.draft = Some(text.clone());
                let mut commands = vec![cmd::kv_put("", DRAFT, &json!(text), true)];
                commands.extend(self.note(VAR, &message, "error"));
                commands
            }
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
                    self.unloaded = false;
                    // The editor keeps the text as typed unless the variables changed elsewhere.
                    if refs::parse_lines(&self.shown).ok().as_ref() != Some(&self.vars) {
                        self.shown = self.text();
                    }
                    let mut commands = Vec::new();
                    if self.var_status.is_none() {
                        commands.extend(self.note(VAR, "Variables load · OK", "ok"));
                    }
                    commands.push(self.contribution());
                    commands
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
                    let mut commands = self.note(ENV, "Variables load · OK", "ok");
                    commands.push(self.contribution());
                    commands
                }
                (ENV, Err(failure)) => self.note(
                    ENV,
                    &format!("Variables load · ERROR · {}", failure.message),
                    "error",
                ),
                ("save", Err(failure)) => self.note(
                    VAR,
                    &format!("Variables save · ERROR · {}", failure.message),
                    "error",
                ),
                (VAR, Err(failure)) => {
                    self.unloaded = true;
                    self.note(
                        VAR,
                        &format!("Variables load · ERROR · {}", failure.message),
                        "error",
                    )
                }
                _ => Vec::new(),
            },
            Event::Ui {
                msg: Msg::Tab(_),
                value: Some(tab),
            } => {
                self.tab = tab.trim_start_matches("variables-tab-").to_owned();
                let shown = if self.tab == ENV {
                    &self.env_status
                } else {
                    &self.var_status
                };
                let (text, light) = shown
                    .as_ref()
                    .map_or(("", ""), |(text, light)| (text.as_str(), *light));
                vec![cmd::status(text, light)]
            }
            Event::Ui {
                msg: Msg::Edit,
                value: Some(text),
            } if text != self.text() || self.draft.is_some() => self.save(text),
            _ => Vec::new(),
        }
    }

    fn view(&self) -> Option<ui::Node> {
        let active = self.active();
        let var = if self.unloaded {
            ui::text("Variables are not loaded.")
        } else {
            ui::editor("headers", self.shown.clone(), &Msg::Edit)
                .key("console-variables")
                .placeholder("# comments start with #\nlocale: en_US\nhosts:\n  catalog: catalog.example.com\n  checkout: checkout.example.com")
                .label("Variables · var")
                .debounce(600)
        };
        let env = ui::viewer("headers", refs::format_lines(&self.env))
            .key("console-variables")
            .label("Variables · env")
            .variant("flush");
        let light = |status: &Option<(String, &'static str)>| {
            status.as_ref().map_or("", |(_, light)| *light).to_owned()
        };
        let tabs = ui::tabs(
            vec![
                ui::tab("variables-tab-var", "var", vec![var]).indicator(light(&self.var_status)),
                ui::tab("variables-tab-env", "env (read-only)", vec![env])
                    .indicator(light(&self.env_status)),
            ],
            format!("variables-tab-{active}"),
            &Msg::Tab(String::new()),
        )
        .key("variables-tabs");
        Some(ui::tabbed_panel(tabs, Vec::new(), Vec::new()))
    }
}

services_sdk::export!(Variables);
