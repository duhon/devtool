mod config;
mod discovery;
mod editor;
mod resp;

use config::{Connection, Plan};
use discovery::{Collector, Scope, TTL_MS};
use resp::{Decoder, Resp};
use serde_json::{Value, json};
use services_sdk::{
    Check, Cmd, Completions, Event, Failure, Line, LineColumns, Module, Rendered, Subscriptions,
    cmd, driver, run, tcp,
};
use std::collections::BTreeMap;

enum Purpose {
    Run(String),
    Discover {
        raw: String,
        scope: Scope,
        force: bool,
    },
}

enum Phase {
    Resolve,
    Connect,
    Auth,
    Select,
    Command,
}

struct Job {
    purpose: Purpose,
    phase: Phase,
    connection: Option<Connection>,
    args: Vec<String>,
    decoder: Decoder,
    collector: Option<Collector>,
    identity: String,
    output: String,
    subscribed: bool,
    acknowledgements: usize,
}

impl Job {
    fn debug(&self) -> Value {
        let connection = self.connection.as_ref();
        json!({
            "driver": "tcp", "protocol": "RESP2",
            "host": connection.map(|c| &c.request.host),
            "port": connection.map(|c| c.request.port),
            "tls": connection.map(|c| c.request.tls),
            "database": connection.map(|c| c.database),
            "command": self.args.first(),
        })
    }

    fn redact(&self, mut error: String) -> String {
        if let Some(password) = self
            .connection
            .as_ref()
            .and_then(|c| c.auth.as_ref())
            .and_then(|a| a.last())
            && !password.is_empty()
        {
            error = error.replace(password, "[redacted]");
        }
        error
    }
}

struct Cached {
    raw: String,
    identity: String,
    scope: Scope,
    snapshot: Value,
    created: f64,
}

#[derive(Default)]
pub struct Valkey {
    serial: u64,
    jobs: BTreeMap<u64, Job>,
    cache: Vec<Cached>,
    identities: BTreeMap<String, String>,
}

impl Valkey {
    fn start(&mut self, purpose: Purpose, plan: Plan) -> Vec<Cmd> {
        self.serial += 1;
        let id = self.serial;
        let job = Job {
            purpose,
            phase: Phase::Resolve,
            connection: None,
            args: Vec::new(),
            decoder: Decoder::default(),
            collector: None,
            identity: String::new(),
            output: String::new(),
            subscribed: false,
            acknowledgements: 0,
        };
        let plan = serde_json::to_value(plan).unwrap();
        self.jobs.insert(id, job);
        vec![
            run::resolve_value(tag(id, "resolve"), &plan),
            cmd::timer(tag(id, "deadline"), 110_000),
        ]
    }

    fn hints(&mut self, check: &Check, pos: u32, force: bool) -> Vec<Cmd> {
        let Some(scope) = (if force {
            Some(Scope::Keys(String::new()))
        } else {
            editor::scope(check, pos)
        }) else {
            return vec![];
        };
        let Ok(mut plan) = Plan::from_check(check, false) else {
            return vec![];
        };
        let raw = plan.identity();
        plan.args = vec![match &scope {
            Scope::Keys(prefix) => prefix.clone(),
            Scope::Hash(hash) => hash.clone(),
        }];
        if self.jobs.values().any(|job| matches!(&job.purpose, Purpose::Discover { raw: pending, scope: target, .. } if pending == &raw && target == &scope)) {
            return vec![];
        }
        if self
            .jobs
            .values()
            .filter(|job| matches!(job.purpose, Purpose::Discover { .. }))
            .count()
            >= 4
        {
            return vec![];
        }
        self.start(Purpose::Discover { raw, scope, force }, plan)
    }

    fn resolved(&mut self, id: u64, result: Result<Value, Failure>) -> Vec<Cmd> {
        let plan = run::resolved_value(result).and_then(config::resolved);
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => return self.fail(id, error),
        };
        let connection = match Connection::parse(&plan) {
            Ok(connection) => connection,
            Err(error) => return self.fail(id, error),
        };
        let identity = serde_json::to_string(&(plan.identity(), &plan.args)).unwrap();
        if let Some(Job {
            purpose: Purpose::Discover { raw, scope, force },
            ..
        }) = self.jobs.get(&id)
        {
            let lookup = scope_key(raw, scope);
            if self.identities.len() >= 32 && !self.identities.contains_key(&lookup) {
                self.identities.clear();
            }
            self.identities.insert(lookup, identity.clone());
            if !force
                && self.cache.iter().any(|cached| {
                    cached.identity == identity
                        && cached.scope == *scope
                        && services_sdk::now() - cached.created < TTL_MS
                })
            {
                // Associate a fresh raw variable expression with an already collected cache.
                if let Some(cached) = self
                    .cache
                    .iter_mut()
                    .find(|cached| cached.identity == identity && cached.scope == *scope)
                {
                    cached.raw = raw.clone();
                }
                self.jobs.remove(&id);
                return vec![cmd::relight()];
            }
        } else if let Err(error) = config::validate_command(&plan.args) {
            return self.fail(id, error);
        }
        let job = self.jobs.get_mut(&id).unwrap();
        job.identity = identity;
        if let Purpose::Discover { scope, .. } = &job.purpose {
            let value = plan.args.first().cloned().unwrap_or_default();
            let resolved_scope = match scope {
                Scope::Keys(_) => Scope::Keys(value),
                Scope::Hash(_) => Scope::Hash(value),
            };
            job.args = resolved_scope.first();
            job.collector = Some(Collector::new(resolved_scope));
        } else {
            job.args = plan.args;
        }
        let timeout = if job.collector.is_some() {
            connection.timeout_ms.min(10_000)
        } else {
            connection.timeout_ms
        };
        let call = connection.request.call();
        job.connection = Some(connection);
        job.phase = Phase::Connect;
        vec![
            cmd::driver(tag(id, "tcp"), &call),
            cmd::timer(tag(id, "timeout"), timeout),
        ]
    }

    fn connected(&mut self, id: u64) -> Vec<Cmd> {
        let job = self.jobs.get_mut(&id).unwrap();
        if !matches!(job.phase, Phase::Connect) {
            return vec![];
        }
        let connection = job.connection.as_ref().unwrap();
        let args = if let Some(auth) = &connection.auth {
            job.phase = Phase::Auth;
            auth.clone()
        } else if connection.database != 0 {
            job.phase = Phase::Select;
            vec!["SELECT".into(), connection.database.to_string()]
        } else {
            job.phase = Phase::Command;
            job.args.clone()
        };
        vec![cmd::write(&tag(id, "tcp"), &resp::encode(&args))]
    }

    fn frame(&mut self, id: u64, frame: Resp) -> Vec<Cmd> {
        let job = self.jobs.get_mut(&id).unwrap();
        match job.phase {
            Phase::Auth | Phase::Select => {
                if let Resp::Error(error) = frame {
                    return self.fail(id, error);
                }
                if frame.text() != Some("OK") {
                    return self.fail(id, "Expected OK during connection setup".into());
                }
                let connection = job.connection.as_ref().unwrap();
                let args = if matches!(job.phase, Phase::Auth) && connection.database != 0 {
                    job.phase = Phase::Select;
                    vec!["SELECT".into(), connection.database.to_string()]
                } else {
                    job.phase = Phase::Command;
                    job.args.clone()
                };
                vec![cmd::write(&tag(id, "tcp"), &resp::encode(&args))]
            }
            Phase::Command => {
                if let Some(collector) = &mut job.collector {
                    return match collector.accept(frame) {
                        Ok(Some(args)) => vec![cmd::write(&tag(id, "tcp"), &resp::encode(&args))],
                        Ok(None) => self.discovered(id),
                        Err(error) => self.fail(id, error),
                    };
                }
                if let Resp::Error(error) = frame {
                    return self.fail(id, error);
                }
                let subscription = job
                    .args
                    .first()
                    .is_some_and(|name| name.eq_ignore_ascii_case("SUBSCRIBE"));
                if subscription {
                    return self.subscription(id, frame);
                }
                let text = frame.display();
                self.finish(id, text)
            }
            _ => self.fail(id, "Unexpected response before a command was sent".into()),
        }
    }

    fn subscription(&mut self, id: u64, frame: Resp) -> Vec<Cmd> {
        let job = self.jobs.get_mut(&id).unwrap();
        let Some(items) = frame.array() else {
            return self.fail(id, "Invalid Pub/Sub response".into());
        };
        let event = match items.first().and_then(Resp::text) {
            Some("subscribe") if items.len() == 3 => {
                job.acknowledgements += 1;
                if job.acknowledgements < job.args.len() - 1 {
                    return vec![];
                }
                job.subscribed = true;
                json!({"type": "subscribed", "channels": &job.args[1..]})
            }
            Some("message") if items.len() == 3 && job.subscribed => {
                json!({"type": "message", "channel": items[1].json(), "payload": items[2].json()})
            }
            _ => return self.fail(id, "Unexpected Pub/Sub frame".into()),
        };
        let text = format!("{event}\n");
        if job.output.len() + text.len() > config::HISTORY_LIMIT {
            return self.fail(
                id,
                "Subscription output reached the 1 MiB history limit; start a new check".into(),
            );
        }
        job.output.push_str(&text);
        match &job.purpose {
            Purpose::Run(run) => vec![cmd::output(run, &text)],
            _ => vec![],
        }
    }

    fn discovered(&mut self, id: u64) -> Vec<Cmd> {
        let job = self.jobs.remove(&id).unwrap();
        let Purpose::Discover { raw, scope, .. } = job.purpose else {
            unreachable!()
        };
        self.cache.retain(|entry| {
            services_sdk::now() - entry.created < TTL_MS
                && !(entry.identity == job.identity && entry.scope == scope)
        });
        if self.cache.len() >= 16 {
            self.cache.remove(0);
        }
        let snapshot = job.collector.unwrap().snapshot();
        self.cache.push(Cached {
            raw,
            scope,
            identity: job.identity,
            snapshot,
            created: services_sdk::now(),
        });
        vec![
            cmd::cancel(&tag(id, "tcp")),
            cmd::relight(),
            cmd::status("Discovery · ready", "ok"),
        ]
    }

    fn finish(&mut self, id: u64, text: String) -> Vec<Cmd> {
        let job = self.jobs.remove(&id).unwrap();
        let debug = job.debug();
        let Purpose::Run(run) = job.purpose else {
            unreachable!()
        };
        vec![
            cmd::cancel(&tag(id, "tcp")),
            cmd::result(
                &run,
                &Rendered {
                    text,
                    language: "text".into(),
                    outcome: "OK".into(),
                    columns: vec![],
                },
                &debug,
            ),
        ]
    }

    fn fail(&mut self, id: u64, error: String) -> Vec<Cmd> {
        let Some(job) = self.jobs.remove(&id) else {
            return vec![];
        };
        let error = job.redact(error);
        let debug = job.debug();
        let mut cmds = vec![cmd::cancel(&tag(id, "tcp"))];
        match job.purpose {
            Purpose::Run(run) => {
                let mut failure = cmd::fail(&run, &error, &debug);
                if !job.output.is_empty() {
                    let mut args = failure.args();
                    args["body"] = json!(job.output);
                    failure.args = serde_json::to_vec(&args).unwrap();
                }
                cmds.push(failure);
            }
            Purpose::Discover { .. } => {
                cmds.push(cmd::status(&format!("Discovery · {error}"), "error"))
            }
        }
        cmds
    }

    fn reply(&mut self, id: u64, step: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        if !self.jobs.contains_key(&id) {
            return vec![];
        }
        if step == "resolve" {
            return self.resolved(id, result);
        }
        if matches!(step, "timeout" | "deadline") {
            if self.jobs[&id].subscribed {
                return vec![];
            }
            return self.fail(id, "Valkey request or subscription setup timed out".into());
        }
        if step != "tcp" {
            return vec![];
        }
        let value = match result {
            Ok(value) => value,
            Err(error) => return self.fail(id, error.message),
        };
        match tcp::Event::parse(&value) {
            Ok(tcp::Event::Connected { .. }) => self.connected(id),
            Ok(tcp::Event::Written { .. }) => vec![],
            Ok(tcp::Event::ChunkBase64(data)) => {
                let frames = driver::decode(&data)
                    .and_then(|bytes| self.jobs.get_mut(&id).unwrap().decoder.feed(&bytes));
                match frames {
                    Ok(frames) => {
                        let mut cmds = Vec::new();
                        for frame in frames {
                            if !self.jobs.contains_key(&id) {
                                break;
                            }
                            cmds.extend(self.frame(id, frame));
                        }
                        cmds
                    }
                    Err(error) => self.fail(id, error),
                }
            }
            _ => self.fail(
                id,
                "Valkey connection closed before the operation completed".into(),
            ),
        }
    }

    fn cached(&self, check: &Check, scope: Option<Scope>) -> Option<&Value> {
        let plan = Plan::from_check(check, false).ok()?;
        let raw = plan.identity();
        let valid = |entry: &&Cached| {
            entry.raw == raw
                && self.identities.get(&scope_key(&raw, &entry.scope)) == Some(&entry.identity)
                && services_sdk::now() - entry.created < TTL_MS
        };
        self.cache
            .iter()
            .rev()
            .filter(valid)
            .find(|entry| Some(&entry.scope) == scope.as_ref())
            .or_else(|| {
                self.cache.iter().rev().filter(valid).find(
                    |entry| matches!(entry.scope, Scope::Keys(ref prefix) if prefix.is_empty()),
                )
            })
            .map(|entry| &entry.snapshot)
    }
}

fn tag(id: u64, step: &str) -> String {
    format!("valkey:{id}:{step}")
}

fn scope_key(raw: &str, scope: &Scope) -> String {
    format!("{raw}:{scope:?}")
}

fn owner(tag: &str) -> Option<(u64, &str)> {
    let (id, step) = tag.strip_prefix("valkey:")?.split_once(':')?;
    Some((id.parse().ok()?, step))
}

impl Module for Valkey {
    type Msg = ();

    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Run { tag, check } => match Plan::from_check(&check, true) {
                Ok(plan) => self.start(Purpose::Run(tag), plan),
                Err(error) => vec![cmd::fail(&tag, &error, &Value::Null)],
            },
            Event::Cancel { tag: run } => {
                let ids: Vec<_> = self
                    .jobs
                    .iter()
                    .filter_map(|(id, job)| {
                        matches!(&job.purpose, Purpose::Run(tag) if *tag == run).then_some(*id)
                    })
                    .collect();
                ids.into_iter()
                    .map(|id| {
                        self.jobs.remove(&id);
                        cmd::cancel(&tag(id, "tcp"))
                    })
                    .collect()
            }
            Event::Reply { tag, result } => match owner(&tag) {
                Some((id, step)) => self.reply(id, step, result),
                None => vec![],
            },
            Event::End { tag } => match owner(&tag) {
                Some((id, "tcp")) => self.fail(id, "Valkey connection ended unexpectedly".into()),
                _ => vec![],
            },
            Event::Hints { check, pos } => self.hints(&check, pos, false),
            Event::Discover(check) => self.hints(&check, 0, true),
            Event::Workspace | Event::Hidden => {
                let ids: Vec<_> = self
                    .jobs
                    .iter()
                    .filter_map(|(id, job)| {
                        matches!(job.purpose, Purpose::Discover { .. }).then_some(*id)
                    })
                    .collect();
                let cmds = ids
                    .into_iter()
                    .map(|id| {
                        self.jobs.remove(&id);
                        cmd::cancel(&tag(id, "tcp"))
                    })
                    .collect();
                if matches!(event, Event::Workspace) {
                    self.cache.clear();
                    self.identities.clear();
                }
                cmds
            }
            _ => vec![],
        }
    }

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            run: true,
            hints: true,
            discover: true,
            workspace: true,
            ..Default::default()
        }
    }

    fn complete(&self, check: &Check, pos: u32) -> Completions {
        editor::complete(check, pos, self.cached(check, editor::scope(check, pos)))
    }

    fn highlight(&self, state: &[u8], lines: &[String], _: &[LineColumns]) -> Vec<Line> {
        editor::highlight(state, lines)
    }

    fn schema(&self, check: &Check) -> Option<Value> {
        self.cached(check, Some(Scope::Keys(String::new())))
            .cloned()
    }
}

services_sdk::export!(Valkey);
