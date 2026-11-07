//! Server reflection through the Host `grpc` driver: the services of a target, or the
//! descriptors of a service with every file it depends on, one question per call.

use crate::proto::Files;
use prost_reflect::DescriptorPool;
use serde_json::Value;
use services_sdk::grpc::reflection::{self, Answer, Query, UNIMPLEMENTED, VERSIONS};
use services_sdk::grpc::{Request, Response};
use services_sdk::{Cmd, Failure, cmd};
use std::collections::{BTreeSet, VecDeque};

/// A reflection question waits this long, as a call.
pub const TIMEOUT_MS: u64 = 5000;
const TAG: &str = "reflect:";

/// Where reflection asks: the raw target and metadata; the App resolves their variables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub target: String,
    pub metadata: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Goal {
    Services,
    Service(String),
}

/// What a finished reflection found.
#[derive(Debug)]
pub enum Found {
    Services(Vec<String>),
    Pool(DescriptorPool),
}

/// What a reply leads to.
#[derive(Debug)]
pub enum Step {
    Ask(Cmd),
    Done(Found),
}

/// A reflection in flight.
#[derive(Debug)]
pub struct Run {
    pub id: u64,
    pub server: Server,
    pub goal: Goal,
    /// Whether the status line follows it.
    pub loud: bool,
    version: usize,
    query: Query,
    files: Files,
    asked: BTreeSet<String>,
    queue: VecDeque<String>,
}

/// The run of a reply tag.
pub fn id(tag: &str) -> Option<u64> {
    tag.strip_prefix(TAG)?.parse().ok()
}

impl Run {
    /// A run and its first question.
    pub fn start(id: u64, server: Server, goal: Goal, loud: bool) -> (Self, Cmd) {
        let query = match &goal {
            Goal::Services => Query::ListServices,
            Goal::Service(service) => Query::FileContainingSymbol(service.clone()),
        };
        let run = Self {
            id,
            server,
            goal,
            loud,
            version: 0,
            query,
            files: Files::default(),
            asked: BTreeSet::new(),
            queue: VecDeque::new(),
        };
        let cmd = run.ask();
        (run, cmd)
    }

    fn ask(&self) -> Cmd {
        let request = Request {
            metadata: self.server.metadata.clone(),
            timeout_ms: Some(TIMEOUT_MS),
            ..reflection::request(&self.server.target, VERSIONS[self.version], &self.query)
        };
        cmd::driver(format!("{TAG}{}", self.id), &request.call())
    }

    /// The driver's answer: the next question, or what was found.
    pub fn reply(&mut self, result: Result<Value, Failure>) -> Result<Step, String> {
        let failed = |message: String| format!("Reflection failed: {message}");
        let response = Response::parse(&result.map_err(|failure| failed(failure.message))?)?;
        if response.status == UNIMPLEMENTED && self.version + 1 < VERSIONS.len() {
            self.version += 1;
            return Ok(Step::Ask(self.ask()));
        }
        if response.status != 0 {
            return Err(failed(format!(
                "{}: {}",
                response.status_name(),
                response.message
            )));
        }
        match (reflection::answer(&response)?, &self.goal) {
            (Answer::Services(services), Goal::Services) => Ok(Step::Done(Found::Services(
                services
                    .into_iter()
                    .filter(|service| !service.starts_with("grpc.reflection."))
                    .collect(),
            ))),
            (Answer::Files(files), Goal::Service(_)) => {
                for name in self.files.add(&files)? {
                    if self.asked.insert(name.clone()) {
                        self.queue.push_back(name);
                    }
                }
                match self.queue.pop_front() {
                    Some(name) => {
                        self.query = Query::FileByFilename(name);
                        Ok(Step::Ask(self.ask()))
                    }
                    None => self.files.pool().map(|pool| Step::Done(Found::Pool(pool))),
                }
            }
            (Answer::Services(_), _) => Err(failed("missing file descriptors".into())),
            (Answer::Files(_), _) => Err(failed("missing service list".into())),
        }
    }
}
