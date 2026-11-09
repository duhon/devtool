//! Agent turns a module starts (`agent.turn`): Copilot through the Host, its console tools
//! served by the Shell, its page tools handed back to the module, and the end of a turn
//! stopping what it started.

use super::{Core, Event, Input};
use crate::Effect;
use crate::host::Reply;
use crate::modules::registry;
use serde_json::{Value, json};
use services_sdk::{Cmd, Failure};
use std::collections::BTreeSet;

/// A Copilot turn a module started.
pub(super) struct Turn {
    module: String,
    stream: u32,
    pub(super) stopping: bool,
    /// The page tool calls handed to the module.
    calls: BTreeSet<String>,
}

impl Core {
    /// The Agent commands; `None` when `cmd` is not one.
    pub(super) fn agent_command(&mut self, module: &str, cmd: &Cmd) -> Option<Vec<Effect>> {
        let args = cmd.args();
        let reply = |result: Result<Value, Failure>| {
            registry::with(|modules| modules.reply(module, &cmd.tag, result))
        };
        Some(match cmd.name.as_str() {
            "agent.turn" => {
                if self.hidden {
                    return Some(reply(Err(Failure::new(
                        "Console is not active. Return to Console before asking the Agent.",
                    ))));
                }
                let mut params = args;
                params["workspaceId"] = json!(self.platform.agent_workspace_id());
                params["service"] = json!(self.service);
                let (m, t) = (module.to_owned(), cmd.tag.clone());
                let answer = self.callback(move |reply| Input::Turn(m.clone(), t.clone(), reply));
                let stream =
                    self.platform
                        .request(json!({"service":"agent","params":params}), true, answer);
                self.calls
                    .insert((module.to_owned(), cmd.tag.clone()), stream);
                self.turns.insert(
                    cmd.tag.clone(),
                    Turn {
                        module: module.to_owned(),
                        stream,
                        stopping: false,
                        calls: BTreeSet::new(),
                    },
                );
                Vec::new()
            }
            "agent.stop" => {
                self.stop_turn(&cmd.tag);
                Vec::new()
            }
            "agent.reply" => {
                if let Some(turn) = self
                    .turns
                    .values_mut()
                    .find(|turn| turn.module == module && turn.calls.contains(&cmd.tag))
                    && turn.calls.remove(&cmd.tag)
                    && !turn.stopping
                    && let Err(error) =
                        self.platform
                            .agent_reply(turn.stream, &cmd.tag, args["result"].clone())
                {
                    self.notice(format!("Agent · ERROR · {error}"));
                }
                Vec::new()
            }
            "agent.approve" => {
                self.emit(Event::Command {
                    module: module.to_owned(),
                    cmd: cmd.clone(),
                });
                Vec::new()
            }
            "agent.sessions" => {
                let (m, t) = (module.to_owned(), cmd.tag.clone());
                let answer = self.callback(move |reply| {
                    Input::Hosted(m.clone(), t.clone(), crate::requests::After::Reply, reply)
                });
                let id = self.platform.request(
                    json!({"service":"agent_sessions","op":args["op"],"sessionIds":args["sessionIds"]}),
                    false,
                    answer,
                );
                if !cmd.tag.is_empty() {
                    self.calls.insert((module.to_owned(), cmd.tag.clone()), id);
                }
                Vec::new()
            }
            _ => return None,
        })
    }

    /// Stops the turn `tag`: its console runs stop, its approvals and edits go.
    fn stop_turn(&mut self, tag: &str) {
        let Some(turn) = self.turns.get_mut(tag) else {
            return;
        };
        turn.stopping = true;
        let stream = turn.stream;
        self.stop_agent_runs(tag);
        self.emit(Event::AgentEnded(tag.to_owned()));
        if let Err(error) = self.platform.stop(stream) {
            self.notice(format!("Agent · Stop failed: {error}"));
        }
    }

    /// Stops every turn (the console was hidden).
    pub(super) fn stop_turns(&mut self) {
        let tags: Vec<String> = self.turns.keys().cloned().collect();
        for tag in tags {
            self.stop_turn(&tag);
        }
    }

    /// The console run waiting for approval in the turn `tag` changed: the module shows it.
    pub(super) fn agent_approval(&mut self, tag: &str, approval: Option<Value>) {
        let Some(turn) = self.turns.get(tag) else {
            return;
        };
        let module = turn.module.clone();
        let effects = registry::with(|modules| {
            modules.reply(
                &module,
                tag,
                Ok(json!({"type":"approval","approval":approval})),
            )
        });
        self.apply(effects);
    }

    /// An event of the turn `tag`: Copilot's, a tool call, or its end.
    pub(super) fn turn_replied(&mut self, module: &str, tag: &str, reply: Reply) {
        let Some(turn) = self.turns.get(tag) else {
            return;
        };
        let stream = turn.stream;
        let stopping = turn.stopping;
        match reply {
            Reply::Data(value) if value["type"] == "workspace" => {
                let call = value["callId"].as_str().unwrap_or_default().to_owned();
                let tool = value["tool"].as_str().unwrap_or_default().to_owned();
                if value["workspaceId"].as_str()
                    != Some(self.platform.agent_workspace_id().as_str())
                    || stopping
                {
                    if let Err(error) = self.platform.agent_reply(
                        stream,
                        &call,
                        json!({"error":"Workspace access revoked"}),
                    ) {
                        self.notice(format!("Agent · ERROR · {error}"));
                    }
                } else if tool.starts_with("page_") {
                    if let Some(turn) = self.turns.get_mut(tag) {
                        turn.calls.insert(call.clone());
                    }
                    let args = json!({"turn": tag, "arguments": value["arguments"]});
                    let effects =
                        registry::with(|modules| modules.tool_call(module, &call, &tool, &args));
                    self.apply(effects);
                } else {
                    self.emit(Event::AgentTool {
                        turn: tag.to_owned(),
                        call,
                        tool,
                        arguments: value["arguments"].clone(),
                    });
                }
            }
            Reply::Data(value) => {
                let effects = registry::with(|modules| modules.reply(module, tag, Ok(value)));
                self.apply(effects);
            }
            Reply::Result(value) | Reply::Exit(value) | Reply::Error(value) => {
                self.turns.remove(tag);
                self.calls.remove(&(module.to_owned(), tag.to_owned()));
                self.stop_agent_runs(tag);
                self.emit(Event::AgentEnded(tag.to_owned()));
                let mut effects = registry::with(|modules| {
                    modules.reply(module, tag, Ok(json!({"type":"result","result":value})))
                });
                effects.extend(registry::with(|modules| modules.end(module, tag)));
                self.apply(effects);
            }
        }
    }

    /// A console tool of the turn `tag` answered.
    pub(super) fn agent_result(&mut self, tag: &str, call: &str, result: Value) {
        let Some(turn) = self.turns.get(tag).filter(|turn| !turn.stopping) else {
            return;
        };
        if let Err(error) = self.platform.agent_reply(turn.stream, call, result) {
            self.notice(format!("Agent · ERROR · {error}"));
        }
    }
}
