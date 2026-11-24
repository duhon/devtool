pub mod action;
mod language;
mod render;

use action::{Defaults, Format, Plan, Source};
use kafka_contract::{
    Cursor, EndReason, FetchSummary, GroupOffset, Offset, Operation, Record, Request, Response,
    Start, StreamEvent,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use services_sdk::{
    Check, Cmd, Completions, Event, Failure, Line, LineColumns, Module, Rendered, Subscriptions,
    cmd, run, ui,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque, hash_map::RandomState},
    hash::BuildHasher,
};

const CACHE_MS: f64 = 60_000.0;
const MAX_JOBS: usize = 16;
const DRIVER_NOTICE: &str =
    "Kafka requires native driver kafka:1. This package does not install or simulate a driver.";

#[derive(Clone, Serialize, Deserialize)]
pub enum Msg {
    Format,
    Isolation,
    Timeout,
    MaxBytes,
    ScanLimit,
}

enum Purpose {
    Run(String),
    Discovery { raw: u64, force: bool },
}

struct Job {
    purpose: Purpose,
    plan: Option<Plan>,
    active: Option<Operation>,
    records: Vec<Record>,
    scanned: u64,
    bytes: usize,
    cursors: BTreeMap<(String, i32), Offset>,
    summary: Option<FetchSummary>,
    started: bool,
    lag: Vec<render::LagRow>,
    pending: VecDeque<Operation>,
}

struct Cached {
    raw: u64,
    resolved: u64,
    topics: Vec<String>,
    at: f64,
}

#[derive(Default)]
pub struct Kafka {
    defaults: Defaults,
    jobs: BTreeMap<u64, Job>,
    serial: u64,
    hashes: RandomState,
    cache: Vec<Cached>,
    transforms: BTreeSet<String>,
    shaping: run::Shaping<Shape>,
}

/// What renders a run's reshaped data: its format, status, secrets and byte budget.
struct Shape {
    format: Format,
    status: String,
    secrets: Vec<String>,
    max_bytes: usize,
}

fn tag(id: u64, step: &str) -> String {
    format!("kafka:{id}:{step}")
}

fn owner(value: &str) -> Option<(u64, &str)> {
    let rest = value.strip_prefix("kafka:")?;
    let (id, step) = rest.split_once(':')?;
    Some((id.parse().ok()?, step))
}

fn redact(mut text: String, plan: Option<&Plan>) -> String {
    if let Some(plan) = plan {
        let mut secrets = plan.request.connection.secrets();
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        for secret in secrets {
            text = text.replace(secret, "[redacted]");
        }
    }
    text
}

fn redact_value(value: &mut Value, plan: &Plan) {
    match value {
        Value::String(text) => *text = redact(std::mem::take(text), Some(plan)),
        Value::Array(values) => {
            for value in values {
                redact_value(value, plan);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_value(value, plan);
            }
        }
        _ => {}
    }
}

fn redact_rendered(rendered: &mut Rendered, plan: Option<&Plan>) {
    if let Some(plan) = plan {
        if rendered.language == "json"
            && let Ok(mut value) = serde_json::from_str(&rendered.text)
        {
            redact_value(&mut value, plan);
            rendered.text = serde_json::to_string_pretty(&value).unwrap();
        } else {
            rendered.text = redact(std::mem::take(&mut rendered.text), Some(plan));
        }
    }
}

impl Kafka {
    fn start(&mut self, purpose: Purpose, source: Source) -> Vec<Cmd> {
        if self.jobs.len() >= MAX_JOBS {
            return match purpose {
                Purpose::Run(run) => vec![cmd::fail(
                    &run,
                    "Too many active Kafka requests",
                    &Value::Null,
                )],
                _ => Vec::new(),
            };
        }
        let source = match source.prepare() {
            Ok(source) => source,
            Err(error) => {
                return match purpose {
                    Purpose::Run(run) => vec![cmd::fail(&run, &error, &Value::Null)],
                    _ => vec![cmd::notify(&error, "warning")],
                };
            }
        };
        self.serial += 1;
        let id = self.serial;
        self.jobs.insert(
            id,
            Job {
                purpose,
                plan: None,
                active: None,
                records: Vec::new(),
                scanned: 0,
                bytes: 0,
                cursors: BTreeMap::new(),
                summary: None,
                started: false,
                lag: Vec::new(),
                pending: VecDeque::new(),
            },
        );
        vec![
            run::resolve_value(tag(id, "resolve"), &serde_json::to_value(source).unwrap()),
            cmd::timer(tag(id, "deadline"), kafka_contract::MAX_TIMEOUT_MS as u32),
        ]
    }

    fn call(&mut self, id: u64, operation: Operation) -> Vec<Cmd> {
        let job = self.jobs.get_mut(&id).unwrap();
        let plan = job.plan.as_ref().unwrap();
        let request = Request {
            version: kafka_contract::VERSION,
            connection: plan.request.connection.clone(),
            operation: operation.clone(),
            limits: plan.request.limits.clone(),
        };
        if let Err(error) = request.validate() {
            return self.fail(id, error);
        }
        let mut call = services_sdk::DriverCall::new(kafka_contract::DRIVER, &request);
        if matches!(operation, Operation::Fetch { .. }) {
            call = call.streaming();
        }
        job.active = Some(operation);
        vec![cmd::driver(tag(id, "call"), &call)]
    }

    fn resolved(&mut self, id: u64, result: Result<Value, Failure>) -> Vec<Cmd> {
        let result = run::resolved_value(result)
            .and_then(|value| {
                serde_json::from_value::<action::PreparedSource>(value)
                    .map_err(|_| "Invalid resolved Kafka source".into())
            })
            .and_then(action::plan_prepared);
        let plan = match result {
            Ok(plan) => plan,
            Err(error) => return self.fail(id, error),
        };
        let identity = self
            .hashes
            .hash_one(serde_json::to_string(&plan.request.connection).unwrap());
        if let Purpose::Discovery { raw, force } = self.jobs[&id].purpose
            && !force
            && let Some(cached) = self
                .cache
                .iter_mut()
                .find(|c| c.resolved == identity && services_sdk::now() - c.at < CACHE_MS)
        {
            cached.raw = raw;
            self.jobs.remove(&id);
            return vec![cmd::relight()];
        }
        if let Purpose::Discovery { raw, .. } = self.jobs[&id].purpose {
            self.cache.retain(|cached| cached.raw != raw);
        }
        let operation = plan.request.operation.clone();
        let timeout = plan.request.limits.timeout_ms;
        self.jobs.get_mut(&id).unwrap().plan = Some(plan);
        let mut commands = vec![cmd::timer(tag(id, "deadline"), timeout as u32)];
        commands.extend(self.call(id, operation));
        commands
    }

    fn fail(&mut self, id: u64, error: String) -> Vec<Cmd> {
        let Some(job) = self.jobs.remove(&id) else {
            return Vec::new();
        };
        let error = redact(error, job.plan.as_ref());
        let mut commands = vec![cmd::cancel(&tag(id, "call"))];
        match &job.purpose {
            Purpose::Run(run) => {
                self.transforms.remove(run);
                let debug = json!({"driver": "kafka", "contractVersion": kafka_contract::VERSION,
                    "operation": job.active.as_ref().map(Operation::name), "error": error});
                let mut rendered = if !job.records.is_empty() {
                    let format = job.plan.as_ref().map_or(Format::Table, |p| p.format);
                    let mut rendered = render::records(&job.records, format, None);
                    if format == Format::Json {
                        rendered.text = json!({"records": job.records, "error": error,
                            "nextOffsets": Self::cursors(&job)})
                        .to_string();
                    } else {
                        rendered.text.push_str(&format!("\nError: {error}"));
                    }
                    rendered
                } else {
                    if job.lag.is_empty() {
                        Rendered {
                            text: error.clone(),
                            language: "text".into(),
                            ..Rendered::default()
                        }
                    } else {
                        let format = job.plan.as_ref().map_or(Format::Table, |p| p.format);
                        let mut rendered = render::lag(&job.lag, format);
                        if format == Format::Json {
                            rendered.text = json!({"rows": job.lag, "error": error}).to_string();
                        } else {
                            rendered.text.push_str(&format!("\nError: {error}"));
                        }
                        rendered
                    }
                };
                redact_rendered(&mut rendered, job.plan.as_ref());
                rendered.outcome = "ERROR".into();
                commands.push(cmd::result(run, &rendered, &debug));
            }
            Purpose::Discovery { .. } => {
                commands.push(cmd::notify(&format!("Kafka discovery: {error}"), "warning"))
            }
        }
        commands
    }

    fn cursors(job: &Job) -> Vec<Cursor> {
        job.cursors
            .iter()
            .map(|((topic, partition), offset)| Cursor {
                topic: topic.clone(),
                partition: *partition,
                offset: offset.clone(),
            })
            .collect()
    }

    fn finish_fetch(&mut self, id: u64, summary: FetchSummary) -> Vec<Cmd> {
        let Some(job) = self.jobs.get(&id) else {
            return Vec::new();
        };
        let plan = job.plan.as_ref().unwrap();
        let rendered = render::records(&job.records, plan.format, Some(&summary));
        let data = render::record_data(&job.records);
        self.finish(id, rendered, json!({"summary": summary}), data)
    }

    /// `data` is what a `Transform` sees as `.`.
    fn finish(&mut self, id: u64, mut rendered: Rendered, details: Value, data: Value) -> Vec<Cmd> {
        let Some(job) = self.jobs.remove(&id) else {
            return Vec::new();
        };
        let plan = job.plan.as_ref().unwrap();
        redact_rendered(&mut rendered, Some(plan));
        let mut commands = vec![cmd::cancel(&tag(id, "call"))];
        if let Purpose::Run(run) = &job.purpose {
            let run = run.as_str();
            if self.transforms.remove(run) {
                let mut debug = json!({"driver": "kafka", "contractVersion": kafka_contract::VERSION,
                    "operation": plan.request.operation.name(), "details": details});
                redact_value(&mut debug, plan);
                let shape = Shape {
                    format: plan.format,
                    status: Self::status(&job, plan),
                    secrets: plan
                        .request
                        .connection
                        .secrets()
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    max_bytes: plan.request.limits.max_bytes,
                };
                commands.push(self.shaping.hand(run, &data, debug, shape));
            } else if rendered.text.len() > plan.request.limits.max_bytes {
                commands.push(cmd::fail(
                    run,
                    "Rendered result exceeds Max-Bytes; reduce LIMIT or choose a different format",
                    &json!({"driver": "kafka", "operation": plan.request.operation.name()}),
                ));
            } else {
                let mut debug = json!({"driver": "kafka", "contractVersion": kafka_contract::VERSION,
                    "operation": plan.request.operation.name(), "details": details});
                redact_value(&mut debug, plan);
                commands.push(cmd::run_status(run, &Self::status(&job, plan)));
                commands.push(cmd::result(run, &rendered, &debug));
            }
        }
        commands
    }

    fn status(job: &Job, plan: &Plan) -> String {
        if matches!(plan.request.operation, Operation::Fetch { .. }) {
            format!("{} scanned; {} shown", job.scanned, job.records.len())
        } else {
            plan.request.operation.name().into()
        }
    }

    fn stream(&mut self, id: u64, event: StreamEvent) -> Vec<Cmd> {
        let job = self.jobs.get_mut(&id).unwrap();
        let plan = job.plan.as_ref().unwrap();
        let Operation::Fetch {
            topic, partitions, ..
        } = &plan.request.operation
        else {
            return self.fail(id, "Unexpected stream event for a non-Fetch request".into());
        };
        match event {
            StreamEvent::Started { partitions: starts } => {
                if job.started || starts.is_empty() || starts.len() > 1024 {
                    return self.fail(id, "Invalid or duplicate Kafka Started event".into());
                }
                for cursor in starts {
                    if cursor.topic != *topic
                        || cursor.partition < 0
                        || matches!(&plan.request.operation,
                            Operation::Fetch { start: Start::Offset(offset), .. }
                                if *offset != cursor.offset)
                        || partitions
                            .as_ref()
                            .is_some_and(|p| !p.contains(&cursor.partition))
                        || job
                            .cursors
                            .insert((cursor.topic, cursor.partition), cursor.offset)
                            .is_some()
                    {
                        return self.fail(id, "Invalid starting partition cursor".into());
                    }
                }
                if partitions
                    .as_ref()
                    .is_some_and(|p| p.len() != job.cursors.len())
                {
                    return self.fail(id, "Driver did not start all requested partitions".into());
                }
                job.started = true;
                Vec::new()
            }
            StreamEvent::Records { records } => {
                if !job.started || job.summary.is_some() {
                    return self.fail(
                        id,
                        "Records must follow Started and precede the final summary".into(),
                    );
                }
                let mut shown = Vec::new();
                for record in records {
                    let key = (record.topic.clone(), record.partition);
                    let valid = job
                        .cursors
                        .get(&key)
                        .is_some_and(|next| record.offset.value() >= next.value());
                    if !valid {
                        return self.fail(
                            id,
                            "Unexpected partition or non-monotonic Kafka offset".into(),
                        );
                    }
                    let bytes = serde_json::to_vec(&record).unwrap().len();
                    if job.scanned >= u64::from(plan.request.limits.max_records)
                        || job.bytes.saturating_add(bytes) > plan.request.limits.max_bytes
                    {
                        return self.fail(id, "Driver exceeded Kafka record/byte limits".into());
                    }
                    let next = match record.offset.next() {
                        Ok(next) => next,
                        Err(error) => return self.fail(id, error),
                    };
                    job.scanned += 1;
                    job.bytes += bytes;
                    job.cursors.insert(key, next);
                    if plan
                        .filter
                        .as_ref()
                        .is_none_or(|filter| filter.matches(&record))
                    {
                        shown.push(record.clone());
                        job.records.push(record);
                    }
                    if job.records.len() >= plan.limit as usize {
                        let summary = FetchSummary {
                            scanned: job.scanned,
                            next_offsets: Self::cursors(job),
                            end_reason: EndReason::RecordLimit,
                        };
                        return self.finish_fetch(id, summary);
                    }
                }
                if let Purpose::Run(run) = &job.purpose {
                    if shown.is_empty() {
                        return Vec::new();
                    }
                    let text = if plan.format == Format::Json {
                        shown
                            .iter()
                            .map(|r| {
                                let mut value = serde_json::to_value(r).unwrap();
                                redact_value(&mut value, plan);
                                format!("{}\n", value)
                            })
                            .collect()
                    } else {
                        format!("{}\n", render::records(&shown, plan.format, None).text)
                    };
                    let text = redact(text, Some(plan));
                    vec![
                        cmd::output(run, &text),
                        cmd::run_status(
                            run,
                            &format!("{} scanned; {} shown", job.scanned, job.records.len()),
                        ),
                    ]
                } else {
                    Vec::new()
                }
            }
        }
    }

    fn response(&mut self, id: u64, mut response: Response) -> Vec<Cmd> {
        let job = self.jobs.get_mut(&id).unwrap();
        let plan = job.plan.as_ref().unwrap();
        let active = job.active.as_ref().unwrap();
        if let Response::Error(error) = &response {
            if !job.records.is_empty() || matches!(job.purpose, Purpose::Discovery { .. }) {
                return self.fail(id, format!("{}: {}", error.code, error.message));
            }
            let rendered = render::response(&response, active, plan.format);
            let data = render::response_data(&response);
            return self.finish(id, rendered, Value::Null, data);
        }
        let expected = matches!(
            (active, &response),
            (Operation::DescribeCluster, Response::Cluster(_))
                | (
                    Operation::ListTopics | Operation::DescribeTopics { .. },
                    Response::Topics(_)
                )
                | (Operation::ListOffsets { .. }, Response::Offsets(_))
                | (
                    Operation::ListGroups | Operation::DescribeGroups { .. },
                    Response::Groups(_)
                )
                | (
                    Operation::ListGroupOffsets { .. },
                    Response::GroupOffsets(_)
                )
                | (Operation::DescribeConfigs { .. }, Response::Configs(_))
                | (Operation::Produce { .. }, Response::Produced(_))
                | (Operation::Fetch { .. }, Response::Fetched(_))
        );
        if !expected {
            return self.fail(id, "Kafka driver returned the wrong response type".into());
        }
        if let Response::Fetched(summary) = response {
            if !job.started
                || job.summary.is_some()
                || summary.scanned != job.scanned
                || summary.next_offsets.len() != job.cursors.len()
            {
                return self.fail(id, "Invalid Kafka final summary".into());
            }
            let mut seen = std::collections::BTreeSet::new();
            for cursor in &summary.next_offsets {
                let key = (cursor.topic.clone(), cursor.partition);
                if !seen.insert(key.clone())
                    || job
                        .cursors
                        .get(&key)
                        .is_none_or(|offset| cursor.offset.value() < offset.value())
                {
                    return self.fail(id, "Invalid Kafka resume cursor".into());
                }
            }
            job.summary = Some(summary);
            return Vec::new();
        }
        if let Response::Produced(delivery) = &response {
            let Operation::Produce { record } = active else {
                unreachable!()
            };
            if delivery.topic != record.topic
                || delivery.partition.is_some_and(|p| {
                    p < 0 || record.partition.is_some_and(|expected| p != expected)
                })
                || delivery.status == kafka_contract::DeliveryStatus::Confirmed
                    && (delivery.partition.is_none()
                        || delivery.offset.is_none()
                        || delivery.error.is_some())
            {
                return self.fail(id, "Invalid Kafka delivery confirmation".into());
            }
        }
        if plan.lag {
            match response {
                Response::GroupOffsets(offsets) => {
                    if offsets.truncated {
                        return self.fail(
                            id,
                            "Group offsets were truncated; LAG would be incomplete".into(),
                        );
                    }
                    return self.lag_start(id, offsets.items);
                }
                Response::Offsets(offsets) => {
                    let Operation::ListOffsets { topic, .. } = active else {
                        unreachable!()
                    };
                    for row in job.lag.iter_mut().filter(|r| r.topic == *topic) {
                        if let Some(end) = offsets
                            .items
                            .iter()
                            .find(|o| o.topic == row.topic && o.partition == row.partition)
                        {
                            row.end = end.offset.clone();
                            if let Some(error) = &end.error {
                                row.error = Some(error.message.clone());
                            }
                            if row.error.is_none() {
                                if let (Some(committed), Some(end)) = (&row.committed, &row.end) {
                                    let lag = end.value() - committed.value();
                                    if lag < 0 {
                                        row.error =
                                            Some("Committed offset exceeds end offset".into());
                                    } else {
                                        row.lag = Some(lag);
                                    }
                                } else {
                                    row.error = Some("Committed/end offset is unknown".into());
                                }
                            }
                        } else {
                            row.error = Some("Partition end offset was not returned".into());
                        }
                    }
                    return self.lag_next(id);
                }
                _ => {}
            }
        }
        if let Purpose::Discovery { raw, .. } = job.purpose {
            let Response::Topics(topics) = response else {
                return self.fail(id, "Discovery requires topic metadata".into());
            };
            if topics.truncated || topics.items.iter().any(|t| t.error.is_some()) {
                return self.fail(id, "Topic discovery returned partial errors".into());
            }
            let topics = topics
                .items
                .into_iter()
                .filter(|t| !t.internal)
                .map(|t| t.name)
                .collect();
            let resolved = self
                .hashes
                .hash_one(serde_json::to_string(&plan.request.connection).unwrap());
            self.cache.retain(|c| c.raw != raw);
            if self.cache.len() >= 16 {
                self.cache.remove(0);
            }
            self.cache.push(Cached {
                raw,
                resolved,
                topics,
                at: services_sdk::now(),
            });
            self.jobs.remove(&id);
            return vec![cmd::relight()];
        }
        let mut limited = false;
        let max = plan.limit as usize;
        macro_rules! truncate {
            ($values:expr) => {{
                if $values.len() > max {
                    $values.truncate(max);
                    limited = true;
                }
            }};
        }
        match &mut response {
            Response::Topics(v) if matches!(active, Operation::ListTopics) => {
                truncate!(v.items);
                v.truncated |= limited;
            }
            Response::Groups(v) => {
                truncate!(v.items);
                v.truncated |= limited;
            }
            Response::Offsets(v) => {
                truncate!(v.items);
                v.truncated |= limited;
            }
            Response::Configs(v) => {
                for entry in v.items.iter_mut().filter(|entry| entry.sensitive) {
                    entry.value = None;
                }
                truncate!(v.items);
                v.truncated |= limited;
            }
            _ => {}
        }
        let mut rendered = render::response(&response, active, plan.format);
        if limited {
            rendered.outcome = "WARNING".into();
        }
        let data = render::response_data(&response);
        self.finish(id, rendered, json!({"limited": limited}), data)
    }

    fn lag_start(&mut self, id: u64, offsets: Vec<GroupOffset>) -> Vec<Cmd> {
        let job = self.jobs.get_mut(&id).unwrap();
        let plan = job.plan.as_ref().unwrap();
        let Operation::ListGroupOffsets { group } = &plan.request.operation else {
            unreachable!()
        };
        if offsets.len() > plan.limit as usize {
            return self.fail(
                id,
                "Group has more partitions than LIMIT; increase LIMIT explicitly".into(),
            );
        }
        let mut topics: BTreeMap<String, Vec<i32>> = BTreeMap::new();
        let mut seen = std::collections::BTreeSet::new();
        for offset in offsets {
            if offset.group != *group
                || offset.partition < 0
                || !seen.insert((offset.topic.clone(), offset.partition))
            {
                return self.fail(id, "Invalid group offset response".into());
            }
            topics
                .entry(offset.topic.clone())
                .or_default()
                .push(offset.partition);
            job.lag.push(render::LagRow {
                group: offset.group,
                topic: offset.topic,
                partition: offset.partition,
                committed: offset.committed,
                end: None,
                lag: None,
                error: offset.error.map(|e| e.message),
            });
        }
        for (topic, partitions) in topics {
            job.pending.push_back(Operation::ListOffsets {
                topic,
                partitions: Some(partitions),
                at: Start::End,
                isolation: kafka_contract::Isolation::ReadUncommitted,
            });
        }
        self.lag_next(id)
    }

    fn lag_next(&mut self, id: u64) -> Vec<Cmd> {
        let job = self.jobs.get_mut(&id).unwrap();
        if let Some(operation) = job.pending.pop_front() {
            return self.call(id, operation);
        }
        let rendered = render::lag(&job.lag, job.plan.as_ref().unwrap().format);
        let data = json!(job.lag);
        self.finish(
            id,
            rendered,
            json!({"lagMeaning": "end offset minus committed offset; not a message count"}),
            data,
        )
    }

    fn reply(&mut self, id: u64, result: Result<Value, Failure>) -> Vec<Cmd> {
        let value = match result {
            Ok(value) => value,
            Err(failure) => {
                let message = if failure.message.contains("Unknown driver") {
                    format!("{}. {DRIVER_NOTICE}", failure.message)
                } else {
                    failure.message
                };
                return self.fail(id, message);
            }
        };
        let job = &self.jobs[&id];
        if job.active.is_none() {
            return self.fail(id, "Reply without an active Kafka operation".into());
        }
        if serde_json::to_vec(&value).unwrap().len()
            > job.plan.as_ref().unwrap().request.limits.max_bytes
        {
            return self.fail(id, "Kafka reply exceeds Max-Bytes".into());
        }
        if value.get("event").is_some() {
            match serde_json::from_value(value) {
                Ok(event) => self.stream(id, event),
                Err(_) => self.fail(id, "Malformed Kafka stream event".into()),
            }
        } else {
            match serde_json::from_value(value) {
                Ok(response) => self.response(id, response),
                Err(_) => self.fail(id, "Malformed Kafka response".into()),
            }
        }
    }

    fn discovery_source(&self, check: &Check) -> Option<Source> {
        let mut source = Source::new(check, &self.defaults);
        if source.target.trim().is_empty() {
            return None;
        }
        source.action = "TOPICS LIMIT 1000".into();
        source.metadata.clear();
        source.payload.clear();
        let pairs = action::pairs(&source.options).ok()?;
        source.options = pairs
            .into_iter()
            .filter(|(key, _)| !matches!(key.as_str(), "tombstone" | "payload-encoding"))
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<_>>()
            .join("\n");
        Some(source)
    }

    fn raw_identity(&self, source: &Source) -> u64 {
        self.hashes.hash_one(serde_json::to_string(source).unwrap())
    }

    fn discover(&mut self, check: &Check, force: bool) -> Vec<Cmd> {
        let Some(source) = self.discovery_source(check) else {
            return Vec::new();
        };
        let raw = self.raw_identity(&source);
        if self
            .jobs
            .values()
            .any(|j| matches!(j.purpose, Purpose::Discovery { raw: value, .. } if value == raw))
        {
            return Vec::new();
        }
        self.start(Purpose::Discovery { raw, force }, source)
    }

    fn topics(&self, check: &Check) -> Vec<String> {
        let Some(source) = self.discovery_source(check) else {
            return Vec::new();
        };
        let raw = self.raw_identity(&source);
        self.cache
            .iter()
            .find(|c| c.raw == raw && services_sdk::now() - c.at < CACHE_MS)
            .map(|c| c.topics.clone())
            .unwrap_or_default()
    }

    fn ui(&mut self, msg: Msg, value: Option<String>) -> Vec<Cmd> {
        let Some(value) = value else {
            return Vec::new();
        };
        let mut defaults = self.defaults.clone();
        let valid = match msg {
            Msg::Format => serde_json::from_value(json!(value))
                .map(|v| defaults.format = v)
                .is_ok(),
            Msg::Isolation => serde_json::from_value(json!(value))
                .map(|v| defaults.isolation = v)
                .is_ok(),
            Msg::Timeout => value.parse().map(|v| defaults.timeout_ms = v).is_ok(),
            Msg::MaxBytes => value.parse().map(|v| defaults.max_bytes = v).is_ok(),
            Msg::ScanLimit => value.parse().map(|v| defaults.scan_limit = v).is_ok(),
        };
        if !valid || defaults.validate().is_err() {
            return vec![cmd::notify(
                "Invalid Kafka default; positive bounded values are required",
                "warning",
            )];
        }
        self.defaults = defaults;
        self.cache.clear();
        vec![cmd::kv_put(
            "defaults:save",
            "defaults",
            &serde_json::to_value(&self.defaults).unwrap(),
            true,
        )]
    }
}

impl Module for Kafka {
    type Msg = Msg;

    fn update(&mut self, event: Event<Msg>) -> Vec<Cmd> {
        match event {
            Event::Init { .. } => vec![
                cmd::kv_get("defaults:load", "defaults"),
                cmd::contribute(&json!({"templates": {"kafka": {"action":
                        "@topics List topics\nTOPICS\n\n@read Recent orders\nREAD /orders FROM LAST 20 LIMIT 20\n\n@tail Watch orders\nTAIL /orders LIMIT 100\n\n@lag Inspect group lag\nLAG /orders-service"}}})),
            ],
            Event::Ui { msg, value } => self.ui(msg, value),
            Event::Reply { tag, result } if self.shaping.owns(&tag) => self
                .shaping
                .reply(&tag, result, |shape, data| {
                    let mut rendered = render::shaped(data, shape.format);
                    let mut secrets = shape.secrets;
                    secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
                    for secret in secrets {
                        rendered.text = rendered.text.replace(&secret, "[redacted]");
                    }
                    if rendered.text.len() > shape.max_bytes {
                        return Err("the transformed result exceeds Max-Bytes".into());
                    }
                    Ok(run::Answer {
                        rendered,
                        status: Some(shape.status),
                    })
                })
                .unwrap_or_default(),
            Event::Reply { tag: value, result } if value == "defaults:load" => {
                if let Ok(value) = result
                    && let Ok(defaults) = serde_json::from_value::<Defaults>(value)
                    && defaults.validate().is_ok()
                {
                    self.defaults = defaults;
                }
                Vec::new()
            }
            Event::Run {
                tag,
                check,
                transform,
            } => {
                if self
                    .jobs
                    .values()
                    .any(|j| matches!(&j.purpose, Purpose::Run(run) if run == &tag))
                {
                    return vec![cmd::notify("Kafka run is already active", "warning")];
                }
                if transform {
                    self.transforms.insert(tag.clone());
                } else {
                    self.transforms.remove(&tag);
                }
                self.start(Purpose::Run(tag), Source::new(&check, &self.defaults))
            }
            Event::Cancel { tag: run } => {
                self.transforms.remove(&run);
                self.shaping.cancel(&run);
                let ids: Vec<_> = self
                    .jobs
                    .iter()
                    .filter_map(|(id, job)| {
                        matches!(&job.purpose, Purpose::Run(value) if value == &run).then_some(*id)
                    })
                    .collect();
                let mut commands = Vec::new();
                for id in ids {
                    let job = self.jobs.remove(&id).unwrap();
                    commands.push(cmd::cancel(&tag(id, "call")));
                    if matches!(job.active, Some(Operation::Produce { .. })) {
                        commands.push(cmd::output(&run, "\nStopped waiting: delivery may continue. Do not resend automatically.\n"));
                    }
                }
                commands
            }
            Event::Reply { tag: value, result } => {
                let Some((id, step)) = owner(&value).filter(|(id, _)| self.jobs.contains_key(id))
                else {
                    return Vec::new();
                };
                match step {
                    "resolve" if self.jobs[&id].plan.is_none() => self.resolved(id, result),
                    "call" => self.reply(id, result),
                    "deadline" => self.fail(
                        id,
                        "Kafka request timed out; a queued produce may still be written".into(),
                    ),
                    _ => Vec::new(),
                }
            }
            Event::End { tag: value } => {
                let Some((id, "call")) = owner(&value).filter(|(id, _)| self.jobs.contains_key(id))
                else {
                    return Vec::new();
                };
                if let Some(summary) = self.jobs.get_mut(&id).unwrap().summary.take() {
                    self.finish_fetch(id, summary)
                } else {
                    self.fail(id, "Kafka stream ended without a final summary".into())
                }
            }
            Event::Discover(check) => self.discover(&check, true),
            Event::Hints { check, pos } => {
                let at = services_sdk::lang::check::At::new(&check.text, pos as usize);
                if at.name() == Some("action") && !at.in_response() && !at.in_placeholder() {
                    self.discover(&check, false)
                } else {
                    Vec::new()
                }
            }
            Event::Workspace => vec![cmd::kv_get("defaults:load", "defaults")],
            Event::Hidden => {
                let ids: Vec<_> = self
                    .jobs
                    .iter()
                    .filter_map(|(id, j)| {
                        matches!(j.purpose, Purpose::Discovery { .. }).then_some(*id)
                    })
                    .collect();
                ids.into_iter()
                    .map(|id| {
                        self.jobs.remove(&id);
                        cmd::cancel(&tag(id, "call"))
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            run: true,
            hints: true,
            discover: true,
            workspace: true,
            transform: true,
            ..Subscriptions::default()
        }
    }

    fn view(&self) -> Option<ui::Node> {
        Some(ui::column(vec![
            ui::text(DRIVER_NOTICE).variant("small"),
            ui::field("Default format", ui::select(
                ["table", "vertical", "json"].iter().map(|v| ui::Choice::new(*v, *v)).collect(),
                serde_json::to_value(self.defaults.format).unwrap().as_str().unwrap(), &Msg::Format)),
            ui::field("Default isolation", ui::select(
                ["read-committed", "read-uncommitted"].iter().map(|v| ui::Choice::new(*v, *v)).collect(),
                serde_json::to_value(self.defaults.isolation).unwrap().as_str().unwrap(), &Msg::Isolation)),
            ui::field("Timeout (ms)", ui::input(Some(self.defaults.timeout_ms.to_string()), "30000", &Msg::Timeout)),
            ui::field("Max bytes", ui::input(Some(self.defaults.max_bytes.to_string()), "1048576", &Msg::MaxBytes)),
            ui::field("Scan limit", ui::input(Some(self.defaults.scan_limit.to_string()), "1000", &Msg::ScanLimit)),
            ui::text("Connection/security settings belong in #options; keep secrets in variable references.").variant("small"),
        ]))
    }

    fn complete(&self, check: &Check, pos: u32) -> Completions {
        language::complete(check, pos, &self.topics(check))
    }

    fn highlight(&self, state: &[u8], lines: &[String], columns: &[LineColumns]) -> Vec<Line> {
        language::highlight(state, lines, columns)
    }

    fn schema(&self, check: &Check) -> Option<Value> {
        let topics = self.topics(check);
        (!topics.is_empty()).then(|| json!({"topics": topics}))
    }

    fn fold(&self, check: &Check) -> Vec<services_sdk::Fold> {
        services_sdk::lang::table::folds(&check.text, &check.columns)
    }
}

services_sdk::export!(Kafka);
