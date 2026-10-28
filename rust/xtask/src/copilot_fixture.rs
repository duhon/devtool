use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, Write},
};

#[derive(Default)]
struct Peer {
    next: u64,
    sessions: HashMap<String, bool>,
    pending: HashMap<String, (String, String, Value)>,
}
impl Peer {
    fn send(&self, value: Value) -> Result<()> {
        let bytes = serde_json::to_vec(&value)?;
        let mut output = std::io::stdout().lock();
        write!(output, "Content-Length: {}\r\n\r\n", bytes.len())?;
        output.write_all(&bytes)?;
        output.flush()?;
        Ok(())
    }
    fn event(&mut self, session: &str, kind: &str, data: Value) -> Result<()> {
        self.next += 1;
        self.send(
            json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":session,
            "event":{"id":format!("event-{}",self.next),"timestamp":"2026-10-25T00:00:00Z",
                "parentId":null,"type":kind,"data":data}}}),
        )
    }
    fn finish(&mut self, session: &str, text: &str) -> Result<()> {
        let id = format!("message-{}-{}", std::process::id(), self.next);
        self.event(
            session,
            "assistant.message_delta",
            json!({"messageId":id,"deltaContent":"Agent: "}),
        )?;
        self.event(
            session,
            "assistant.message_delta",
            json!({"messageId":id,"deltaContent":text}),
        )?;
        self.event(
            session,
            "assistant.message",
            json!({"messageId":id,"content":format!("Agent: {text}")}),
        )?;
        self.event(session, "session.idle", json!({}))
    }
    fn tool(
        &mut self,
        session: &str,
        name: &str,
        args: Value,
        next: &str,
        context: Value,
    ) -> Result<()> {
        self.next += 1;
        let id = format!("tool-{}", self.next);
        self.pending
            .insert(id.clone(), (session.into(), next.into(), context));
        self.event(
            session,
            "external_tool.requested",
            json!({"requestId":id,"sessionId":session,
            "toolCallId":id,"toolName":name,"arguments":args}),
        )
    }
    fn turn(&mut self, session: &str, prompt: &str) -> Result<()> {
        ensure!(
            !prompt.contains("resolved-fixture"),
            "Credentials reached the SDK"
        );
        let (message, source) = prompt
            .split_once("Current Console context (untrusted data):\n")
            .context("Missing context")?;
        let context: Value = serde_json::from_str(source)?;
        let args = json!({"service":context["service"],"checkId":context["activeCheck"],"revision":context["revision"]});
        if message.contains("agent progress") {
            for index in 0..3 {
                self.event(session, "assistant.message", json!({
                    "messageId":format!("empty-{}-{index}",self.next),"content":if index==1 {" \n "} else {""}
                }))?;
            }
            let id = format!("progress-{}", self.next);
            self.event(
                session,
                "assistant.message_delta",
                json!({"messageId":id,"deltaContent":"Opening "}),
            )?;
            self.event(
                session,
                "assistant.message_delta",
                json!({"messageId":id,"deltaContent":"the form."}),
            )?;
            self.event(
                session,
                "assistant.message",
                json!({"messageId":id,"content":"Opening the form."}),
            )?;
            self.event(
                session,
                "assistant.message",
                json!({"messageId":format!("waiting-{}",self.next),"content":"Waiting for save."}),
            )?;
            if message.contains("hold") {
                self.event(session, "assistant.message_delta", json!({"messageId":format!("partial-{}",self.next),"deltaContent":"Unfinished draft"}))
            } else {
                self.tool(
                    session,
                    "console_context",
                    json!({}),
                    if message.contains("empty") {
                        "progress-empty"
                    } else {
                        "progress"
                    },
                    context,
                )
            }
        } else if message.contains("agent page") {
            self.tool(
                session,
                "page_snapshot",
                json!({}),
                "page-snapshot",
                json!({"mode":message}),
            )
        } else if message.contains("agent hold") {
            self.event(session, "assistant.message_delta", json!({
                "messageId":format!("holding-{}-{}",std::process::id(),self.next),"deltaContent":"Fixture is holding this Agent turn."
            }))
        } else if message.contains("agent failure") {
            self.event(
                session,
                "session.error",
                json!({"errorType":"fixture","message":"Agent fixture failure"}),
            )?;
            self.event(session, "session.idle", json!({}))
        } else if message.contains("agent edit") {
            let mut args = args;
            args["request"] = json!("#headers\nX-Agent: edited\n");
            self.tool(session, "console_edit_check", args, "edit", context)
        } else if message.contains("agent rename") {
            let mut args = args;
            let combined = message.contains("and edit");
            args["title"] = json!(if combined {
                "products updated"
            } else {
                "products overview"
            });
            if combined {
                args["request"] = json!("#headers\nX-Agent: renamed");
            }
            self.tool(
                session,
                "console_edit_check",
                args,
                if combined { "rename-edit" } else { "rename" },
                context,
            )
        } else if message.contains("agent bad title") {
            let mut args = args;
            args["title"] = json!("#check products overview");
            args["request"] = json!("#headers\nX-Agent: rejected");
            self.tool(session, "console_create_check", args, "bad-title", context)
        } else if message.contains("agent stale rename") {
            let mut args = args;
            args["revision"] = json!("0");
            args["title"] = json!("Stale rename");
            self.tool(session, "console_edit_check", args, "stale", context)
        } else if message.contains("agent create") {
            let request = context["request"]["text"]
                .as_str()
                .context("Missing request")?;
            let endpoint = request
                .split_whitespace()
                .find(|line| line.starts_with("http://"))
                .context("Missing endpoint")?;
            let origin = endpoint
                .strip_prefix("http://")
                .context("Expected HTTP endpoint")?
                .split('/')
                .next()
                .context("Missing host")?;
            self.tool(session,"console_create_check",json!({"service":context["service"],"revision":context["revision"],
                "title":"Created by agent","request":format!("#endpoint\nGET http://{origin}/agent-created")}),"create",context)
        } else if message.contains("agent run") || message.contains("agent stop") {
            self.tool(session, "console_run_check", args, "run", context)
        } else if message.contains("agent switch") {
            self.tool(
                session,
                "console_select_service",
                json!({"service":"mysql"}),
                "switch",
                context,
            )
        } else if message.contains("agent stale edit") {
            let mut args = args;
            args["revision"] = json!("0");
            args["request"] = json!("GET");
            self.tool(session, "console_edit_check", args, "stale", context)
        } else if message.contains("agent guide") {
            self.tool(
                session,
                "console_skill",
                json!({"topic":"discovery"}),
                "guide-discovery",
                context,
            )
        } else if message.contains("agent markdown") {
            self.finish(
                session,
                r#"Markdown ready

## Formatted reply

Current request: `#check test1`.

- **Method and URL:** `PUT https://fakerestapi.azurewebsites.net/api/v1/Books/123`.
- *Body:* `{"id": 111}`.

```sql
SELECT id, name
FROM products;
```

| Field | Value |
| --- | --- |
| id | 123 |

> Review the ID before running.

- [x] Request reviewed

[Documentation](https://example.invalid/docs)
[Unsafe link](javascript:alert%281%29)
[Unsafe data](data:text/html,unsafe)
![External image](https://images.fixture.invalid/tracker)

Raw HTML: <img src="https://images.fixture.invalid/raw" onerror="globalThis.agentMarkdownExecuted=true">

<script>globalThis.agentMarkdownExecuted=true</script>
"#,
            )
        } else {
            self.tool(session, "console_skill", json!({}), "skill", context)
        }
    }
    fn tool_result(&mut self, id: &str, result: Value) -> Result<()> {
        let (session, next, context) = self.pending.remove(id).context("Unknown tool response")?;
        if next.starts_with("page-probe-") {
            ensure!(
                result["error"].is_string(),
                "Unsafe page operation succeeded: {next}: {result}"
            );
            let step: usize = next.trim_start_matches("page-probe-").parse()?;
            return self.page_probe(&session, step + 1, context);
        }
        match next.as_str() {
            "progress" => self.finish(&session, "Progress final answer."),
            "progress-empty" => {
                self.event(
                    &session,
                    "assistant.message",
                    json!({"messageId":format!("empty-final-{}",self.next),"content":" \n "}),
                )?;
                self.event(&session, "session.idle", json!({}))
            }
            "page-snapshot" => {
                if let Some(error) = result["error"].as_str() {
                    return self.finish(&session, &format!("Page rejected: {error}"));
                }
                ensure!(result["untrusted"] == true && result["pageId"].is_string());
                let serialized = result.to_string();
                for secret in [
                    "field-secret",
                    "password-secret",
                    "textarea-secret",
                    "editable-secret",
                    "hidden-secret",
                    "cookie-secret",
                    "storage-secret",
                ] {
                    ensure!(
                        !serialized.contains(secret),
                        "Page snapshot exposed {secret}"
                    );
                }
                let mut context = context;
                context["pageId"] = result["pageId"].clone();
                let mode = context["mode"].as_str().unwrap_or("");
                if mode.contains("guide permission") {
                    self.tool(
                        &session,
                        "page_click",
                        json!({"pageId":context["pageId"],"selector":"#apply"}),
                        "guide-blocked",
                        context,
                    )
                } else if mode.contains("guide") {
                    self.guide(&session, context, 1)
                } else if mode.contains("actions") {
                    self.tool(&session, "page_fill", json!({"pageId":context["pageId"],"selector":"#name","value":"Agent filled"}), "page-filled", context)
                } else if mode.contains("constraints") {
                    self.page_probe(&session, 0, context)
                } else {
                    ensure!(
                        result["text"]
                            .as_str()
                            .is_some_and(|text| text.contains("Inspected browser content"))
                    );
                    self.finish(&session, "Page snapshot ready")
                }
            }
            "page-guide" => {
                if let Some(error) = result["error"].as_str() {
                    return self.finish(&session, &format!("Guide rejected: {error}"));
                }
                let action = result["action"].as_str().context("Missing guide action")?;
                let mode = context["mode"].as_str().unwrap_or("");
                if action == "next" && mode.contains("mutation") {
                    return self.tool(
                        &session,
                        "page_click",
                        json!({"pageId":context["pageId"],"selector":"#apply"}),
                        "guide-blocked",
                        context,
                    );
                }
                if mode.contains("walkthrough") && matches!(action, "next" | "back") {
                    let step = context["guideStep"]
                        .as_u64()
                        .context("Missing guide step")?;
                    let step = if action == "back" { step - 1 } else { step + 1 };
                    if step <= 2 {
                        let mut context = context;
                        context["guideStep"] = json!(step);
                        return self.tool(
                            &session,
                            "page_snapshot",
                            json!({}),
                            "guide-fresh",
                            context,
                        );
                    }
                }
                self.tool(
                    &session,
                    "page_guide_clear",
                    json!({}),
                    &format!("guide-ended-{action}"),
                    context,
                )
            }
            "guide-fresh" => {
                ensure!(
                    result["pageId"].is_string(),
                    "Guide could not inspect fresh state: {result}"
                );
                let step = context["guideStep"]
                    .as_u64()
                    .context("Missing next guide step")?;
                let mut context = context;
                context["pageId"] = result["pageId"].clone();
                self.guide(&session, context, step)
            }
            "guide-blocked" => {
                ensure!(
                    result["error"]
                        .as_str()
                        .is_some_and(|error| error.contains("guide-only")),
                    "Guide performed a site action: {result}"
                );
                self.finish(&session, "Guide mutation blocked")
            }
            "page-filled" => {
                ensure!(
                    result["performed"] == "page_fill",
                    "Page fill failed: {result}"
                );
                self.tool(
                    &session,
                    "page_select",
                    json!({"pageId":context["pageId"],"selector":"#choice","value":"two"}),
                    "page-selected",
                    context,
                )
            }
            "page-selected" => {
                ensure!(
                    result["performed"] == "page_select",
                    "Page select failed: {result}"
                );
                self.tool(
                    &session,
                    "page_click",
                    json!({"pageId":context["pageId"],"selector":"#apply"}),
                    "page-clicked",
                    context,
                )
            }
            "page-clicked" => {
                ensure!(
                    result["performed"] == "page_click",
                    "Page click failed: {result}"
                );
                self.tool(
                    &session,
                    "page_scroll",
                    json!({"pageId":context["pageId"],"y":500}),
                    "page-scrolled",
                    context,
                )
            }
            "page-scrolled" => {
                ensure!(
                    result["y"].as_f64().is_some_and(|y| y >= 400.0),
                    "Page did not scroll: {result}"
                );
                self.finish(&session, "Page actions complete")
            }
            "edit" | "create" => {
                ensure!(
                    result.get("error").is_none(),
                    "Workspace mutation failed: {result}"
                );
                self.finish(
                    &session,
                    if next == "edit" {
                        "Edited live check"
                    } else {
                        "Created live check"
                    },
                )
            }
            "run" => {
                if let Some(error) = result["error"].as_str() {
                    return self.finish(&session, &format!("Run rejected: {error}"));
                }
                ensure!(result["complete"] == true, "Run not completed");
                self.tool(
                    &session,
                    "console_read_response",
                    json!({"service":context["service"],
                    "checkId":context["activeCheck"],"limit":1000}),
                    "response",
                    context,
                )
            }
            "bad-title" => {
                ensure!(
                    result["error"]
                        .as_str()
                        .is_some_and(|error| error.contains("plain check name"))
                );
                self.finish(&session, "Prefixed title rejected")
            }
            "rename" | "rename-edit" => {
                ensure!(result.get("error").is_none(), "Rename failed: {result}");
                ensure!(
                    result["checks"][0]["id"] == context["activeCheck"],
                    "Renaming must retain check identity"
                );
                ensure!(
                    result["revision"] != context["revision"],
                    "Renaming must advance document revision"
                );
                ensure!(
                    result["checks"][0]["title"]
                        == if next == "rename-edit" {
                            "products updated"
                        } else {
                            "products overview"
                        }
                );
                self.tool(&session,"console_read_response",
                    json!({"service":context["service"],"checkId":context["activeCheck"],"limit":1000}),
                    if next=="rename-edit" {"rename-edit-response"}else{"rename-response"},context)
            }
            "rename-response" | "rename-edit-response" => {
                ensure!(
                    result["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("agent-approved-response")),
                    "Renaming must preserve captured response ownership"
                );
                self.finish(
                    &session,
                    if next == "rename-edit-response" {
                        "Renamed and edited live check"
                    } else {
                        "Renamed live check; captured response retained"
                    },
                )
            }
            "response" => self.finish(
                &session,
                &format!(
                    "Read response: {}",
                    result["text"]
                        .as_str()
                        .context("Missing captured response")?
                ),
            ),
            "switch" => self.tool(&session, "console_context", json!({}), "switched", context),
            "switched" => {
                ensure!(result["service"] == "mysql");
                self.finish(&session, "Switched to mysql")
            }
            "stale" => {
                ensure!(
                    result["error"]
                        .as_str()
                        .is_some_and(|error| error.contains("CONFLICT"))
                );
                self.finish(&session, "Stale edit rejected")
            }
            "skill" => {
                ensure!(
                    result["content"]
                        .as_str()
                        .is_some_and(|skill| skill.contains("one-time approval"))
                );
                self.finish(
                    &session,
                    if self.sessions[&session] {
                        "Resumed Copilot session"
                    } else {
                        "Copilot session ready"
                    },
                )
            }
            "guide-discovery" => {
                ensure!(
                    result["topic"] == "discovery"
                        && result["version"] == env!("CARGO_PKG_VERSION")
                );
                let content = result["content"]
                    .as_str()
                    .context("Missing discovery guide")?;
                ensure!(
                    content.contains("There is no Discover button")
                        && content.contains("Ctrl+Space")
                        && content.contains("/swagger/v1/swagger.json")
                        && content.contains("schemaAvailable")
                );
                self.tool(
                    &session,
                    "console_skill",
                    json!({"topic":"interface"}),
                    "guide-interface",
                    context,
                )
            }
            "guide-interface" => {
                let content = result["content"]
                    .as_str()
                    .context("Missing interface guide")?;
                ensure!(
                    result["topic"] == "interface"
                        && content.contains("Shift+Enter")
                        && content.contains("legacy")
                        && content.contains("Dictionary")
                );
                self.tool(
                    &session,
                    "console_skill",
                    json!({"topic":"services"}),
                    "guide-services",
                    context,
                )
            }
            "guide-services" => {
                let content = result["content"]
                    .as_str()
                    .context("Missing service guide")?;
                ensure!(
                    result["topic"] == "services"
                        && content.contains("#endpoint")
                        && content.contains("#message")
                        && content.contains("#command")
                );
                self.tool(
                    &session,
                    "console_skill",
                    json!({"topic":"variables"}),
                    "guide-variables",
                    context,
                )
            }
            "guide-variables" => {
                let content = result["content"]
                    .as_str()
                    .context("Missing variable guide")?;
                ensure!(
                    result["topic"] == "variables"
                        && content.contains("{{profile:NAME}}")
                        && content.contains(".dictionary-profiles.json")
                        && content.contains("read-only")
                );
                self.finish(
                    &session,
                    "Application knowledge ready: interface, discovery, services, variables.",
                )
            }
            next if next.starts_with("guide-ended-") => {
                ensure!(
                    result["cleared"] == false,
                    "Finished guide retained overlay ownership: {result}"
                );
                self.finish(
                    &session,
                    &format!("Guide ended: {}", next.trim_start_matches("guide-ended-")),
                )
            }
            _ => bail!("Unknown fixture continuation"),
        }
    }
    fn guide(&mut self, session: &str, mut context: Value, step: u64) -> Result<()> {
        context["guideStep"] = json!(step);
        let mut args = json!({"pageId":context["pageId"],"selector":if step==1 {"#name"} else {"#apply"},
            "title":if step==1 {"Product name"} else {"Save product"},
            "text":"Do this yourself, then choose Next. <img src=x onerror=alert(1)>",
            "step":step,"total":2});
        if context["mode"]
            .as_str()
            .is_some_and(|mode| mode.contains("invalid"))
        {
            args["total"] = json!(0);
        }
        if context["mode"]
            .as_str()
            .is_some_and(|mode| mode.contains("dynamic"))
        {
            args.as_object_mut()
                .context("Guide arguments must be an object")?
                .remove("total");
        }
        self.tool(session, "page_guide_show", args, "page-guide", context)
    }
    fn page_probe(&mut self, session: &str, step: usize, context: Value) -> Result<()> {
        let probes = [
            (
                "page_click",
                json!({"pageId":"stale-document","selector":"#apply"}),
            ),
            ("page_click", json!({"selector":".ambiguous"})),
            ("page_fill", json!({"selector":"#password","value":"no"})),
            ("page_fill", json!({"selector":"#file","value":"no"})),
            ("page_fill", json!({"selector":"#hidden","value":"no"})),
            ("page_fill", json!({"selector":"#readonly","value":"no"})),
            ("page_fill", json!({"selector":"#disabled","value":"no"})),
            ("page_click", json!({"selector":"#newtab"})),
            ("page_click", json!({"selector":"#unsafe"})),
            (
                "page_select",
                json!({"selector":"#choice","value":"missing"}),
            ),
            ("page_scroll", json!({"y":2001})),
            ("page_scroll", json!({"x":false,"y":0})),
            ("page_click", json!({"selector":"#same-tab"})),
        ];
        let Some((tool, mut args)) = probes.get(step).cloned() else {
            return self.finish(session, "Page constraints verified");
        };
        if args.get("pageId").is_none() {
            args["pageId"] = context["pageId"].clone();
        }
        self.tool(session, tool, args, &format!("page-probe-{step}"), context)
    }
    fn handle(&mut self, request: Value) -> Result<()> {
        let method = request["method"].as_str().context("Missing RPC method")?;
        let params = &request["params"];
        let session = params["sessionId"].as_str().unwrap_or("");
        let result = match method {
            "connect" => json!({"ok":true,"protocolVersion":3,"version":"services-sdk-acceptance"}),
            "ping" => json!({"message":params["message"],"timestamp":0,"protocolVersion":3}),
            "session.create" | "session.resume" => {
                ensure!(params["availableTools"] == json!(["custom:*"]));
                ensure!(params["requestPermission"] == true);
                let tools = params["tools"].as_array().context("Missing custom tools")?;
                ensure!(
                    tools.len() == 16 && tools.iter().all(|tool| tool["skipPermission"] == true)
                );
                for name in [
                    "page_snapshot",
                    "page_click",
                    "page_fill",
                    "page_select",
                    "page_scroll",
                    "page_guide_show",
                    "page_guide_clear",
                ] {
                    ensure!(
                        tools.iter().any(|tool| tool["name"] == name),
                        "Missing page tool {name}"
                    );
                }
                ensure!(
                    params["systemMessage"]["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("one-time approval")
                            && text.contains(include_str!(
                                "../../native/agent-skills/services-console/SKILL.md"
                            )))
                );
                self.sessions
                    .insert(session.into(), method == "session.resume");
                json!({"sessionId":session,"workspacePath":params["workingDirectory"]})
            }
            "session.eventLog.registerInterest" => {
                json!({"handle":format!("interest-{}",self.next)})
            }
            "session.eventLog.releaseInterest"
            | "session.skills.reload"
            | "session.options.update"
            | "session.destroy"
            | "session.detach"
            | "session.abort"
            | "session.tools.handlePendingToolCall" => json!({"success":true}),
            "session.send" => json!({"messageId":format!("user-{}",self.next)}),
            _ => {
                return self.send(json!({"jsonrpc":"2.0","id":request["id"],
                    "error":{"code":-32601,"message":format!("Unsupported acceptance RPC: {method}")}}));
            }
        };
        self.send(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))?;
        match method {
            "session.send" => self.turn(
                session,
                params["prompt"].as_str().context("Missing prompt")?,
            ),
            "session.tools.handlePendingToolCall" => self.tool_result(
                params["requestId"].as_str().context("Missing requestId")?,
                serde_json::from_str(
                    params["result"]
                        .as_str()
                        .context("Expected text tool result")?,
                )?,
            ),
            _ => Ok(()),
        }
    }
}
pub fn run() -> Result<()> {
    let home = std::env::var_os("COPILOT_HOME").context("SDK must isolate COPILOT_HOME")?;
    std::fs::write(
        std::path::PathBuf::from(home).join(format!("acceptance-peer-{}.pid", std::process::id())),
        std::process::id().to_string(),
    )?;
    let mut input = std::io::stdin().lock();
    let mut peer = Peer::default();
    loop {
        let mut size = None;
        loop {
            let mut header = String::new();
            if input.read_line(&mut header)? == 0 {
                return Ok(());
            }
            if header == "\r\n" {
                break;
            }
            if let Some(length) = header.strip_prefix("Content-Length: ") {
                size = Some(length.trim().parse::<usize>()?);
            }
        }
        let size = size.context("Missing Content-Length")?;
        ensure!(size <= 2 * 1024 * 1024, "Oversized SDK frame");
        let mut body = vec![0; size];
        std::io::Read::read_exact(&mut input, &mut body)?;
        peer.handle(serde_json::from_slice(&body)?)?;
    }
}
