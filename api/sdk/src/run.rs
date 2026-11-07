//! Runs that answer a check with one driver call: the call runs, its answer is rendered and
//! the run finishes. A run resolves its variables itself ([`resolve_value`]): the App does
//! not touch the driver calls of a run.

use crate::{Check, Cmd, DriverCall, Failure, Rendered, cmd};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const CALL: &str = ":call";

/// A rendered answer and the run's status text (like `3 rows`).
pub struct Answer {
    pub rendered: Rendered,
    pub status: Option<String>,
}

impl From<Rendered> for Answer {
    fn from(rendered: Rendered) -> Self {
        Self {
            rendered,
            status: None,
        }
    }
}

/// Asks the App to resolve the `{{…}}` variables in the strings of `value`; read the reply
/// with [`resolved_value`]. Each value is inserted as is, whatever characters it holds.
pub fn resolve_value(tag: impl Into<String>, value: &Value) -> Cmd {
    cmd::resolve_json(tag, &value.to_string())
}

pub fn resolved_value(result: Result<Value, Failure>) -> Result<Value, String> {
    let text = result.map_err(|failure| failure.message)?;
    let text = text
        .as_str()
        .ok_or("Variables resolved to something else")?;
    serde_json::from_str(text).map_err(|error| format!("Variables made the check invalid: {error}"))
}

/// Refuses an `#options` section with keys besides the console's own (Repeat,
/// Repeat-Until), for a service `name` that takes none.
pub fn no_options(check: &Check, name: &str) -> Result<(), String> {
    match check
        .text_of("options")
        .lines()
        .find(|line| !line.trim().is_empty())
    {
        Some(line) => {
            let option = line
                .split_once(':')
                .map_or(line, |(option, _)| option)
                .trim();
            Err(format!(
                "Unknown option {option:?}; {name} takes Repeat and Repeat-Until"
            ))
        }
        None => Ok(()),
    }
}

/// `check` with the texts of the sections `resolved` names replaced.
pub fn with_sections(check: &Check, resolved: &Value) -> Check {
    let mut check = check.clone();
    for section in &mut check.sections {
        if let Some(text) = resolved[section.name.as_str()].as_str() {
            section.text = text.to_owned();
        }
    }
    check
}

/// The one-call runs in flight, by run tag.
#[derive(Debug, Default)]
pub struct OneCall {
    calls: BTreeMap<String, DriverCall>,
}

impl OneCall {
    /// The run `run` makes `call`.
    pub fn call(&mut self, run: &str, call: DriverCall) -> Cmd {
        let command = cmd::driver(format!("{run}{CALL}"), &call);
        self.calls.insert(run.to_owned(), call);
        command
    }

    pub fn cancel(&mut self, run: &str) {
        self.calls.remove(run);
    }

    /// The end of a run after the reply `tag`; `None` when the tag is not a run's call.
    /// `render` turns the call and its answer into the response.
    pub fn reply(
        &mut self,
        tag: &str,
        result: Result<Value, Failure>,
        render: impl FnOnce(&DriverCall, &Value) -> Result<Answer, String>,
    ) -> Option<Vec<Cmd>> {
        let run = tag.strip_suffix(CALL)?;
        let call = self.calls.remove(run)?;
        let request = call.request();
        Some(match result {
            Ok(response) => {
                let debug = json!({"request": request, "response": response});
                match render(&call, &response) {
                    Ok(Answer { rendered, status }) => {
                        let mut cmds: Vec<Cmd> = status
                            .iter()
                            .map(|status| cmd::run_status(run, status))
                            .collect();
                        cmds.push(cmd::result(run, &rendered, &debug));
                        cmds
                    }
                    Err(error) => {
                        let error = format!("The response could not be shown: {error}");
                        vec![cmd::fail(run, &error, &debug)]
                    }
                }
            }
            Err(failure) => {
                let debug = json!({"request": request, "reply": failure.details()});
                vec![cmd::fail(run, &failure.message, &debug)]
            }
        })
    }
}
