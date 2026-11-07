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
    "vars.profiles",
    "clipboard.read",
    "clipboard.write",
    "sources.read",
    "document.read",
    "document.insert",
    "document.replace",
    "document.select",
    "document.run",
    "workspace.snapshot",
    "workspace.restore",
    "page.eval",
    "agent.turn",
    "agent.stop",
    "agent.tools",
    "modules.install",
    "modules.remove",
    "modules.update",
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

/// Interim until the Dictionary panel module: the profiles variables resolve from, as
/// `{order, profiles}` — the session `var`, the Dictionary of the sources with its local
/// layer, and `env`.
pub fn vars_profiles(tag: impl Into<String>) -> Cmd {
    Cmd::new(tag, "vars.profiles", &json!({}))
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
