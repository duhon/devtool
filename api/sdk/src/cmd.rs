//! Commands a module asks the Kernel to run. Each carries the module's `tag`; the answer
//! comes back as `Event::Reply` with that tag (a stream: several replies, then `End`).

use crate::wire::{Cmd, DriverCall, json};
use serde::Serialize;
use serde_json::{Value, json};

/// Every command the Kernel knows; anything else is answered with an error.
pub const NAMES: &[&str] = &[
    "driver.call",
    "driver.stream",
    "driver.cancel",
    "kv.get",
    "kv.put",
    "kv.delete",
    "kv.watch",
    "history.insert",
    "history.search",
    "os.env",
    "vars.resolve",
    "clipboard.read",
    "clipboard.write",
    "sources.read",
    "editor.contribute",
    "editor.relight",
    "document.read",
    "document.insert",
    "document.replace",
    "document.select",
    "document.run",
    "workspace.list",
    "workspace.restore",
    "workspace.copy",
    "workspace.delete",
    "workspace.rename",
    "workspace.retry",
    "page.eval",
    "page.network",
    "agent.turn",
    "agent.stop",
    "agent.reply",
    "agent.approve",
    "agent.sessions",
    "agent.models",
    "agent.options",
    "modules.check",
    "modules.install",
    "modules.remove",
    "sources.list",
    "sources.save",
    "sources.token",
    "sources.apply",
    "sources.open",
    "app.update",
    "app.reload",
    "lock.acquire",
    "lock.release",
    "timer.after",
    "status.set",
    "run.output",
    "run.result",
    "notify",
    "log",
    "diagnostics",
];

impl Cmd {
    pub fn new(tag: impl Into<String>, name: &str, args: &impl Serialize) -> Self {
        Self {
            tag: tag.into(),
            name: name.into(),
            args: json(args),
        }
    }
}

/// Runs a driver call; a streaming call answers with several replies and an end.
pub fn driver(tag: impl Into<String>, call: &DriverCall) -> Cmd {
    let name = if call.stream {
        "driver.stream"
    } else {
        "driver.call"
    };
    Cmd::new(
        tag,
        name,
        &json!({"driver": call.driver, "request": call.request()}),
    )
}

/// Cancels the driver call or stream started with `target`.
pub fn cancel(target: &str) -> Cmd {
    Cmd::new(target, "driver.cancel", &json!({}))
}

/// Resolves the `{{…}}` variables in `text`; the reply is the resolved text as a string.
pub fn resolve(tag: impl Into<String>, text: &str) -> Cmd {
    Cmd::new(tag, "vars.resolve", &json!({"text": text}))
}

/// Like [`resolve`], but each value is escaped as the content of a JSON string.
pub fn resolve_json(tag: impl Into<String>, text: &str) -> Cmd {
    Cmd::new(tag, "vars.resolve", &json!({"text": text, "mode": "json"}))
}

/// Appends `text` to the response the run `run` shows while it runs.
pub fn output(run: &str, text: &str) -> Cmd {
    Cmd::new(run, "run.output", &json!({"text": text}))
}

/// The status of the run `run` (like "3 rows"), shown after its duration and outcome.
pub fn run_status(run: &str, text: &str) -> Cmd {
    Cmd::new(run, "status.set", &json!({"text": text, "indicator": ""}))
}

/// Finishes the run `run`: the response it shows and saves; the Kernel cancels the run's
/// calls still open and releases its locks.
pub fn result(run: &str, rendered: &crate::Rendered, debug: &Value) -> Cmd {
    Cmd::new(
        run,
        "run.result",
        &json!({"body": rendered.text, "language": rendered.language,
            "outcome": rendered.outcome, "debug": debug,
            "columns": rendered.columns.iter()
                .map(|column| [&column.name, &column.kind]).collect::<Vec<_>>()}),
    )
}

/// Finishes the run `run` with an error.
pub fn fail(run: &str, error: &str, debug: &Value) -> Cmd {
    Cmd::new(
        run,
        "run.result",
        &json!({"error": error, "outcome": "ERROR", "debug": debug}),
    )
}

/// Saves a finished run in History: `entry` has `checkId`, `sessionId`, `title`, `module`,
/// `started`, `ended` (ms), `status` (`success`, `error`, `stopped`), `text` and `defer`
/// (kept back like a repeat's earlier runs).
pub fn history_insert(tag: impl Into<String>, entry: &Value) -> Cmd {
    Cmd::new(tag, "history.insert", entry)
}

/// Lists History; `query` as the Host's History `list` takes it.
pub fn history_search(tag: impl Into<String>, query: &Value) -> Cmd {
    Cmd::new(tag, "history.search", query)
}

/// The environment of the user's login shell as a JSON object of strings.
pub fn env(tag: impl Into<String>) -> Cmd {
    Cmd::new(tag, "os.env", &json!({}))
}

pub fn clipboard_write(tag: impl Into<String>, text: &str) -> Cmd {
    Cmd::new(tag, "clipboard.write", &json!({"text": text}))
}

/// The raw document `name` (like `dictionary.json`) of every applied source, by order:
/// `[{source, text}]`, the local layer last.
pub fn sources_read(tag: impl Into<String>, name: &str) -> Cmd {
    Cmd::new(tag, "sources.read", &json!({"name": name}))
}

/// The saved workspaces (sessions): `{current, sessions: [{id, name}], saving, saveError,
/// importError}`; `0` is the temporary one, never saved. A workspace holds the documents,
/// the layout and the session keys of every module's kv; the Kernel saves the current one
/// as it changes.
pub fn workspace_list(tag: impl Into<String>) -> Cmd {
    Cmd::new(tag, "workspace.list", &json!({}))
}

/// Makes workspace `id` the current one; every module gets `Workspace`.
pub fn workspace_restore(tag: impl Into<String>, id: u64) -> Cmd {
    Cmd::new(tag, "workspace.restore", &json!({"id": id}))
}

/// Saves a copy of workspace `source` (`0`: an empty one) as `name` and makes it current.
pub fn workspace_copy(tag: impl Into<String>, source: u64, name: &str) -> Cmd {
    Cmd::new(
        tag,
        "workspace.copy",
        &json!({"source": source, "name": name}),
    )
}

/// Deletes saved workspace `id`; deleting the current one makes the temporary one current.
pub fn workspace_delete(tag: impl Into<String>, id: u64) -> Cmd {
    Cmd::new(tag, "workspace.delete", &json!({"id": id}))
}

/// Renames saved workspace `id`; the temporary one keeps its name.
pub fn workspace_rename(tag: impl Into<String>, id: u64, name: &str) -> Cmd {
    Cmd::new(tag, "workspace.rename", &json!({"id": id, "name": name}))
}

/// Retries what the App could not do at startup, such as importing the original workspace,
/// and reconnects to the Host; replies when it was asked for.
pub fn workspace_retry(tag: impl Into<String>) -> Cmd {
    Cmd::new(tag, "workspace.retry", &json!({}))
}

/// What the module adds to the editors: `{"variables": [[reference, value], …]}` (completion
/// and hover of `{{…}}`), `{"templates": {service: {section: text}}}` or
/// `{"hints": {service: text}}`. Replaces what the module contributed before under that key.
pub fn contribute(contribution: &Value) -> Cmd {
    Cmd::new("", "editor.contribute", contribution)
}

/// The console's current service and its document: `{service, text, line}`.
pub fn document_read(tag: impl Into<String>) -> Cmd {
    Cmd::new(tag, "document.read", &json!({}))
}

/// Inserts `text` at the cursor of the current console document.
pub fn document_insert(tag: impl Into<String>, text: &str) -> Cmd {
    Cmd::new(tag, "document.insert", &json!({"text": text}))
}

/// Commands only built-in modules (Settings) may send: they manage the modules, their
/// sources and the App.
pub const PRIVILEGED: &[&str] = &[
    "modules.check",
    "modules.install",
    "modules.remove",
    "sources.list",
    "sources.save",
    "sources.token",
    "sources.apply",
    "sources.open",
    "app.update",
    "app.reload",
];

/// Evaluates `expression` on the inspected page (DevTools only); the reply is its JSON value.
pub fn page_eval(tag: impl Into<String>, expression: &str) -> Cmd {
    Cmd::new(tag, "page.eval", &json!({"expression": expression}))
}

/// The requests DevTools recorded for the inspected page: `{entries: [{url, method, status,
/// time, bodySize, started}]}` (`started` in epoch milliseconds).
pub fn page_network(tag: impl Into<String>) -> Cmd {
    Cmd::new(tag, "page.network", &json!({}))
}

/// Starts an Agent turn: a stream of `{type: "copilot", event}` replies (the Copilot events)
/// that ends with the turn. `params` are `{workspaceId, sessionId, prompt, options}`. Console
/// tools are served by the App; page tools come to the module as `Event::ToolCall` with
/// `args: {turn, arguments}`, a console run waiting for approval as a reply
/// `{type: "approval", approval}` (`null` once decided).
pub fn agent_turn(tag: impl Into<String>, params: &Value) -> Cmd {
    Cmd::new(tag, "agent.turn", params)
}

/// Stops the Agent turn started with `turn`.
pub fn agent_stop(turn: &str) -> Cmd {
    Cmd::new(turn, "agent.stop", &json!({}))
}

/// The result of the Agent tool call `tag` (from `Event::ToolCall`).
pub fn agent_reply(tag: &str, result: &Value) -> Cmd {
    Cmd::new(tag, "agent.reply", &json!({"result": result}))
}

/// Approves or denies the console run waiting for approval in the turn `turn`.
pub fn agent_approve(turn: &str, approve: bool) -> Cmd {
    Cmd::new(turn, "agent.approve", &json!({"approve": approve}))
}

/// Deletes the Copilot history of Agent sessions `ids`.
pub fn agent_sessions_delete(tag: impl Into<String>, ids: &[String]) -> Cmd {
    Cmd::new(
        tag,
        "agent.sessions",
        &json!({"op": "delete", "sessionIds": ids}),
    )
}

/// The Copilot models: `{catalog, options}` — the catalog of the account and the options new
/// chats get.
pub fn agent_models(tag: impl Into<String>) -> Cmd {
    Cmd::new(tag, "agent.models", &json!({}))
}

/// Saves the options (model, reasoning) new Agent chats get; `None` only reads them. The
/// reply is the saved options.
pub fn agent_options(tag: impl Into<String>, options: Option<&Value>) -> Cmd {
    Cmd::new(tag, "agent.options", &json!({"options": options}))
}

/// The sources with what they offer and what is installed: `{sources, installed}`. `force`
/// checks again the sources checked recently.
pub fn modules_check(tag: impl Into<String>, force: bool) -> Cmd {
    Cmd::new(tag, "modules.check", &json!({"force": force}))
}

/// Installs (or updates to) `version` of `module` from source `source`; the Kernel reloads
/// the modules.
pub fn modules_install(tag: impl Into<String>, source: &str, module: &str, version: &str) -> Cmd {
    Cmd::new(
        tag,
        "modules.install",
        &json!({"source": source, "module": module, "version": version}),
    )
}

/// Removes `module`; `forget` deletes its documents in every session too.
pub fn modules_remove(tag: impl Into<String>, module: &str, forget: bool) -> Cmd {
    Cmd::new(
        tag,
        "modules.remove",
        &json!({"module": module, "forget": forget}),
    )
}

/// The configured sources: `{sources, tokenSet, warning}`.
pub fn sources_list(tag: impl Into<String>) -> Cmd {
    Cmd::new(tag, "sources.list", &json!({}))
}

/// Saves the list of sources (order, names, new and removed ones); replies like
/// [`sources_list`].
pub fn sources_save(tag: impl Into<String>, sources: &[Value]) -> Cmd {
    Cmd::new(tag, "sources.save", &json!({"sources": sources}))
}

/// Saves the GitHub token, or clears it with `None`.
pub fn sources_token(tag: impl Into<String>, token: Option<&str>) -> Cmd {
    Cmd::new(tag, "sources.token", &json!({"token": token}))
}

/// Applies the settings of source `source`; every module gets `Workspace`.
pub fn sources_apply(tag: impl Into<String>, source: &str) -> Cmd {
    Cmd::new(tag, "sources.apply", &json!({"source": source}))
}

/// Opens folder source `source` in the file manager.
pub fn sources_open(tag: impl Into<String>, source: &str) -> Cmd {
    Cmd::new(tag, "sources.open", &json!({"source": source}))
}

/// Installs App `version` from source `source`; the App saves the workspace, installs and
/// reloads. The reply comes once it is installed and could not reload by itself.
pub fn app_update(tag: impl Into<String>, source: &str, version: &str) -> Cmd {
    Cmd::new(
        tag,
        "app.update",
        &json!({"source": source, "version": version}),
    )
}

/// Asks the editors of the module's service for its highlighting and marks again.
pub fn relight() -> Cmd {
    Cmd::new("", "editor.relight", &json!({}))
}

/// Reloads the App after an installed update.
pub fn app_reload() -> Cmd {
    Cmd::new("", "app.reload", &json!({}))
}

pub fn kv_get(tag: impl Into<String>, key: &str) -> Cmd {
    Cmd::new(tag, "kv.get", &json!({"key": key}))
}

/// Stores a value under `key` in the module's own kv; `session` keys go into session snapshots.
pub fn kv_put(tag: impl Into<String>, key: &str, value: &Value, session: bool) -> Cmd {
    Cmd::new(
        tag,
        "kv.put",
        &json!({"key": key, "value": value, "session": session}),
    )
}

pub fn kv_delete(tag: impl Into<String>, key: &str) -> Cmd {
    Cmd::new(tag, "kv.delete", &json!({"key": key}))
}

/// `Event::Storage(key)` arrives when another place changes the key.
pub fn kv_watch(key: &str) -> Cmd {
    Cmd::new("", "kv.watch", &json!({"key": key}))
}

/// Replies once after `ms` milliseconds.
pub fn timer(tag: impl Into<String>, ms: u32) -> Cmd {
    Cmd::new(tag, "timer.after", &json!({"ms": ms}))
}

/// Replies when the lock is held; shared by every App window of the extension.
pub fn lock(tag: impl Into<String>, name: &str) -> Cmd {
    Cmd::new(tag, "lock.acquire", &json!({"name": name}))
}

pub fn unlock(name: &str) -> Cmd {
    Cmd::new("", "lock.release", &json!({"name": name}))
}

/// The module's own status line and indicator (`idle`, `busy`, `ok`, `warning`, `error`).
pub fn status(text: &str, indicator: &str) -> Cmd {
    Cmd::new(
        "",
        "status.set",
        &json!({"text": text, "indicator": indicator}),
    )
}

/// A [`status`] the user can stop: the status line's Stop button (`testid`, `console-<panel>-stop`
/// when empty) is enabled while the indicator is `busy` or `warning` and sends `stop` to the module's `update`.
pub fn status_stoppable(text: &str, indicator: &str, stop: &impl Serialize, testid: &str) -> Cmd {
    Cmd::new(
        "",
        "status.set",
        &json!({"text": text, "indicator": indicator, "stop": {"msg": stop, "testid": testid}}),
    )
}

/// A [`status_stoppable`] with structured details for its Debug; `disabled` keeps Stop disabled
/// whatever the indicator, e.g. while the module is already stopping.
pub fn status_stoppable_with(
    text: &str,
    indicator: &str,
    detail: &impl Serialize,
    stop: &impl Serialize,
    testid: &str,
    disabled: bool,
) -> Cmd {
    Cmd::new(
        "",
        "status.set",
        &json!({"text": text, "indicator": indicator, "detail": detail,
            "stop": {"msg": stop, "testid": testid, "disabled": disabled}}),
    )
}

/// A [`status`] with structured details for its Debug, e.g. what discovery found.
pub fn status_with(text: &str, indicator: &str, detail: &impl Serialize) -> Cmd {
    Cmd::new(
        "",
        "status.set",
        &json!({"text": text, "indicator": indicator, "detail": detail}),
    )
}

pub fn notify(text: &str, level: &str) -> Cmd {
    Cmd::new("", "notify", &json!({"text": text, "level": level}))
}

/// A line in the module's Debug log.
pub fn log(level: &str, text: &str) -> Cmd {
    Cmd::new("", "log", &json!({"level": level, "text": text}))
}

#[cfg(test)]
mod stoppable_tests {
    use super::*;

    #[test]
    fn a_stoppable_status_carries_its_stop() {
        let cmd = status_stoppable("Running", "busy", &"stop", "x-stop");
        let args = cmd.args();
        assert_eq!(args["stop"]["msg"], "stop");
        assert_eq!(args["stop"]["testid"], "x-stop");
        assert!(NAMES.contains(&"workspace.retry"));
        assert_eq!(workspace_retry("t").name, "workspace.retry");
    }
}
