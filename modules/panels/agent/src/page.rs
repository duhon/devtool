//! The page tools of Agent turns: each runs the page agent script on the inspected page
//! (`page.eval`, `page.network`) step by step. Writing needs the user's permission per page;
//! a guide shows hints and waits for the user's Next, Back or Finish.

use serde_json::{Value, json};
use services_sdk::{Cmd, Failure, cmd};
use std::collections::{BTreeMap, BTreeSet};

const SCRIPT: &str = include_str!("../assets/page-agent.js");
/// Global keys that cache the injected page agent and its content version on the inspected page.
const AGENT_KEY: &str = "__servicesDevtoolsPageAgentFn";
const VERSION_KEY: &str = "__servicesDevtoolsPageAgentVersion";
/// Sentinel the fast path returns when the cached agent is missing or stale and must be re-injected.
const NEEDS_INJECTION: &str = "__servicesDevtoolsPageAgentReinject";
/// Content hash of the agent script, so an updated module re-injects instead of reusing a stale copy.
const SCRIPT_VERSION: u64 = fnv1a(SCRIPT.as_bytes());
const fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

pub const REVOKED: &str = "Page access revoked; Agent turn ended or stopped.";
/// Tags of the module's page commands start with this.
pub const TAG: &str = "page:";
const WRITES: &[&str] = &[
    "page_click",
    "page_fill",
    "page_select",
    "page_focus",
    "page_set_checked",
    "page_press_key",
];

pub fn is_tool(tool: &str) -> bool {
    matches!(
        tool,
        "page_snapshot"
            | "page_inspect"
            | "page_read"
            | "page_search"
            | "page_options"
            | "page_context"
            | "page_errors"
            | "page_network"
            | "page_wait"
            | "page_scroll"
            | "page_guide_show"
            | "page_guide_clear"
    ) || WRITES.contains(&tool)
}

/// The expression that runs the page agent with `args`, injecting it once per document.
pub fn expression(args: &Value, inject: bool) -> String {
    if inject {
        format!(
            "(function(a){{var g=globalThis;g.{AGENT_KEY}={SCRIPT};g.{VERSION_KEY}={SCRIPT_VERSION};return g.{AGENT_KEY}(a);}})({args})"
        )
    } else {
        format!(
            "(function(a){{var g=globalThis;return g.{VERSION_KEY}==={SCRIPT_VERSION}?g.{AGENT_KEY}(a):{{{NEEDS_INJECTION}:true}};}})({args})"
        )
    }
}

/// A page tool call of a turn: `tag` is the tool call to answer.
#[derive(Clone, Debug)]
pub struct Call {
    pub tag: String,
    pub turn: String,
    pub chat: String,
    pub tool: String,
    pub arguments: Value,
}

/// A page writing permission the user is asked for.
#[derive(Clone, Debug)]
pub struct Approval {
    pub chat: String,
    pub origin: String,
    pub url: String,
    pub title: String,
    page_id: Value,
}

#[derive(Clone, Debug)]
struct Guide {
    turn: String,
    id: String,
    origin: String,
    page_id: Value,
}

#[derive(Debug)]
enum Step {
    /// `page_guide_clear` removes the turn's guide.
    Clear,
    Metadata,
    Network {
        metadata: Value,
        args: Value,
    },
    /// The page is read again after Network, to see it did not change meanwhile.
    Recheck {
        metadata: Value,
        value: Value,
    },
    Wait {
        args: Value,
        started: f64,
        timeout: u64,
    },
    WaitDelay {
        args: Value,
        started: f64,
        timeout: u64,
    },
    Show {
        guide: Guide,
    },
    Status {
        guide: Guide,
        started: f64,
    },
    StatusDelay {
        guide: Guide,
        started: f64,
    },
    Cleanup {
        guide: Guide,
        result: Result<Value, String>,
    },
    Plain,
}

#[derive(Debug)]
struct Job {
    call: Call,
    step: Step,
    /// The arguments of the evaluation waiting for its answer, and whether it injected.
    args: Value,
    injected: bool,
    /// The operation that keeps others waiting (all but a guide clear).
    main: bool,
}

#[derive(Default)]
pub struct Page {
    /// Prefix of the ids this module makes (unique per App window).
    pub prefix: String,
    seq: u64,
    active: BTreeSet<String>,
    denied: BTreeSet<String>,
    guided: BTreeSet<String>,
    grant: Option<(String, Value)>,
    access: BTreeMap<String, String>,
    pending: Option<(Call, Approval)>,
    jobs: BTreeMap<String, Job>,
    guide: Option<Guide>,
    /// Guide evaluations of revoked turns still running: once each answers, its guide is
    /// cleaned up again, so an overlay shown after the revocation does not stay.
    stale: BTreeMap<String, String>,
}

fn fail(call: &Call, message: impl Into<String>) -> Vec<Cmd> {
    vec![cmd::agent_reply(
        &call.tag,
        &json!({"error": message.into()}),
    )]
}

impl Page {
    fn next(&mut self) -> String {
        self.seq += 1;
        format!("{}{}", TAG, self.seq)
    }
    fn id(&mut self) -> String {
        self.seq += 1;
        format!("{}-page-{}", self.prefix, self.seq)
    }
    fn busy(&self) -> bool {
        self.jobs.values().any(|job| job.main)
    }
    fn eval(&mut self, mut job: Job, args: Value) -> Vec<Cmd> {
        let tag = self.next();
        let command = cmd::page_eval(tag.clone(), &expression(&args, false));
        job.args = args;
        job.injected = false;
        self.jobs.insert(tag, job);
        vec![command]
    }
    fn delay(&mut self, job: Job, ms: u32) -> Vec<Cmd> {
        let tag = self.next();
        self.jobs.insert(tag.clone(), job);
        vec![cmd::timer(tag, ms)]
    }
    /// An evaluation nobody waits for (cleanup).
    fn forget(&mut self, args: Value) -> Cmd {
        let tag = format!("{}-cleanup", self.next());
        cmd::page_eval(tag, &expression(&args, false))
    }

    pub fn start(&mut self, turn: &str) {
        self.active.insert(turn.into());
    }

    /// The turn ended or stopped: its access goes and its waiting call is answered.
    pub fn revoke(&mut self, turn: &str) -> Vec<Cmd> {
        let mut cmds = Vec::new();
        self.active.remove(turn);
        self.denied.remove(turn);
        self.guided.remove(turn);
        if let Some(access) = self.access.remove(turn) {
            cmds.push(self.forget(json!({"tool":"access_cleanup","accessId":access})));
        }
        if self.guide.as_ref().is_some_and(|guide| guide.turn == turn) {
            let guide = self.guide.take().expect("guide");
            cmds.push(self.forget(json!({"tool":"guide_cleanup","guideId":guide.id})));
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|(call, _)| call.turn == turn)
        {
            let (call, _) = self.pending.take().expect("pending");
            cmds.extend(fail(&call, REVOKED));
        }
        let tags: Vec<String> = self
            .jobs
            .iter()
            .filter(|(_, job)| job.call.turn == turn)
            .map(|(tag, _)| tag.clone())
            .collect();
        for tag in tags {
            let job = self.jobs.remove(&tag).expect("job");
            match &job.step {
                Step::Show { guide } | Step::Status { guide, .. } => {
                    self.stale.insert(tag, guide.id.clone());
                }
                Step::StatusDelay { guide, .. } => {
                    let args = json!({"tool":"guide_cleanup","guideId":guide.id});
                    cmds.push(self.forget(args));
                }
                _ => {}
            }
            cmds.extend(fail(&job.call, REVOKED));
        }
        cmds
    }

    pub fn shutdown(&mut self) -> Vec<Cmd> {
        let turns: Vec<String> = self.active.iter().cloned().collect();
        turns.iter().flat_map(|turn| self.revoke(turn)).collect()
    }

    pub fn approval(&self, chat: &str) -> Option<&Approval> {
        self.pending
            .as_ref()
            .map(|(_, approval)| approval)
            .filter(|approval| approval.chat == chat)
    }

    /// The user allowed writing (or only the guide) or denied it; the call runs again.
    pub fn decide(&mut self, chat: &str, approve: bool, guide_only: bool) -> Vec<Cmd> {
        if self.approval(chat).is_none() {
            return Vec::new();
        }
        let (call, approval) = self.pending.take().expect("pending");
        if approve {
            if guide_only {
                self.guided.insert(call.turn.clone());
            } else {
                self.grant = Some((approval.origin, approval.page_id));
            }
        } else {
            self.denied.insert(call.turn.clone());
        }
        self.request(call)
    }

    pub fn request(&mut self, call: Call) -> Vec<Cmd> {
        if !self.active.contains(&call.turn) {
            return fail(&call, REVOKED);
        }
        if !is_tool(&call.tool) {
            return fail(&call, "Unknown page tool.");
        }
        if call.tool == "page_guide_clear" {
            return match self.guide.clone() {
                Some(guide) if guide.turn == call.turn => {
                    let job = Job {
                        call,
                        step: Step::Clear,
                        args: Value::Null,
                        injected: false,
                        main: false,
                    };
                    self.eval(job, json!({"tool":"guide_cleanup","guideId":guide.id}))
                }
                Some(_) => fail(&call, "Another Agent turn owns the active page guide."),
                None => vec![cmd::agent_reply(&call.tag, &json!({"cleared": false}))],
            };
        }
        if call.tool == "page_guide_show" {
            self.guided.insert(call.turn.clone());
        }
        if self.guided.contains(&call.turn) && WRITES.contains(&call.tool.as_str()) {
            return fail(
                &call,
                "This turn is in guide-only mode. The user must click, fill and select on the website themselves.",
            );
        }
        if self.busy() {
            return fail(
                &call,
                "Another page operation is in progress; wait before retrying.",
            );
        }
        let job = Job {
            call,
            step: Step::Metadata,
            args: Value::Null,
            injected: false,
            main: true,
        };
        self.eval(job, json!({"tool":"metadata"}))
    }

    /// The answer to one of the module's page commands (`tag` starts with [`TAG`]).
    pub fn reply(&mut self, tag: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        if let Some(guide) = self.stale.remove(tag) {
            return vec![self.forget(json!({"tool":"guide_cleanup","guideId":guide}))];
        }
        let Some(mut job) = self.jobs.remove(tag) else {
            return Vec::new();
        };
        let result = result.map_err(|failure| failure.message);
        // A stale page agent answers that it must be injected first.
        if let Ok(value) = &result
            && value[NEEDS_INJECTION] == true
            && !job.injected
            && !matches!(job.step, Step::WaitDelay { .. } | Step::StatusDelay { .. })
        {
            let tag = self.next();
            let command = cmd::page_eval(tag.clone(), &expression(&job.args, true));
            job.injected = true;
            self.jobs.insert(tag, job);
            return vec![command];
        }
        if !self.active.contains(&job.call.turn) {
            return fail(&job.call, REVOKED);
        }
        self.step(job, result)
    }

    fn step(&mut self, job: Job, result: Result<Value, String>) -> Vec<Cmd> {
        let Job { call, step, .. } = job;
        let job = |step| Job {
            call: call.clone(),
            step,
            args: Value::Null,
            injected: false,
            main: true,
        };
        match step {
            Step::Clear => match result {
                Ok(_) => {
                    self.guide = None;
                    vec![cmd::agent_reply(&call.tag, &json!({"cleared": true}))]
                }
                Err(error) => fail(&call, error),
            },
            Step::Metadata => match result {
                Ok(metadata) => self.perform(call, metadata),
                Err(error) => fail(&call, error),
            },
            Step::Network { metadata, args } => match result
                .and_then(|har| network(&args, &metadata, &har))
            {
                Ok(value) => self.eval(
                    job(Step::Recheck { metadata, value }),
                    json!({"tool":"metadata"}),
                ),
                Err(error) => fail(&call, error),
            },
            Step::Recheck { metadata, value } => match result {
                Ok(current)
                    if current["origin"] != metadata["origin"]
                        || current["pageId"] != metadata["pageId"] =>
                {
                    fail(
                        &call,
                        "The document changed while reading Network; inspect fresh state.",
                    )
                }
                Ok(_) => vec![cmd::agent_reply(&call.tag, &value)],
                Err(error) => fail(&call, error),
            },
            Step::Wait {
                args,
                started,
                timeout,
            } => match result {
                Ok(value) if value["matched"] == true => {
                    vec![cmd::agent_reply(&call.tag, &value)]
                }
                Ok(_) if services_sdk::now() - started >= timeout as f64 => fail(
                    &call,
                    format!(
                        "Page wait timed out after {timeout} ms; the requested state was not observed."
                    ),
                ),
                Ok(_) => self.delay(
                    job(Step::WaitDelay {
                        args,
                        started,
                        timeout,
                    }),
                    100,
                ),
                Err(error) => fail(&call, error),
            },
            Step::WaitDelay {
                args,
                started,
                timeout,
            } => self.eval(
                job(Step::Wait {
                    args: args.clone(),
                    started,
                    timeout,
                }),
                args,
            ),
            Step::Show { guide } => match result {
                Ok(shown) if shown["shown"] != true => self.cleanup(
                    job,
                    guide,
                    Err("The inspected page did not confirm the guide overlay.".into()),
                ),
                Ok(_) => {
                    let args = status(&guide);
                    self.eval(
                        job(Step::Status {
                            guide,
                            started: services_sdk::now(),
                        }),
                        args,
                    )
                }
                Err(error) => self.cleanup(job, guide, Err(error)),
            },
            Step::Status { guide, started } => match result {
                _ if services_sdk::now() - started > 300_000.0 => self.cleanup(
                    job,
                    guide,
                    Err("Page guide timed out after five minutes without a Next, Back or Finish decision.".into()),
                ),
                Ok(value) => match value["action"].as_str() {
                    Some("waiting") => self.delay(job(Step::StatusDelay { guide, started }), 250),
                    Some(
                        "next" | "back" | "finish" | "dismissed" | "target_lost"
                        | "disconnected" | "missing",
                    ) => self.cleanup(job, guide, Ok(value)),
                    _ => self.cleanup(job, guide, Err("Invalid page guide action reply.".into())),
                },
                Err(error) => self.cleanup(job, guide, Err(error)),
            },
            Step::StatusDelay { guide, started } => {
                let args = status(&guide);
                self.eval(job(Step::Status { guide, started }), args)
            }
            Step::Cleanup { guide, result: shown } => {
                if self
                    .guide
                    .as_ref()
                    .is_some_and(|current| current.id == guide.id)
                    && result.is_ok()
                {
                    self.guide = None;
                }
                let reply = match (shown, result) {
                    (Ok(value), Ok(_)) => Ok(value),
                    (Err(error), Ok(_)) => Err(error),
                    (Ok(_), Err(error)) => Err(format!("Page guide cleanup failed: {error}")),
                    (Err(error), Err(cleanup)) => {
                        Err(format!("{error}; page guide cleanup failed: {cleanup}"))
                    }
                };
                match reply {
                    Ok(value) => vec![cmd::agent_reply(&call.tag, &value)],
                    Err(error) => fail(&call, error),
                }
            }
            Step::Plain => match result {
                Ok(value) => vec![cmd::agent_reply(&call.tag, &value)],
                Err(error) => fail(&call, error),
            },
        }
    }

    fn cleanup(
        &mut self,
        job: impl Fn(Step) -> Job,
        guide: Guide,
        result: Result<Value, String>,
    ) -> Vec<Cmd> {
        let args = json!({"tool":"guide_cleanup","guideId":guide.id});
        self.eval(job(Step::Cleanup { guide, result }), args)
    }

    /// The page's metadata came: the call may need permission, then it runs.
    fn perform(&mut self, call: Call, metadata: Value) -> Vec<Cmd> {
        let Some(origin) = metadata["origin"].as_str().map(str::to_owned) else {
            return fail(&call, "Page origin is unavailable.");
        };
        let Ok(parsed) = url::Url::parse(&origin) else {
            return fail(&call, "Page origin is invalid.");
        };
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.origin().ascii_serialization() != origin
        {
            return fail(&call, "Page tools require an inspected HTTP(S) page.");
        }
        let writes = WRITES.contains(&call.tool.as_str());
        let identity = (origin.clone(), metadata["pageId"].clone());
        if self.grant.as_ref().is_some_and(|grant| grant != &identity) {
            self.grant = None;
        }
        if writes && call.arguments["pageId"] != metadata["pageId"] {
            return fail(
                &call,
                "The document changed. Read a fresh snapshot before acting.",
            );
        }
        if writes && self.denied.contains(&call.turn) {
            return fail(
                &call,
                "Page writing was denied for this Agent turn; reading remains available.",
            );
        }
        if writes && self.grant.as_ref() != Some(&identity) {
            if self.pending.is_some() {
                return fail(
                    &call,
                    "Page approval is already pending; wait before retrying.",
                );
            }
            let Some(url) = metadata["url"].as_str() else {
                return fail(&call, "Page URL is unavailable.");
            };
            let approval = Approval {
                chat: call.chat.clone(),
                origin: origin.clone(),
                url: url.into(),
                title: metadata["title"].as_str().unwrap_or("").into(),
                page_id: metadata["pageId"].clone(),
            };
            self.pending = Some((call, approval));
            return Vec::new();
        }
        let mut args = call.arguments.clone();
        let Some(object) = args.as_object_mut() else {
            return fail(&call, "Page arguments must be an object.");
        };
        object.insert("tool".into(), json!(call.tool));
        object.insert("origin".into(), json!(origin));
        if !self.access.contains_key(&call.turn) {
            let id = self.id();
            self.access.insert(call.turn.clone(), id);
        }
        args["accessId"] = json!(self.access[&call.turn]);
        let job = |step| Job {
            call: call.clone(),
            step,
            args: Value::Null,
            injected: false,
            main: true,
        };
        match call.tool.as_str() {
            "page_network" => {
                if args["pageId"] != metadata["pageId"] {
                    return fail(
                        &call,
                        "The document changed. Read a fresh snapshot before reading Network.",
                    );
                }
                let tag = self.next();
                self.jobs
                    .insert(tag.clone(), job(Step::Network { metadata, args }));
                vec![cmd::page_network(tag)]
            }
            "page_wait" => {
                let timeout = match args.get("timeoutMs") {
                    None => 5000,
                    Some(value) => match value.as_u64().filter(|value| *value <= 10_000) {
                        Some(timeout) => timeout,
                        None => {
                            return fail(&call, "timeoutMs must be an integer from 0 to 10000.");
                        }
                    },
                };
                let step = Step::Wait {
                    args: args.clone(),
                    started: services_sdk::now(),
                    timeout,
                };
                self.eval(job(step), args)
            }
            "page_guide_show" => {
                if self.guide.is_some() {
                    return fail(&call, "Another page guide is already active.");
                }
                let guide = Guide {
                    turn: call.turn.clone(),
                    id: self.id(),
                    origin,
                    page_id: args["pageId"].clone(),
                };
                args["guideId"] = json!(guide.id);
                self.guide = Some(guide.clone());
                self.eval(job(Step::Show { guide }), args)
            }
            _ => self.eval(job(Step::Plain), args),
        }
    }
}

fn status(guide: &Guide) -> Value {
    json!({"tool":"guide_status","origin":guide.origin,"pageId":guide.page_id,"guideId":guide.id})
}

/// The Network requests of the inspected document (`page.network` answers `{entries: [{url,
/// method, status, time, bodySize, started}]}`, `started` in milliseconds since 1970).
fn network(args: &Value, metadata: &Value, har: &Value) -> Result<Value, String> {
    let range = |name, default, maximum| match args.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|value| *value <= maximum)
            .ok_or_else(|| format!("Invalid Network {name}.")),
    };
    let offset = range("offset", 0, u32::MAX.into())? as usize;
    let limit = range("limit", 20, 100)? as usize;
    if limit == 0 {
        return Err("Network limit must be positive.".into());
    }
    let query = match args.get("query") {
        None => String::new(),
        Some(value) => value
            .as_str()
            .filter(|value| value.chars().count() <= 200)
            .ok_or("Network query must be a string of at most 200 characters.")?
            .to_lowercase(),
    };
    let entries = har["entries"]
        .as_array()
        .ok_or("DevTools returned an invalid Network log.")?;
    let time_origin = metadata["pageId"]
        .as_str()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .ok_or("The inspected document has no navigation time.")?;
    let mut items = Vec::new();
    let mut omitted = 0;
    for entry in entries {
        let started = entry["started"].as_f64().filter(|value| value.is_finite());
        if started.is_none_or(|value| value < time_origin.floor()) {
            omitted += 1;
            continue;
        }
        let Some(url) = entry["url"].as_str() else {
            return Err("DevTools returned a Network request without a URL.".into());
        };
        let url = url::Url::parse(url).map_err(|_| "DevTools returned an invalid request URL.")?;
        if !matches!(url.scheme(), "http" | "https") {
            omitted += 1;
            continue;
        }
        let sanitized = format!("{}{}", url.origin().ascii_serialization(), url.path());
        if !sanitized.to_lowercase().contains(&query) {
            continue;
        }
        let method = entry["method"]
            .as_str()
            .ok_or("DevTools returned a Network request without a method.")?;
        items.push(json!({
            "url": sanitized.chars().take(600).collect::<String>(),
            "urlTruncated": sanitized.chars().count() > 600,
            "method": method.chars().take(20).collect::<String>(),
            "methodTruncated": method.chars().count() > 20,
            "status": entry["status"].as_f64(),
            "durationMs": entry["time"].as_f64(),
            "bodySize": entry["bodySize"].as_f64(),
        }));
    }
    if offset > items.len() {
        return Err("Network offset is out of range.".into());
    }
    let mut result = json!({"pageId":metadata["pageId"],"origin":metadata["origin"],"untrusted":true,
        "offset":offset,"total":items.len(),"items":[],"nextOffset":null,"omittedRequests":omitted,
        "limitations":["Only requests recorded by DevTools since this document's navigation; earlier history may be absent.",
            "No headers, bodies, URL credentials, query strings or fragments. The Network log is live; entries may change.",
            "Earlier/unattributed navigation timestamps and non-HTTP(S) URLs are omitted and counted."]});
    for (index, item) in items.iter().enumerate().skip(offset).take(limit) {
        result["items"]
            .as_array_mut()
            .expect("items")
            .push(item.clone());
        result["nextOffset"] = if index + 1 < items.len() {
            json!(index + 1)
        } else {
            Value::Null
        };
        if result.to_string().len() > 6000 {
            result["items"].as_array_mut().expect("items").pop();
            result["nextOffset"] = json!(index);
            break;
        }
    }
    Ok(result)
}
