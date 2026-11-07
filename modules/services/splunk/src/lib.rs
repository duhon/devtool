//! Splunk: a search streamed from the export API through the Host `http` driver, paced per
//! server across every App window, its rows shown as a table.

pub mod export;
pub mod language;
pub mod logfmt;
mod search;

use services_sdk::{
    Check, Cmd, Completions, Event, Failure, Line, Module, Subscriptions, cmd, run,
};
use std::collections::BTreeMap;

pub use search::{Plan, normalize_search, retry_after};

/// How long a search may take, rate-limit waits included.
pub const TIMEOUT_SECONDS: u64 = 600;
pub const PREVIEW_WARNING: &str =
    "Preview results are provisional and may change as the search runs.";

#[derive(Debug, Default)]
pub struct Splunk {
    searches: BTreeMap<String, search::Search>,
}

impl Splunk {
    /// The run and the step a reply `tag` belongs to.
    fn owner(&self, tag: &str) -> Option<(String, String)> {
        self.searches.keys().find_map(|run| {
            let step = tag.strip_prefix(run.as_str())?.strip_prefix(':')?;
            Some((run.clone(), step.to_owned()))
        })
    }

    fn start(&mut self, run: String, check: &Check) -> Vec<Cmd> {
        if let Err(error) = search::check_markers(&check.text) {
            return vec![cmd::fail(&run, &error, &serde_json::Value::Null)];
        }
        let sections = search::sections(check);
        self.searches
            .insert(run.clone(), search::Search::new(services_sdk::now()));
        vec![run::resolve_value(format!("{run}:resolve"), &sections)]
    }

    fn step(
        &mut self,
        run: &str,
        step: &str,
        result: Option<Result<serde_json::Value, Failure>>,
    ) -> Vec<Cmd> {
        let Some(search) = self.searches.get_mut(run) else {
            return vec![];
        };
        let cmds = search.step(run, step, result);
        if search.finished() {
            self.searches.remove(run);
        }
        cmds
    }
}

impl Module for Splunk {
    type Msg = ();

    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Run { tag, check } => self.start(tag, &check),
            Event::Reply { tag, result } => match self.owner(&tag) {
                Some((run, step)) => self.step(&run, &step, Some(result)),
                None => vec![],
            },
            Event::End { tag } => match self.owner(&tag) {
                Some((run, step)) => self.step(&run, &step, None),
                None => vec![],
            },
            Event::Cancel { tag } => self
                .searches
                .remove(&tag)
                .map(|search| search.cancelled())
                .unwrap_or_default(),
            _ => vec![],
        }
    }

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            run: true,
            ..Subscriptions::default()
        }
    }

    fn highlight(&self, state: &[u8], lines: &[String]) -> Vec<Line> {
        language::highlight(state, lines)
    }

    fn fold(&self, check: &Check) -> Vec<services_sdk::Fold> {
        services_sdk::lang::table::folds(&check.text)
    }

    fn complete(&self, check: &Check, pos: u32) -> Completions {
        language::complete(check, pos as usize)
    }
}

services_sdk::export!(Splunk);
