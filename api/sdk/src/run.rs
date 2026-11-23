//! Runs that answer a check with one driver call: the call runs, its answer is rendered and
//! the run finishes. A run resolves its variables itself ([`resolve_value`]): the App does
//! not touch the driver calls of a run. A run with a `Transform` hands the data it is about
//! to render to the Kernel and renders what comes back ([`Shaping`]; [`OneCall`] does it
//! for its call's answer).

use crate::{Check, Cmd, DriverCall, Failure, Rendered, cmd};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const CALL: &str = ":call";
const DATA: &str = ":data";

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

/// The one-call runs in flight, by run tag. A run with a `Transform` ([`OneCall::transform`])
/// hands the call's answer to it, and renders what comes back.
#[derive(Debug, Default)]
pub struct OneCall {
    calls: BTreeMap<String, DriverCall>,
    transforms: BTreeSet<String>,
    shaping: Shaping<DriverCall>,
}

impl OneCall {
    /// The run `run` makes `call`.
    pub fn call(&mut self, run: &str, call: DriverCall) -> Cmd {
        let command = cmd::driver(format!("{run}{CALL}"), &call);
        self.calls.insert(run.to_owned(), call);
        command
    }

    /// Whether the run `run` has a `Transform` (`Event::Run { transform }`).
    pub fn transform(&mut self, run: &str, transform: bool) {
        if transform {
            self.transforms.insert(run.to_owned());
        } else {
            self.transforms.remove(run);
        }
    }

    pub fn cancel(&mut self, run: &str) {
        self.calls.remove(run);
        self.transforms.remove(run);
        self.shaping.cancel(run);
    }

    /// The end of a run after the reply `tag`; `None` when the tag is not a run's call or
    /// data. `render` turns the call and its answer (reshaped, with a `Transform`) into the
    /// response.
    pub fn reply(
        &mut self,
        tag: &str,
        result: Result<Value, Failure>,
        render: impl FnOnce(&DriverCall, &Value) -> Result<Answer, String>,
    ) -> Option<Vec<Cmd>> {
        if self.shaping.owns(tag) {
            return self
                .shaping
                .reply(tag, result, |call, data| render(&call, &data));
        }
        let run = tag.strip_suffix(CALL)?;
        let call = self.calls.remove(run)?;
        let request = call.request();
        Some(match result {
            Ok(response) => {
                let debug = json!({"request": request, "response": response});
                if self.transforms.remove(run) {
                    return Some(vec![self.shaping.hand(run, &response, debug, call)]);
                }
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
                self.transforms.remove(run);
                let debug = json!({"request": request, "reply": failure.details()});
                vec![cmd::fail(run, &failure.message, &debug)]
            }
        })
    }
}

/// Runs that hand their final data to their `Transform` (`Event::Run { transform: true }`;
/// the module subscribes `transform`): [`Shaping::hand`] sends the data the run is about to
/// render and keeps what renders it; the reply `{run}:data` brings the data back reshaped,
/// and [`Shaping::reply`] renders it. A run without a `Transform` renders as usual, with no
/// round trip.
#[derive(Debug)]
pub struct Shaping<T> {
    waiting: BTreeMap<String, (T, Value)>,
}

impl<T> Default for Shaping<T> {
    fn default() -> Self {
        Self {
            waiting: BTreeMap::new(),
        }
    }
}

impl<T> Shaping<T> {
    /// The run `run` hands `data`; `keep` renders it and `debug` is the run's debug info.
    pub fn hand(&mut self, run: &str, data: &Value, debug: Value, keep: T) -> Cmd {
        self.waiting.insert(run.to_owned(), (keep, debug));
        cmd::data(run, data)
    }

    pub fn cancel(&mut self, run: &str) {
        self.waiting.remove(run);
    }

    /// Whether `tag` is the reply with the data of a run waiting for it.
    pub fn owns(&self, tag: &str) -> bool {
        tag.strip_suffix(DATA)
            .is_some_and(|run| self.waiting.contains_key(run))
    }

    /// The end of a run after the reply `tag`; `None` when the tag is not a run's data.
    /// `render` turns what the run kept and the reshaped data into the response.
    pub fn reply(
        &mut self,
        tag: &str,
        result: Result<Value, Failure>,
        render: impl FnOnce(T, Value) -> Result<Answer, String>,
    ) -> Option<Vec<Cmd>> {
        let run = tag.strip_suffix(DATA)?;
        let (keep, debug) = self.waiting.remove(run)?;
        Some(
            match result.map_err(|failure| failure.message).and_then(|data| {
                render(keep, data)
                    .map_err(|error| format!("The response could not be shown: {error}"))
            }) {
                Ok(Answer { rendered, status }) => {
                    let mut cmds: Vec<Cmd> = status
                        .iter()
                        .map(|status| cmd::run_status(run, status))
                        .collect();
                    cmds.push(cmd::result(run, &rendered, &debug));
                    cmds
                }
                Err(error) => vec![cmd::fail(run, &error, &debug)],
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_one_call_run_with_a_transform_renders_the_reshaped_answer() {
        let render = |_: &DriverCall, answer: &Value| {
            Ok(Answer::from(Rendered {
                text: answer.to_string(),
                ..Rendered::default()
            }))
        };
        let mut calls = OneCall::default();
        calls.call("a", DriverCall::default());
        let plain = calls.reply("a:call", Ok(json!(1)), render).unwrap();
        assert_eq!(
            (plain[0].name.as_str(), plain[0].args()["body"].clone()),
            ("run.result", json!("1"))
        );

        calls.call("x", DriverCall::default());
        calls.transform("x", true);
        let handed = calls.reply("x:call", Ok(json!(1)), render).unwrap();
        assert_eq!(
            (handed[0].name.as_str(), handed[0].args()),
            ("run.data", json!({"data": 1}))
        );
        let shown = calls.reply("x:data", Ok(json!(2)), render).unwrap();
        assert_eq!(shown[0].args()["body"], "2");
        assert_eq!(shown[0].args()["debug"]["response"], 1);
    }

    #[test]
    fn a_shaped_run_renders_the_data_that_comes_back() {
        let mut shaping = Shaping::default();
        let sent = shaping.hand("r", &json!([1, 2]), json!({"x": 1}), "one");
        assert_eq!(
            (sent.name.as_str(), sent.args()),
            ("run.data", json!({"data": [1, 2]}))
        );
        assert!(
            shaping
                .reply("r:call", Ok(json!(0)), |_, _| unreachable!())
                .is_none()
        );
        let render = |keep: &str, data: Value| {
            Ok(Answer {
                rendered: Rendered {
                    text: format!("{keep} {data}"),
                    ..Rendered::default()
                },
                status: Some("2 rows".into()),
            })
        };
        let cmds = shaping.reply("r:data", Ok(json!([2])), render).unwrap();
        assert_eq!(cmds[0].args()["text"], "2 rows");
        assert_eq!(cmds[1].args()["body"], "one [2]");
        assert_eq!(cmds[1].args()["debug"], json!({"x": 1}));
        assert!(shaping.reply("r:data", Ok(json!([2])), render).is_none());

        shaping.hand("r", &json!(1), Value::Null, "two");
        let cmds = shaping
            .reply("r:data", Ok(json!(1)), |_, _| Err("no rows".into()))
            .unwrap();
        assert_eq!(
            cmds[0].args()["error"],
            "The response could not be shown: no rows"
        );
        shaping.hand("r", &json!(1), Value::Null, "two");
        let failed = Err(Failure::new("Transform {{$x}}: boom"));
        let cmds = shaping.reply("r:data", failed, render).unwrap();
        assert_eq!(cmds[0].args()["error"], "Transform {{$x}}: boom");
    }
}
