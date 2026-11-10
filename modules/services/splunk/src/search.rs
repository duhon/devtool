//! One search run: its variables resolved, the server's pacing lock taken, the export
//! streamed with 404 and 429 handling, the rows shown.

use crate::export::{Export, MAX_RESPONSE_BYTES};
use crate::{PREVIEW_WARNING, TIMEOUT_SECONDS};
use serde_json::{Value, json};
use services_sdk::http::{self, Request};
use services_sdk::lang::headers;
use services_sdk::lang::table::{self, Column, Format, Kind, Options};
use services_sdk::{Check, Cmd, Failure, Rendered, cmd, run};

/// The least time between two export calls to one server, from any window.
pub const INTERVAL_MS: f64 = 13_000.0;
/// The longest wait for response headers or the next piece of the stream.
pub const API_TIMEOUT_MS: f64 = 120_000.0;
/// The wait after a 429 without a usable Retry-After.
const RETRY_AFTER_MS: f64 = 60_000.0;

const LEGACY_SECTIONS: [&str; 6] = ["from", "to", "app", "count", "preview", "offset"];

/// Sections that used to hold options are refused instead of being sent as SPL.
pub fn check_markers(text: &str) -> Result<(), String> {
    for line in text.lines() {
        if let Some(marker) = line.trim().strip_prefix('#') {
            let marker = marker
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if LEGACY_SECTIONS.contains(&marker.as_str()) {
                return Err(format!(
                    "Unsupported Splunk section #{marker}; use #options."
                ));
            }
        }
    }
    Ok(())
}

/// The sections a search reads, to resolve their variables at once.
pub fn sections(check: &Check) -> Value {
    json!({
        "target": check.text_of("target").trim(),
        "headers": check.text_of("headers"),
        "options": check.text_of("options"),
        "payload": check.text_of("payload"),
    })
}

/// A search prefixed with `search` unless it starts with a command (`|`) or is one.
pub fn normalize_search(search: &str) -> String {
    let search = search.trim();
    let generating = search
        .get(..6)
        .is_some_and(|s| s.eq_ignore_ascii_case("search"))
        && search[6..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
    if search.starts_with('|') || generating {
        search.into()
    } else {
        format!("search {search}")
    }
}

/// A Retry-After header in milliseconds from `now`: seconds or an HTTP date.
pub fn retry_after(value: &str, now: f64) -> Option<f64> {
    let value = value.trim();
    value
        .parse::<f64>()
        .ok()
        .filter(|seconds| seconds.is_finite())
        .map(|seconds| seconds.max(0.0) * 1000.0)
        .or_else(|| {
            let date = httpdate::parse_http_date(value).ok()?;
            let at = date.duration_since(std::time::UNIX_EPOCH).ok()?;
            Some((at.as_secs_f64() * 1000.0 - now).max(0.0))
        })
}

fn unsigned(value: Option<&str>, label: &str, default: u64) -> Result<u64, String> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(default),
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| format!("Splunk {label} must be a non-negative integer")),
    }
}

fn preview(value: Option<&str>) -> Result<bool, String> {
    match value.map(str::trim) {
        None | Some("") => Ok(false),
        Some(value) if value.eq_ignore_ascii_case("true") => Ok(true),
        Some(value) if value.eq_ignore_ascii_case("false") => Ok(false),
        _ => Err("Splunk Preview must be true or false".into()),
    }
}

/// What a search sends and how it shows the rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub url: String,
    /// The pre-v2 export API, tried once when the server does not know v2.
    pub legacy: String,
    pub origin: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub limit: Option<u64>,
    pub offset: u64,
    pub preview: bool,
    pub columns: Options,
    pub format: Option<Format>,
}

impl Plan {
    /// The plan of resolved [`sections`].
    pub fn parse(sections: &Value) -> Result<Self, String> {
        let text = |name: &str| sections[name].as_str().unwrap_or("");
        let search = text("payload");
        if search.trim().is_empty() {
            return Err("Splunk request needs a #payload block".into());
        }
        let base = text("target").trim();
        if base.is_empty() {
            return Err("splunk request needs a #target block".into());
        }
        let mut options = std::collections::BTreeMap::new();
        for (name, value) in headers::pairs(text("options")) {
            let key = name.to_ascii_lowercase();
            options.insert(key, value);
        }
        let option = |key: &str| options.get(key).map(String::as_str);
        let count = unsigned(option("count"), "Count", 100)?;
        let offset = unsigned(option("offset"), "Offset", 0)?;
        let preview = preview(option("preview"))?;
        let columns = Options::parse(option("columns-type"), option("columns-size"))?;
        let format = option("format").map(Format::parse).transpose()?;
        let app = option("app").unwrap_or("").trim();
        let base = url::Url::parse(&format!("{}/", base.trim_end_matches('/')))
            .map_err(|_| format!("Invalid baseUrl: {base}"))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err("Splunk #target must use HTTP or HTTPS".into());
        }
        let path = if app.is_empty() {
            "services/search/v2/jobs/export".to_string()
        } else {
            format!(
                "servicesNS/-/{}/search/v2/jobs/export",
                url::form_urlencoded::byte_serialize(app.as_bytes())
                    .collect::<String>()
                    .replace('+', "%20")
            )
        };
        let legacy = base
            .join(&path.replace("search/v2/jobs/", "search/jobs/"))
            .map_err(|error| error.to_string())?;
        let url = base.join(&path).map_err(|error| error.to_string())?;
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("search", &normalize_search(search))
            .append_pair("output_mode", "json")
            .append_pair("preview", if preview { "true" } else { "false" })
            .append_pair("segmentation", "none");
        for (option, key) in [("from", "earliest_time"), ("to", "latest_time")] {
            if let Some(value) = options.get(option).filter(|value| !value.is_empty()) {
                form.append_pair(key, value);
            }
        }
        let mut headers = vec![(
            "Content-Type".to_owned(),
            "application/x-www-form-urlencoded".to_owned(),
        )];
        for (name, value) in headers::pairs(text("headers")) {
            headers.retain(|(known, _)| !known.eq_ignore_ascii_case(&name));
            headers.push((name, value));
        }
        Ok(Self {
            url: url.to_string(),
            legacy: legacy.to_string(),
            origin: base.origin().ascii_serialization(),
            headers,
            body: form.finish(),
            limit: (count != 0).then_some(count),
            offset,
            preview,
            columns,
            format,
        })
    }

    fn request(&self, url: &str, timeout: f64) -> Request {
        Request {
            headers: self.headers.clone(),
            body: Some(self.body.clone()),
            redirects: Some(0),
            timeout_ms: Some(timeout.max(1.0) as u64),
            ..Request::new("POST", url)
        }
    }

    /// The rows as the response shows them.
    pub fn render(&self, export: &Export) -> Result<Rendered, String> {
        let mut columns: Vec<Column> = export
            .columns
            .iter()
            .map(|name| Column::new(name, Kind::Text))
            .collect();
        self.columns.apply(&mut columns)?;
        let rows: Vec<Vec<Value>> = export
            .rows
            .iter()
            .map(|row| {
                columns
                    .iter()
                    .map(|column| row.get(&column.name).cloned().unwrap_or(Value::Null))
                    .collect()
            })
            .collect();
        let mut text = table::show(&columns, &rows, self.format, false);
        let preview = export.preview == Some(true);
        if preview {
            text = format!("{PREVIEW_WARNING}\n\n{text}");
        }
        Ok(Rendered {
            text,
            language: if self.format == Some(Format::Json) {
                "json".into()
            } else {
                "splunk".into()
            },
            outcome: if preview { "WARNING" } else { "OK" }.into(),
            // Splunk exports carry no field types; only `Columns-Type` declares them.
            columns: Vec::new(),
        })
    }
}

#[derive(Debug, PartialEq)]
enum Stage {
    Resolving,
    Locking,
    Pacing,
    Waiting,
    /// The export call `n` runs.
    Calling(u32),
    Done,
}

/// A search in flight.
#[derive(Debug)]
pub struct Search {
    stage: Stage,
    started: f64,
    deadline: f64,
    plan: Option<Plan>,
    url: String,
    calls: u32,
    /// The generation of the idle timer; older ones are stale.
    idle: u32,
    head: Option<http::Response>,
    export: Option<Export>,
    error_body: Vec<u8>,
    bytes: usize,
    debug: Value,
}

fn key(origin: &str) -> String {
    format!("next:{origin}")
}

impl Search {
    pub fn new(now: f64) -> Self {
        Self {
            stage: Stage::Resolving,
            started: now,
            deadline: now + TIMEOUT_SECONDS as f64 * 1000.0,
            plan: None,
            url: String::new(),
            calls: 0,
            idle: 0,
            head: None,
            export: None,
            error_body: vec![],
            bytes: 0,
            debug: json!({"connection": {"events": []}}),
        }
    }

    pub fn finished(&self) -> bool {
        self.stage == Stage::Done
    }

    fn plan(&self) -> &Plan {
        self.plan.as_ref().expect("a planned search")
    }

    fn event(&mut self, name: &str, data: Value) {
        let at = (services_sdk::now() - self.started).max(0.0) as u64;
        if let Some(events) = self.debug["connection"]["events"].as_array_mut() {
            events.push(json!({"event": name, "atMs": at, "data": data}));
        }
    }

    /// The step `step` of run `run` got its reply (`None`: a stream ended).
    pub fn step(
        &mut self,
        run: &str,
        step: &str,
        result: Option<Result<Value, Failure>>,
    ) -> Vec<Cmd> {
        let now = services_sdk::now();
        match (step, result) {
            ("resolve", Some(result)) if self.stage == Stage::Resolving => {
                match run::resolved_value(result).and_then(|sections| Plan::parse(&sections)) {
                    Ok(plan) => {
                        let lock = format!("splunk:{}", plan.origin);
                        self.url = plan.url.clone();
                        self.debug["request"] = json!({"method": "POST", "url": plan.url,
                            "headers": plan.headers.iter().map(|(n, v)| (n.clone(), json!(v))).collect::<serde_json::Map<_, _>>()});
                        self.debug["export"] = json!({"rowLimit": plan.limit, "offset": plan.offset,
                            "preview": plan.preview, "rateLimitedRetries": 0, "legacyFallbacks": 0});
                        self.plan = Some(plan);
                        self.stage = Stage::Locking;
                        vec![cmd::lock(format!("{run}:lock"), &lock)]
                    }
                    Err(error) => self.fail(run, &error),
                }
            }
            ("lock", Some(Ok(_))) if self.stage == Stage::Locking => {
                self.stage = Stage::Pacing;
                vec![cmd::kv_get(
                    format!("{run}:next"),
                    &key(&self.plan().origin),
                )]
            }
            ("next", Some(result)) if self.stage == Stage::Pacing => {
                let next = result.ok().and_then(|value| value.as_f64()).unwrap_or(0.0);
                self.wait(run, next - now)
            }
            ("wait", Some(Ok(_))) if self.stage == Stage::Waiting => self.send(run),
            (step, Some(Ok(_))) if step.starts_with("idle:") => {
                let current = format!("idle:{}", self.idle);
                if step != current || !matches!(self.stage, Stage::Calling(_)) {
                    return vec![];
                }
                if now >= self.deadline {
                    return self.fail(run, &timed_out());
                }
                let error = if self.head.is_none() {
                    "Splunk export timed out waiting for response headers"
                } else {
                    "Splunk export timed out waiting for stream data"
                };
                self.fail(run, error)
            }
            (step, result) if step.starts_with("call:") => {
                let n: u32 = step[5..].parse().unwrap_or(0);
                if self.stage != Stage::Calling(n) {
                    return vec![];
                }
                match result {
                    Some(Ok(event)) => self.streamed(run, event),
                    Some(Err(failure)) => self.fail(run, &failure.message),
                    None => self.ended(run),
                }
            }
            (_, Some(Err(failure))) => self.fail(run, &failure.message),
            _ => vec![],
        }
    }

    /// Waits `ms` for the server's pacing, then sends.
    fn wait(&mut self, run: &str, ms: f64) -> Vec<Cmd> {
        if ms <= 0.0 {
            return self.send(run);
        }
        if services_sdk::now() + ms >= self.deadline {
            return self.fail(run, &timed_out());
        }
        self.stage = Stage::Waiting;
        vec![cmd::timer(format!("{run}:wait"), ms.ceil() as u32)]
    }

    fn send(&mut self, run: &str) -> Vec<Cmd> {
        let now = services_sdk::now();
        let remaining = self.deadline - now;
        if remaining <= 0.0 {
            return self.fail(run, &timed_out());
        }
        self.calls += 1;
        self.stage = Stage::Calling(self.calls);
        self.head = None;
        self.error_body.clear();
        let plan = self.plan.as_ref().expect("a planned search");
        self.export = Some(Export::new(plan.limit, plan.offset, plan.preview));
        self.debug["request"]["url"] = json!(self.url);
        let call = plan.request(&self.url, remaining).call().streaming();
        // The next call waits even if this window goes away mid-search.
        let paced = cmd::kv_put("", &key(&plan.origin), &json!(now + INTERVAL_MS), false);
        let mut cmds = vec![
            paced,
            cmd::driver(format!("{run}:call:{}", self.calls), &call),
        ];
        cmds.push(self.idle_timer(run));
        cmds
    }

    fn idle_timer(&mut self, run: &str) -> Cmd {
        self.idle += 1;
        let ms = API_TIMEOUT_MS
            .min(self.deadline - services_sdk::now())
            .max(0.0);
        cmd::timer(format!("{run}:idle:{}", self.idle), ms.ceil() as u32)
    }

    /// Ends the call in flight and records when the server may be called next.
    fn close(&mut self, run: &str, after: f64) -> Vec<Cmd> {
        let Stage::Calling(n) = self.stage else {
            return vec![];
        };
        self.stage = Stage::Waiting;
        let next = services_sdk::now() + after;
        vec![
            cmd::cancel(&format!("{run}:call:{n}")),
            cmd::kv_put("", &key(&self.plan().origin), &json!(next), false),
        ]
    }

    fn streamed(&mut self, run: &str, event: Value) -> Vec<Cmd> {
        if let Some(head) = event.get("head") {
            return match http::Response::parse(head) {
                Ok(head) => self.headed(run, head),
                Err(error) => self.fail(run, &error),
            };
        }
        let Some(chunk) = event["chunk"].as_str() else {
            return vec![];
        };
        self.bytes += chunk.len();
        let mut cmds = vec![self.idle_timer(run)];
        let status = self.head.as_ref().map_or(0, |head| head.status);
        if !(200..300).contains(&status) {
            if self.error_body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                let error = format!(
                    "Splunk returned HTTP {} with an oversized error body",
                    self.status_line()
                );
                return self.fail(run, &error);
            }
            self.error_body.extend_from_slice(chunk.as_bytes());
            return cmds;
        }
        let export = self.export.as_mut().expect("an export");
        match export.push(chunk.as_bytes()) {
            Ok(true) => {
                cmds.extend(self.close(run, INTERVAL_MS));
                cmds.extend(self.done(run));
                cmds
            }
            Ok(false) => cmds,
            Err(error) => self.fail(run, &error),
        }
    }

    fn status_line(&self) -> String {
        let Some(head) = &self.head else {
            return String::new();
        };
        let text = if head.status_text.is_empty() {
            "<unknown status code>"
        } else {
            &head.status_text
        };
        format!("{} {text}", head.status)
    }

    fn headed(&mut self, run: &str, head: http::Response) -> Vec<Cmd> {
        let now = services_sdk::now();
        let status = head.status;
        let headers: serde_json::Map<String, Value> = head
            .headers
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), json!(value)))
            .collect();
        self.debug["response"] =
            json!({"status": status, "statusText": head.status_text, "headers": headers});
        self.debug["connection"]["timing"]["ttfbMs"] = json!((now - self.started) as u64);
        if let Some(remote) = &head.remote {
            self.debug["connection"]["remoteAddress"] = json!(remote.address);
            self.debug["connection"]["remotePort"] = json!(remote.port);
            self.debug["connection"]["remoteFamily"] = json!(remote.family);
        }
        if self.url.starts_with("https:") {
            self.debug["connection"]["tls"] = json!({"encrypted": true});
        }
        let data = json!({"httpVersion": head.http_version, "status": status, "url": self.url});
        self.event("responseHeaders", data);
        let retry = head.header("retry-after").map(str::to_owned);
        self.head = Some(head);
        if status == 404 && self.url != self.plan().legacy {
            let mut cmds = self.close(run, INTERVAL_MS);
            self.event(
                "responseBody",
                json!({"byteLength": 0, "closedEarly": true}),
            );
            self.debug["export"]["legacyFallbacks"] = json!(1);
            self.url = self.plan().legacy.clone();
            cmds.extend(self.wait(run, INTERVAL_MS));
            return cmds;
        }
        if status == 429 {
            let delay = retry
                .and_then(|value| retry_after(&value, now))
                .unwrap_or(RETRY_AFTER_MS)
                .max(INTERVAL_MS);
            let mut cmds = self.close(run, delay);
            self.event(
                "responseBody",
                json!({"byteLength": 0, "closedEarly": true}),
            );
            if delay >= self.deadline - now {
                cmds.extend(self.fail(
                    run,
                    "Splunk returned HTTP 429; Retry-After exceeds the remaining search timeout",
                ));
                return cmds;
            }
            let retries = self.debug["export"]["rateLimitedRetries"]
                .as_u64()
                .unwrap_or(0);
            self.debug["export"]["rateLimitedRetries"] = json!(retries + 1);
            cmds.extend(self.wait(run, delay));
            return cmds;
        }
        let mut cmds = vec![self.idle_timer(run)];
        if (300..400).contains(&status) {
            cmds.extend(self.fail(
                run,
                "Splunk redirected the API call. Use the direct API endpoint; automatic redirects are disabled to preserve the server's API limit.",
            ));
        }
        cmds
    }

    fn ended(&mut self, run: &str) -> Vec<Cmd> {
        let status = self.head.as_ref().map_or(0, |head| head.status);
        if !(200..300).contains(&status) {
            let body = std::mem::take(&mut self.error_body);
            let messages = serde_json::from_slice::<Value>(&body)
                .map(|value| summary(&value))
                .unwrap_or_default();
            let error = if messages.is_empty() {
                let text: String = String::from_utf8_lossy(&body).chars().take(200).collect();
                format!("Splunk returned HTTP {}: {text}", self.status_line())
            } else {
                messages
            };
            return self.fail(run, &error);
        }
        let finished = self.export.as_mut().expect("an export").finish();
        match finished {
            Ok(()) => {
                let mut cmds = self.paced();
                cmds.extend(self.done(run));
                cmds
            }
            Err(error) => self.fail(run, &error),
        }
    }

    /// After a call ended by itself: the server may be called again after the interval.
    fn paced(&mut self) -> Vec<Cmd> {
        if !matches!(self.stage, Stage::Calling(_)) {
            return vec![];
        }
        self.stage = Stage::Waiting;
        let next = services_sdk::now() + INTERVAL_MS;
        vec![cmd::kv_put(
            "",
            &key(&self.plan().origin),
            &json!(next),
            false,
        )]
    }

    fn export_debug(&mut self) {
        let Some(export) = &self.export else {
            return;
        };
        let plan = self.plan.as_ref().expect("a planned search");
        let complete = export.complete;
        let fields = json!({
            "bytesRead": self.bytes, "records": export.records,
            "resultCount": export.rows.len(),
            "previewRecords": export.previews,
            "ignoredPreviewRecords": if plan.preview { 0 } else { export.previews },
            "rowsSkipped": plan.offset - export.skip,
            "selectedPreview": export.preview == Some(true),
            "stoppedAfterCount": export.limited,
            "stoppedAfterPreview": export.preview_complete,
            "streamComplete": export.complete,
        });
        let messages = json!(export.messages);
        for (name, value) in fields.as_object().into_iter().flatten() {
            self.debug["export"][name] = value.clone();
        }
        self.debug["response"]["messages"] = messages;
        let bytes = self.bytes;
        self.event(
            "responseBody",
            json!({"byteLength": bytes, "closedEarly": !complete}),
        );
    }

    fn done(&mut self, run: &str) -> Vec<Cmd> {
        self.export_debug();
        let export = self.export.as_ref().expect("an export");
        let rendered = self.plan().render(export);
        let rows = export.rows.len();
        match rendered {
            Ok(rendered) => {
                self.stage = Stage::Done;
                let status = format!("{rows} {}", if rows == 1 { "row" } else { "rows" });
                vec![
                    cmd::run_status(run, &status),
                    cmd::result(run, &rendered, &self.debug),
                ]
            }
            Err(error) => self.fail(run, &error),
        }
    }

    fn fail(&mut self, run: &str, error: &str) -> Vec<Cmd> {
        if self.stage == Stage::Done {
            return vec![];
        }
        let mut cmds = self.paced();
        if self.export.is_some() && !self.debug["export"]["streamComplete"].is_boolean() {
            self.export_debug();
        }
        self.debug["error"] = json!({"name": "Error", "message": error});
        self.stage = Stage::Done;
        cmds.push(cmd::fail(run, error, &self.debug));
        cmds
    }

    /// Stop: a call in flight still counts for the server's pacing.
    pub fn cancelled(mut self) -> Vec<Cmd> {
        self.paced()
    }
}

fn timed_out() -> String {
    format!(
        "Splunk search timed out after {TIMEOUT_SECONDS} seconds, including API rate-limit waits"
    )
}

fn summary(value: &Value) -> String {
    value["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["text"].as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_is_not_capped_below_the_servers_required_cooldown() {
        assert_eq!(retry_after("300", 0.0), Some(300_000.0));
        assert_eq!(retry_after("0.5", 0.0), Some(500.0));
        assert_eq!(
            retry_after("Thu, 01 Jan 1970 00:00:10 GMT", 4_000.0),
            Some(6_000.0)
        );
        for value in ["invalid", "NaN", "inf"] {
            assert_eq!(retry_after(value, 0.0), None, "{value}");
        }
    }

    fn plan(options: &str) -> Result<Plan, String> {
        Plan::parse(&json!({"target": "https://splunk.fixture", "headers": "",
            "options": options, "payload": "index=main"}))
    }

    #[test]
    fn count_offset_and_preview_are_validated_with_compatible_defaults() {
        let default = plan("").unwrap();
        assert_eq!(
            (default.limit, default.offset, default.preview),
            (Some(100), 0, false)
        );
        assert_eq!(plan("Count: 0").unwrap().limit, None);
        assert_eq!(plan("Count: 30").unwrap().limit, Some(30));
        assert_eq!(plan("Offset: 30").unwrap().offset, 30);
        for value in ["true", " TRUE "] {
            assert!(plan(&format!("Preview: {value}")).unwrap().preview);
        }
        for bad in [
            "Count: -1",
            "Count: abc",
            "Count: 1.5",
            "Offset: -1",
            "Preview: yes",
        ] {
            assert!(plan(bad).is_err(), "{bad}");
        }
        assert_eq!(
            plan("Format: wide").unwrap_err(),
            "Format takes table, vertical or json, not \"wide\""
        );
    }

    #[test]
    fn options_become_the_export_form_and_app_path() {
        let plan = plan("From: -24h\nTo: -5m\nApp: my app\nPreview: true").unwrap();
        assert_eq!(
            plan.url,
            "https://splunk.fixture/servicesNS/-/my%20app/search/v2/jobs/export"
        );
        assert_eq!(
            plan.legacy,
            "https://splunk.fixture/servicesNS/-/my%20app/search/jobs/export"
        );
        assert_eq!(
            plan.body,
            "search=search+index%3Dmain&output_mode=json&preview=true&segmentation=none&earliest_time=-24h&latest_time=-5m"
        );
        assert!(Plan::parse(&json!({"target": "ftp://x", "payload": "a"})).is_err());
        assert_eq!(
            Plan::parse(&json!({"target": "https://x"})).unwrap_err(),
            "Splunk request needs a #payload block"
        );
    }

    #[test]
    fn searches_get_the_search_command_unless_they_start_with_one() {
        assert_eq!(normalize_search("index=main"), "search index=main");
        assert_eq!(normalize_search("| tstats count"), "| tstats count");
        assert_eq!(normalize_search("SEARCH x"), "SEARCH x");
        assert_eq!(normalize_search("searchable=1"), "search searchable=1");
    }

    #[test]
    fn removed_sections_are_refused() {
        for marker in LEGACY_SECTIONS {
            assert_eq!(
                check_markers(&format!("#target\nx\n  #{}  \n", marker.to_uppercase()))
                    .unwrap_err(),
                format!("Unsupported Splunk section #{marker}; use #options.")
            );
        }
        assert!(check_markers("#payload\nindex=main").is_ok());
    }
}
