//! What the Agent panel module asks of the App: Copilot turns through the Host (their console
//! tools served here, page tools handed back to the module), the inspected page (DevTools)
//! and the Copilot history of its chats.

use super::*;
use services_kernel::Effect;
use services_sdk::Failure;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;

/// A Copilot turn a module started (`agent.turn`).
pub(super) struct AgentTurn {
    module: String,
    stream: u32,
    stopping: bool,
    /// The page tool calls handed to the module.
    calls: BTreeSet<String>,
}

impl AgentTurn {
    pub(super) fn stopping(&self) -> bool {
        self.stopping
    }
}

/// An Agent console tool call: `session` is the tag of the turn it came in.
#[derive(Clone)]
pub(super) struct WorkspaceRequest {
    pub session: String,
    pub tool: String,
    pub arguments: Value,
    pub reply: Callback<Value>,
}

/// A console run the Agent waits to have approved.
#[derive(Clone, PartialEq)]
pub(super) struct Approval {
    pub service: String,
    pub title: String,
    pub request: String,
    pub endpoint: String,
}

const UNAVAILABLE: &str = "Page tools are available only in the DevTools extension attached to a browser tab, not in standalone Console.";

impl App {
    /// The Agent and page commands of `kernel_command`; `None` when `cmd` is not one.
    pub(super) fn agent_command(
        &mut self,
        ctx: &Context<Self>,
        module: &str,
        cmd: &services_sdk::Cmd,
    ) -> Option<Vec<Effect>> {
        let args = cmd.args();
        let reply = |result: Result<Value, Failure>| {
            crate::kernel::with(|modules| modules.reply(module, &cmd.tag, result))
        };
        Some(match cmd.name.as_str() {
            "agent.turn" => {
                if self.hidden {
                    return Some(reply(Err(Failure::new(
                        "Console is not active. Return to Console before asking the Agent.",
                    ))));
                }
                let mut params = args;
                params["workspaceId"] = json!(self.agent_workspace_id());
                params["service"] = json!(self.console_service);
                let (m, t) = (module.to_owned(), cmd.tag.clone());
                let stream = self.transport.request(
                    json!({"service":"agent","params":params}),
                    true,
                    ctx.link()
                        .callback(move |reply| Msg::AgentReply(m.clone(), t.clone(), reply)),
                );
                self.kernel
                    .calls
                    .insert((module.to_owned(), cmd.tag.clone()), stream);
                self.agent_turns.insert(
                    cmd.tag.clone(),
                    AgentTurn {
                        module: module.to_owned(),
                        stream,
                        stopping: false,
                        calls: BTreeSet::new(),
                    },
                );
                Vec::new()
            }
            "agent.stop" => {
                self.agent_stop(ctx, &cmd.tag);
                Vec::new()
            }
            "agent.reply" => {
                if let Some(turn) = self
                    .agent_turns
                    .values_mut()
                    .find(|turn| turn.module == module && turn.calls.contains(&cmd.tag))
                    && turn.calls.remove(&cmd.tag)
                    && !turn.stopping
                    && let Err(error) =
                        self.transport
                            .agent_reply(turn.stream, &cmd.tag, args["result"].clone())
                {
                    self.notice = Some(Notice::new(format!("Agent · ERROR · {error}")));
                }
                Vec::new()
            }
            "agent.approve" => {
                if self
                    .agent_run
                    .as_ref()
                    .is_some_and(|run| run.request.session == cmd.tag)
                {
                    self.approve_agent_run(ctx, args["approve"] == true);
                    self.agent_approval(ctx, &cmd.tag);
                }
                Vec::new()
            }
            "agent.sessions" => {
                let (m, t) = (module.to_owned(), cmd.tag.clone());
                let id = self.transport.request(
                    json!({"service":"agent_sessions","op":args["op"],"sessionIds":args["sessionIds"]}),
                    false,
                    ctx.link().callback(move |reply| {
                        Msg::KernelHosted(m.clone(), t.clone(), kernel_panel::After::Reply, reply)
                    }),
                );
                if !cmd.tag.is_empty() {
                    self.kernel
                        .calls
                        .insert((module.to_owned(), cmd.tag.clone()), id);
                }
                Vec::new()
            }
            "page.eval" | "page.network" => {
                let link = ctx.link().clone();
                let (m, t) = (module.to_owned(), cmd.tag.clone());
                let expression = args["expression"].as_str().unwrap_or_default().to_owned();
                let network = cmd.name == "page.network";
                spawn_local(async move {
                    let result = if network {
                        page_network().await
                    } else {
                        page_eval(&expression).await
                    };
                    link.send_message(Msg::PageReplied(m, t, result));
                });
                Vec::new()
            }
            _ => return None,
        })
    }

    /// The per-window id the Host checks workspace tool calls against.
    fn agent_workspace_id(&self) -> String {
        let storage = window().session_storage().ok().flatten();
        const KEY: &str = "servicesConsole.agentWorkspace";
        if let Some(id) = storage
            .as_ref()
            .and_then(|storage| storage.get_item(KEY).ok().flatten())
        {
            return id;
        }
        let id = uuid::Uuid::now_v7().to_string();
        if let Some(storage) = storage {
            let _ = storage.set_item(KEY, &id);
        }
        id
    }

    /// Stops the turn `tag`: its console requests stop, its approvals and edits go.
    pub(super) fn agent_stop(&mut self, ctx: &Context<Self>, tag: &str) {
        let Some(turn) = self.agent_turns.get_mut(tag) else {
            return;
        };
        turn.stopping = true;
        let stream = turn.stream;
        self.stop_agent_requests(ctx, tag);
        self.agent_revoke(tag, "Agent turn ended; approval revoked");
        if let Err(error) = self.transport.stop(stream) {
            self.notice = Some(Notice::new(format!("Agent · Stop failed: {error}")));
        }
    }

    /// Stops every turn (Console hidden).
    pub(super) fn agent_stop_all(&mut self, ctx: &Context<Self>) {
        let tags: Vec<String> = self.agent_turns.keys().cloned().collect();
        for tag in tags {
            self.agent_stop(ctx, &tag);
        }
    }

    /// The pending approval and edit of the turn `tag` end with `error`.
    fn agent_revoke(&mut self, tag: &str, error: &str) {
        if self
            .agent_run
            .as_ref()
            .is_some_and(|run| run.request.session == tag)
        {
            let pending = self.agent_run.take().unwrap();
            pending.request.reply.emit(json!({ "error": error }));
        }
        if self
            .agent_edit
            .as_ref()
            .is_some_and(|edit| edit.request.session == tag)
        {
            let pending = self.agent_edit.take().unwrap();
            pending
                .request
                .reply
                .emit(json!({"error":"Agent turn ended before edit acknowledgement"}));
        }
    }

    /// The console run waiting for approval in turn `tag` changed: the module shows it.
    pub(super) fn agent_approval(&mut self, ctx: &Context<Self>, tag: &str) {
        let Some(turn) = self.agent_turns.get(tag) else {
            return;
        };
        let approval = self
            .agent_run
            .as_ref()
            .filter(|run| run.request.session == tag && !run.started)
            .map(|run| {
                json!({"service":run.approval.service,"title":run.approval.title,
                    "request":run.approval.request,"endpoint":run.approval.endpoint})
            });
        let module = turn.module.clone();
        let effects = crate::kernel::with(|modules| {
            modules.reply(
                &module,
                tag,
                Ok(json!({"type":"approval","approval":approval})),
            )
        });
        self.kernel_effects(ctx, effects);
    }

    /// An event of the turn `tag`: Copilot's, a tool call, or its end.
    pub(super) fn agent_reply(
        &mut self,
        ctx: &Context<Self>,
        module: &str,
        tag: &str,
        reply: Reply,
    ) {
        let Some(turn) = self.agent_turns.get(tag) else {
            return;
        };
        let stream = turn.stream;
        let stopping = turn.stopping;
        match reply {
            Reply::Data(value) if value["type"] == "workspace" => {
                let call_id = value["callId"].as_str().unwrap_or_default().to_owned();
                let tool = value["tool"].as_str().unwrap_or_default().to_owned();
                if value["workspaceId"].as_str() != Some(self.agent_workspace_id().as_str())
                    || stopping
                {
                    if let Err(error) = self.transport.agent_reply(
                        stream,
                        &call_id,
                        json!({"error":"Workspace access revoked"}),
                    ) {
                        self.notice = Some(Notice::new(format!("Agent · ERROR · {error}")));
                    }
                } else if tool.starts_with("page_") {
                    if let Some(turn) = self.agent_turns.get_mut(tag) {
                        turn.calls.insert(call_id.clone());
                    }
                    let args = json!({"turn": tag, "arguments": value["arguments"]});
                    let effects = crate::kernel::with(|modules| {
                        modules.tool_call(module, &call_id, &tool, &args)
                    });
                    self.kernel_effects(ctx, effects);
                } else {
                    let session = tag.to_owned();
                    let reply = ctx.link().callback(move |result| {
                        Msg::AgentToolResult(session.clone(), call_id.clone(), result)
                    });
                    let request = WorkspaceRequest {
                        session: tag.to_owned(),
                        tool,
                        arguments: value["arguments"].clone(),
                        reply,
                    };
                    if let Err(error) = self.agent_tool(ctx, &request) {
                        request.reply.emit(json!({ "error": error }));
                    } else if request.tool == "console_run_check" {
                        self.agent_approval(ctx, tag);
                    }
                }
            }
            Reply::Data(value) => {
                let effects = crate::kernel::with(|modules| modules.reply(module, tag, Ok(value)));
                self.kernel_effects(ctx, effects);
            }
            Reply::Result(value) | Reply::Exit(value) | Reply::Error(value) => {
                self.agent_turns.remove(tag);
                self.kernel
                    .calls
                    .remove(&(module.to_owned(), tag.to_owned()));
                self.stop_agent_requests(ctx, tag);
                self.agent_revoke(tag, "Agent turn ended; approval revoked");
                let mut effects = crate::kernel::with(|modules| {
                    modules.reply(module, tag, Ok(json!({"type":"result","result":value})))
                });
                effects.extend(crate::kernel::with(|modules| modules.end(module, tag)));
                self.kernel_effects(ctx, effects);
            }
        }
    }

    /// A console tool of the turn `tag` answered.
    pub(super) fn agent_tool_result(&mut self, tag: &str, call_id: &str, result: Value) {
        let Some(turn) = self.agent_turns.get(tag).filter(|turn| !turn.stopping) else {
            return;
        };
        if let Err(error) = self.transport.agent_reply(turn.stream, call_id, result) {
            self.notice = Some(Notice::new(format!("Agent · ERROR · {error}")));
        }
    }
}

fn inspected() -> Result<JsValue, String> {
    let inspected = get(
        &get(&get(&js_sys::global(), "chrome"), "devtools"),
        "inspectedWindow",
    );
    if !get(&inspected, "eval").is_function() || get(&inspected, "tabId").as_f64().is_none() {
        return Err(UNAVAILABLE.into());
    }
    Ok(inspected)
}

/// Evaluates `expression` on the inspected page; the answer is its JSON value.
async fn page_eval(expression: &str) -> Result<Value, String> {
    let inspected = inspected()?;
    let expression = expression.to_owned();
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let fail = reject.clone();
        let callback = Closure::once_into_js(move |value: JsValue, exception: JsValue| {
            if get(&exception, "isException").as_bool() == Some(true)
                || get(&exception, "isError").as_bool() == Some(true)
            {
                let message = get(&exception, "value").as_string()
                    .or_else(|| get(&exception, "description").as_string())
                    .unwrap_or_else(|| "Inspected page evaluation failed; navigation may have occurred. Read a fresh snapshot before retrying.".into());
                let _ = reject.call1(&JsValue::NULL, &message.into());
            } else {
                let _ = resolve.call1(&JsValue::NULL, &value);
            }
        });
        if let Err(error) = call(&inspected, "eval", &[expression.clone().into(), callback]) {
            let _ = fail.call1(&JsValue::NULL, &error);
        }
    });
    let value = await_reply(promise).await?;
    serde_wasm_bindgen::from_value(value)
        .map_err(|error| format!("Invalid inspected page reply: {error}"))
}

/// The requests DevTools recorded: `{entries: [{url, method, status, time, bodySize, started}]}`.
async fn page_network() -> Result<Value, String> {
    inspected()?;
    let network = get(
        &get(&get(&js_sys::global(), "chrome"), "devtools"),
        "network",
    );
    if !get(&network, "getHAR").is_function() {
        return Err("Network inspection is unavailable in this DevTools context.".into());
    }
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let callback = Closure::once_into_js(move |value: JsValue| {
            let _ = resolve.call1(&JsValue::NULL, &value);
        });
        if let Err(error) = call(&network, "getHAR", &[callback]) {
            let _ = reject.call1(&JsValue::NULL, &error);
        }
    });
    let har = await_reply(promise).await?;
    let entries = get(&har, "entries");
    if !js_sys::Array::is_array(&entries) {
        return Err("DevTools returned an invalid Network log.".into());
    }
    let entries: Vec<Value> = js_sys::Array::from(&entries)
        .iter()
        .map(|entry| {
            let request = get(&entry, "request");
            let response = get(&entry, "response");
            json!({
                "url": get(&request, "url").as_string(),
                "method": get(&request, "method").as_string(),
                "status": get(&response, "status").as_f64(),
                "time": get(&entry, "time").as_f64(),
                "bodySize": get(&response, "bodySize").as_f64(),
                "started": get(&entry, "startedDateTime")
                    .as_string()
                    .map(|value| js_sys::Date::parse(&value))
                    .filter(|value| value.is_finite()),
            })
        })
        .collect();
    Ok(json!({ "entries": entries }))
}

/// Waits for the inspected page at most 15 seconds.
async fn await_reply(promise: js_sys::Promise) -> Result<JsValue, String> {
    let window = window();
    let mut timer = None;
    let deadline = js_sys::Promise::new(&mut |_, reject| {
        let fail = reject.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            let _ = fail.call1(&JsValue::NULL, &"Inspected page did not respond within 15 seconds. Navigation may have occurred; inspect fresh state before retrying.".into());
        });
        match window.set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            15_000,
        ) {
            Ok(id) => timer = Some((id, callback)),
            Err(error) => {
                let _ = reject.call1(&JsValue::NULL, &error);
            }
        }
    });
    let value = JsFuture::from(js_sys::Promise::race(&js_sys::Array::of2(
        &promise, &deadline,
    )))
    .await;
    if let Some((id, _callback)) = timer {
        window.clear_timeout_with_handle(id);
    }
    value.map_err(error)
}

use super::super::{call, get};
