//! The console tools of the Agent: the Kernel hands the Shell the tool calls of the Agent
//! turns a module started; the Shell answers them from the console, holds the run waiting
//! for approval and hands what the Agent's runs end with back to the turn.

use super::*;

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

impl App {
    /// A console tool call of the Agent turn `turn`.
    pub(super) fn agent_tool_call(
        &mut self,
        ctx: &Context<Self>,
        turn: String,
        call: String,
        tool: String,
        arguments: Value,
    ) {
        let session = turn.clone();
        let reply = ctx
            .link()
            .callback(move |result| Msg::AgentToolResult(session.clone(), call.clone(), result));
        let request = WorkspaceRequest {
            session: turn.clone(),
            tool,
            arguments,
            reply,
        };
        if let Err(error) = self.agent_tool(ctx, &request) {
            request.reply.emit(json!({ "error": error }));
        } else if request.tool == "console_run_check" {
            let approval = self.agent_approval(&turn);
            self.kernel.agent_approval(&turn, approval);
        }
    }

    /// The pending approval and edit of the turn `tag` end with `error`.
    pub(super) fn agent_revoke(&mut self, tag: &str, error: &str) {
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

    /// The console run waiting for approval in the turn `tag`, as its module shows it.
    pub(super) fn agent_approval(&self, tag: &str) -> Option<Value> {
        self.agent_run
            .as_ref()
            .filter(|run| run.request.session == tag && !run.started)
            .map(|run| {
                json!({"service":run.approval.service,"title":run.approval.title,
                    "request":run.approval.request,"endpoint":run.approval.endpoint})
            })
    }

    /// The approved Agent run `key` ended: the Agent hears how.
    pub(super) fn agent_run_ended(&mut self, key: &str) {
        let Some(run) = self.runs.get(key).filter(|run| !run.view.running) else {
            return;
        };
        if !self
            .agent_run
            .as_ref()
            .is_some_and(|pending| pending.started && pending.operation == key)
        {
            return;
        }
        let pending = self.agent_run.take().expect("checked");
        let view = &run.view;
        pending.request.reply.emit(if view.result["cancelled"] == true {
            json!({"error":"Request cancelled by the user","cancelled":true,"status":view.status})
        } else if view.result.get("error").is_some() {
            json!({"error":error_text(&view.result),"status":view.status,
                "details":"Read console_read_debug for this check's full Debug"})
        } else {
            json!({"complete":true,"service":view.service,"checkId":pending.check_id,"status":view.status})
        });
    }
}
