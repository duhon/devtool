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
    Name,
    Switch(u64),
    Copy(u64),
    Delete(u64),
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Session {
    pub id: u64,
    pub name: String,
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
    name: String,
    /// The last operation: `(text, indicator)`.
    result: Option<(String, &'static str)>,
    polling: bool,
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

    fn rows(&self) -> impl Iterator<Item = (u64, &str)> {
        std::iter::once((0, TEMPORARY)).chain(
            self.library
                .sessions
                .iter()
                .map(|session| (session.id, session.name.as_str())),
        )
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
            Event::Shown | Event::Workspace => vec![cmd::workspace_list(LIST)],
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
            Event::Ui {
                msg: Msg::Name,
                value,
            } => {
                self.name = value.unwrap_or_default();
                Vec::new()
            }
            Event::Ui { msg, .. } => {
                let command = match msg {
                    Msg::Switch(id) => cmd::workspace_restore("switch", id),
                    Msg::Copy(id) => cmd::workspace_copy("copy", id, &self.name),
                    Msg::Delete(id) => cmd::workspace_delete("delete", id),
                    Msg::Name => return Vec::new(),
                };
                vec![command, cmd::status("Sessions · Running…", "busy")]
            }
            _ => Vec::new(),
        }
    }

    fn view(&self) -> Option<ui::Node> {
        let mut body = vec![
            ui::text(if self.library.saving {
                "Saving…"
            } else if self.library.save_error.is_some() {
                "Not saved"
            } else {
                "Saved"
            })
            .key("session-storage-status"),
        ];
        if let Some(error) = &self.library.import_error {
            body.push(
                ui::text(format!("Original workspace import is pending: {error}"))
                    .variant("danger")
                    .key("session-import-error"),
            );
        }
        body.push(ui::row(vec![
            ui::text("New session name"),
            ui::input(None, "", &Msg::Name)
                .key("session-new-name")
                .on_input(&Msg::Name),
        ]));
        let current = self.library.current;
        let rows = self
            .rows()
            .map(|(id, name)| {
                let row = ui::TableRow::new(
                    format!("session-{id}"),
                    vec![
                        ui::text(id.to_string()),
                        ui::text(name),
                        ui::row(vec![
                            ui::button("Switch", &Msg::Switch(id))
                                .key(format!("session-switch-{id}"))
                                .disabled(id == current),
                            ui::button("Delete", &Msg::Delete(id))
                                .key(format!("session-delete-{id}"))
                                .disabled(id == 0),
                            ui::button("Create from", &Msg::Copy(id))
                                .key(format!("session-copy-{id}")),
                        ]),
                    ],
                );
                if id == current {
                    row.variant("active")
                } else {
                    row
                }
            })
            .collect();
        body.push(
            ui::table(
                ["#", "Session", "Actions"].map(str::to_owned).to_vec(),
                rows,
            )
            .key("session-list"),
        );
        body.push(ui::text(
            "0 is temporary. Creating from 0 starts empty. Named sessions save \
             automatically. Dictionary and environment variables are shared. Agent chats \
             belong to their session; deleting a session closes them.",
        ));
        Some(
            ui::panel("Sessions", body, Vec::new())
                .indicator("idle")
                .key("console-sessions"),
        )
    }
}

services_sdk::export!(Sessions);
