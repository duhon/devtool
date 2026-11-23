//! The Kubernetes service: a `#target` (`k8s://<context>/<namespace>`), an `#action` (a
//! kubectl-style verb and resource), `#metadata` request parameters, app-side `#options` and a
//! YAML or JSON `#payload` for writes, run through the Host `kubernetes` driver. Discovery feeds
//! completion with contexts, namespaces, resources, object names, containers and labels.
//!
//! The module is split by owner so that its parts change independently. Language and run:
//! [`target`], [`action`] (the run and its reply tags), [`config`], [`payload`], [`render`].
//! Discovery and editor: [`discovery`] (the Hints/Document/Discover events, its cache and calls)
//! and [`editor`] (highlight, completion, marks, schema). This file only routes.
//!
//! Reply tags: the run `{run}:resolve` (variables of the sections) and `{run}:call` (driver calls
//! of the run, numbered `{run}:call:<n>` when a run makes several) belong to [`action`]; the tags
//! `discover:<n>` (discovery driver calls) and `resolve:<n>` (variables of a hints request, see
//! `Resolver`) belong to [`discovery`]. Every other reply tag is a run's.

pub mod action;
pub mod config;
pub mod discovery;
pub mod editor;
pub mod payload;
pub mod render;
pub mod resources;
pub mod target;

use action::Runs;
use discovery::Discovery;
use serde_json::Value;
use services_sdk::{Check, Cmd, Completions, Event, Line, LineColumns, Module, Subscriptions};

/// The sections of a check, in the order they are offered.
pub const SECTIONS: [&str; 5] = ["target", "action", "metadata", "options", "payload"];

/// Replies with this prefix belong to discovery: its driver calls and its variable resolution.
pub const DISCOVER: &str = "discover:";
pub const RESOLVE: &str = "resolve:";

#[derive(Default)]
pub struct Kubernetes {
    runs: Runs,
    discovery: Discovery,
}

fn discovering(tag: &str) -> bool {
    tag.starts_with(DISCOVER) || tag.starts_with(RESOLVE)
}

impl Module for Kubernetes {
    type Msg = ();

    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Hints { check, pos } => self.discovery.hints(&check, Some(pos as usize)),
            Event::Document(check) => self.discovery.hints(&check, None),
            Event::Discover(check) => self.discovery.discover(&check),
            Event::Reply { tag, result } if discovering(&tag) => self.discovery.reply(&tag, result),
            Event::Reply { tag, result } => self.runs.reply(&tag, result),
            Event::End { tag } => self.runs.end(&tag),
            Event::Run {
                tag,
                check,
                transform,
            } => self.runs.run(tag, check, transform),
            Event::Cancel { tag } => self.runs.cancel(&tag),
            Event::Workspace => self.discovery.reset(),
            _ => vec![],
        }
    }

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            hints: true,
            document: true,
            discover: true,
            run: true,
            workspace: true,
            transform: true,
            ..Subscriptions::default()
        }
    }

    fn highlight(&self, state: &[u8], lines: &[String], columns: &[LineColumns]) -> Vec<Line> {
        editor::highlight(state, lines, columns)
    }

    fn complete(&self, check: &Check, pos: u32) -> Completions {
        editor::complete(check, pos as usize, &self.discovery.catalog(check))
    }

    fn marks(&self, check: &Check) -> Vec<Line> {
        editor::marks(check, &self.discovery.catalog(check))
    }

    fn schema(&self, check: &Check) -> Option<Value> {
        editor::schema(check, &self.discovery.catalog(check))
    }
}

services_sdk::export!(Kubernetes);
