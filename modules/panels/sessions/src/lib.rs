//! The Sessions panel: the saved workspaces (sessions) of the Kernel — switch to one, start
//! a new one from another, delete one. `0` is the temporary workspace, never saved.

use serde::{Deserialize, Serialize};
use services_sdk::{Cmd, Event, Module, Subscriptions, cmd, ui};

const LIST: &str = "list";
const SAVING: &str = "saving";
/// How often the list is read while the current workspace is being saved.
const SAVING_POLL_MS: u32 = 500;
pub const TEMPORARY: &str = "Temporary";

#[derive(Serialize, Deserialize)]
pub enum Msg {
    Switch(u64),
    Copy(u64),
    Delete(u64),
    /// The new name is the value.
    Rename(u64),
    Retry,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Session {
    pub id: u64,
    pub name: String,
    /// When its changes were last saved, in milliseconds since the epoch.
    #[serde(default)]
    pub used: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Library {
    pub current: u64,
    pub sessions: Vec<Session>,
    pub saving: bool,
    pub save_error: Option<String>,
    pub import_error: Option<String>,
}

#[derive(Default)]
pub struct Sessions {
    library: Library,
    /// The last operation: `(text, indicator)`.
    result: Option<(String, &'static str)>,
    polling: bool,
    /// The local time zone offset in minutes, as `Date.getTimezoneOffset` gives it.
    offset: f64,
}

impl Sessions {
    fn status(&self) -> Cmd {
        if let Some(error) = &self.library.save_error {
            return cmd::status(&format!("Autosave · ERROR · {error}"), "error");
        }
        if self.library.saving {
            return cmd::status("Saving…", "busy");
        }
        match &self.result {
            Some((text, indicator)) => cmd::status(text, indicator),
            None => cmd::status("Sessions", "idle"),
        }
    }

    fn poll(&mut self) -> Vec<Cmd> {
        if self.library.saving && !self.polling {
            self.polling = true;
            vec![cmd::timer(SAVING, SAVING_POLL_MS)]
        } else {
            Vec::new()
        }
    }

    fn rows(&self) -> impl Iterator<Item = Session> + '_ {
        std::iter::once(Session {
            id: 0,
            name: TEMPORARY.into(),
            used: 0,
        })
        .chain(self.library.sessions.iter().cloned())
    }

    fn name(&self, id: u64) -> String {
        self.rows()
            .find(|session| session.id == id)
            .map(|session| session.name)
            .unwrap_or_else(|| TEMPORARY.into())
    }

    /// The name of a session created from `parent`: `<parent>-<N>`, one more than the
    /// largest such number among the sessions.
    fn child_name(&self, parent: &str) -> String {
        let prefix = format!("{parent}-");
        let last = self
            .library
            .sessions
            .iter()
            .filter_map(|session| session.name.strip_prefix(&prefix)?.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        format!("{prefix}{}", last + 1)
    }
}

impl Module for Sessions {
    type Msg = Msg;

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            workspace: true,
            ..Default::default()
        }
    }

    fn update(&mut self, event: Event<Msg>) -> Vec<Cmd> {
        match event {
            Event::Init { settings } => {
                self.offset = settings["timezoneOffset"].as_f64().unwrap_or_default();
                Vec::new()
            }
            Event::Shown | Event::Workspace => {
                // A hidden panel's timer reply is dropped, so a poll may never have answered.
                self.polling = false;
                vec![cmd::workspace_list(LIST)]
            }
            Event::Reply { tag, result } if tag == SAVING => {
                self.polling = false;
                let _ = result;
                vec![cmd::workspace_list(LIST)]
            }
            Event::Reply { tag, result } if tag == LIST => match result {
                Ok(value) => match serde_json::from_value::<Library>(value) {
                    Ok(library) => {
                        self.library = library;
                        let mut commands = self.poll();
                        commands.push(self.status());
                        commands
                    }
                    Err(error) => vec![cmd::status(
                        &format!("Sessions · ERROR · Invalid session list: {error}"),
                        "error",
                    )],
                },
                Err(failure) => vec![cmd::status(
                    &format!("Sessions · ERROR · {}", failure.message),
                    "error",
                )],
            },
            Event::Reply { result, .. } => {
                self.result = Some(match result {
                    Ok(_) => ("Sessions · OK".into(), "ok"),
                    Err(failure) => (format!("Sessions · ERROR · {}", failure.message), "error"),
                });
                vec![cmd::workspace_list(LIST), self.status()]
            }
            Event::Ui { msg, value } => {
                let command = match msg {
                    Msg::Switch(id) if id == self.library.current => return Vec::new(),
                    Msg::Switch(id) => cmd::workspace_restore("switch", id),
                    Msg::Copy(id) => {
                        let name = self.child_name(&self.name(id));
                        cmd::workspace_copy("copy", id, &name)
                    }
                    Msg::Delete(id) => cmd::workspace_delete("delete", id),
                    Msg::Rename(id) => {
                        let name = value.unwrap_or_default().trim().to_owned();
                        if name.is_empty() || name == self.name(id) {
                            return Vec::new();
                        }
                        cmd::workspace_rename("rename", id, &name)
                    }
                    Msg::Retry => cmd::workspace_retry("retry"),
                };
                vec![command, cmd::status("Sessions · Running…", "busy")]
            }
            _ => Vec::new(),
        }
    }

    fn view(&self) -> Option<ui::Node> {
        let mut body = Vec::new();
        if let Some(error) = &self.library.import_error {
            body.push(ui::row(vec![
                ui::text(format!("Original workspace import is pending: {error}"))
                    .key("session-import-error"),
                ui::button("Retry import", &Msg::Retry).key("session-import-retry"),
            ]));
        }
        let current = self.library.current;
        let rows = self
            .rows()
            .map(|Session { id, name, used }| {
                let label = if id == current && id != 0 {
                    ui::input(Some(name.clone()), "", &Msg::Rename(id))
                        .variant("inline")
                        .label("Session name")
                        .title("Rename this session")
                } else {
                    ui::text(name.clone()).title(name.clone())
                };
                let date = if id == 0 {
                    "Immutable".to_owned()
                } else if used == 0 {
                    "—".to_owned()
                } else {
                    services_sdk::time::date(used as f64, self.offset)
                };
                let row = ui::TableRow::new(
                    format!("session-{id}"),
                    vec![
                        ui::text(id.to_string()),
                        label.key(format!("session-name-{id}")),
                        ui::text(date).key(format!("session-used-{id}")),
                        ui::row(vec![
                            ui::button("+", &Msg::Copy(id))
                                .key(format!("session-copy-{id}"))
                                .title(format!("Create a session from {name}")),
                            ui::button("×", &Msg::Delete(id))
                                .key(format!("session-delete-{id}"))
                                .title(if id == 0 {
                                    "The temporary session cannot be deleted".to_owned()
                                } else {
                                    format!("Delete {name}")
                                })
                                .confirm(format!("Delete session \"{name}\"?"))
                                .disabled(id == 0),
                        ])
                        .variant("end nowrap"),
                    ],
                )
                .clickable(&Msg::Switch(id));
                if id == current {
                    row.variant("active")
                } else {
                    row
                }
            })
            .collect();
        body.push(
            ui::table(
                ["#", "Session", "Last used", ""]
                    .map(str::to_owned)
                    .to_vec(),
                rows,
            )
            .widths(vec![0, 55, 45, 0])
            .variant("roomy")
            .key("session-list"),
        );
        Some(
            ui::panel("Sessions", body, Vec::new())
                .variant("padded")
                .indicator("idle")
                .key("console-sessions"),
        )
    }
}

services_sdk::export!(Sessions);
