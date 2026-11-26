//! Prometheus service module. The Kernel owns Stop, Repeat and secret-masked history.
pub mod chart;
pub mod diagnostics;
pub mod discovery;
pub mod language;
pub mod render;
pub mod request;
pub mod sequence;

use serde_json::Value;
use services_sdk::{Check, Cmd, Completions, Event, Line, Module, Subscriptions, cmd, run};
use std::collections::BTreeMap;

#[derive(Debug)]
enum Pending {
    Resolving { now_ms: f64 },
    Calling(Box<request::Plan>),
}

#[derive(Debug, Default)]
pub struct Prometheus {
    discovery: discovery::Discovery,
    pending: BTreeMap<String, Pending>,
    charts: BTreeMap<String, chart::State>,
}

impl Module for Prometheus {
    type Msg = ();

    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Run { tag, check, .. } => {
                let sections = match request::sections(&check) {
                    Ok(sections) => sections,
                    Err(error) => return vec![cmd::fail(&tag, &error, &Value::Null)],
                };
                self.pending.insert(
                    tag.clone(),
                    Pending::Resolving {
                        now_ms: services_sdk::now(),
                    },
                );
                vec![run::resolve_value(format!("{tag}:resolve"), &sections)]
            }
            Event::Reply { tag, result } => {
                if let Some(run_tag) = tag.strip_suffix(":resolve")
                    && matches!(self.pending.get(run_tag), Some(Pending::Resolving { .. }))
                {
                    let Some(Pending::Resolving { now_ms }) = self.pending.remove(run_tag) else {
                        unreachable!()
                    };
                    return match run::resolved_value(result)
                        .and_then(|value| request::Plan::parse(&value, now_ms))
                    {
                        Ok(plan) => {
                            if matches!(plan.format, request::Format::Chart) {
                                if !self.charts.contains_key(run_tag)
                                    && self.charts.len() >= 32
                                    && let Some(first) = self.charts.keys().next().cloned()
                                {
                                    self.charts.remove(&first);
                                }
                            } else {
                                self.charts.remove(run_tag);
                            }
                            let call = cmd::driver(format!("{run_tag}:call"), &plan.request.call());
                            self.pending
                                .insert(run_tag.to_owned(), Pending::Calling(Box::new(plan)));
                            vec![call]
                        }
                        Err(error) => vec![cmd::fail(run_tag, &error, &Value::Null)],
                    };
                }
                if let Some(run_tag) = tag.strip_suffix(":call")
                    && matches!(self.pending.get(run_tag), Some(Pending::Calling(_)))
                {
                    let Some(Pending::Calling(plan)) = self.pending.remove(run_tag) else {
                        unreachable!()
                    };
                    // Encoded request strings cannot be safely masked by raw-value history
                    // redaction. Do not persist the form, target or authorization in debug.
                    return match result {
                        Ok(value) => match if matches!(plan.format, request::Format::Chart) {
                            render::answer_with_chart(
                                &plan,
                                &value,
                                self.charts.entry(run_tag.to_owned()).or_default(),
                            )
                        } else {
                            render::answer(&plan, &value)
                        } {
                            Ok(answer) => {
                                let mut cmds = vec![];
                                if let Some(status) = answer.status {
                                    cmds.push(cmd::run_status(run_tag, &status));
                                }
                                cmds.push(cmd::result(run_tag, &answer.rendered, &Value::Null));
                                cmds
                            }
                            Err(error) => vec![cmd::fail(run_tag, &error, &Value::Null)],
                        },
                        Err(failure) => {
                            vec![cmd::fail(run_tag, &failure.message, &Value::Null)]
                        }
                    };
                }
                self.discovery.reply(&tag, result).unwrap_or_default()
            }
            Event::End { tag } => {
                if let Some(run_tag) = tag.strip_suffix(":call")
                    && matches!(self.pending.get(run_tag), Some(Pending::Calling(_)))
                {
                    self.pending.remove(run_tag);
                    return vec![cmd::fail(
                        run_tag,
                        "Prometheus request ended without a response",
                        &Value::Null,
                    )];
                }
                vec![]
            }
            Event::Cancel { tag } => {
                self.pending.remove(&tag);
                self.charts.remove(&tag);
                vec![]
            }
            Event::Workspace => {
                self.charts.clear();
                vec![]
            }
            Event::Document(check) => self.discovery.document(&check),
            Event::Hints { check, pos } => self.discovery.hints(&check, pos as usize),
            Event::Discover(check) => self.discovery.manual(&check),
            _ => vec![],
        }
    }

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            run: true,
            document: true,
            hints: true,
            discover: true,
            workspace: true,
            ..Subscriptions::default()
        }
    }

    fn highlight(
        &self,
        state: &[u8],
        lines: &[String],
        columns: &[services_sdk::LineColumns],
    ) -> Vec<Line> {
        language::highlight(state, lines, columns)
    }

    fn complete(&self, check: &Check, pos: u32) -> Completions {
        language::complete(check, pos as usize, &self.discovery)
    }

    fn fold(&self, check: &Check) -> Vec<services_sdk::Fold> {
        language::fold(check)
    }

    fn marks(&self, check: &Check) -> Vec<Line> {
        diagnostics::marks(check)
    }

    fn schema(&self, check: &Check) -> Option<Value> {
        self.discovery.schema_of(check)
    }
}

services_sdk::export!(Prometheus);
