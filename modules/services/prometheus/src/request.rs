//! Resolve strings before URL/form encoding; never rewrite the PromQL payload.
use crate::chart;
use serde_json::{Value, json};
use services_sdk::{Check, http::Request, lang::table::Options};
use std::collections::{BTreeMap, BTreeSet};

pub const TIMEOUT_MS: u64 = 120_000;

fn pairs(text: &str, section: &str) -> Result<Vec<(String, String)>, String> {
    let mut found = BTreeSet::new();
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| format!("Invalid #{section} line; expected Name: value"))?;
            let name = name.trim();
            if name.is_empty() || !found.insert(name.to_ascii_lowercase()) {
                return Err(format!("Empty or duplicate #{section} key: {name}"));
            }
            Ok((name.to_owned(), value.trim().to_owned()))
        })
        .collect()
}

pub fn sections(check: &Check) -> Result<Value, String> {
    Ok(json!({
        "target": check.text_of("target").trim(),
        "headers": pairs(check.http_metadata(), "metadata")?,
        "options": pairs(check.text_of("options"), "options")?,
        "payload": payload(check),
    }))
}

fn payload(check: &Check) -> String {
    let mut start = None;
    let mut offset = 0;
    let mut quote = None;
    for line in check.text.split_inclusive('\n') {
        let mut content = line;
        if quote.is_none()
            && let Some((name, rest)) = services_sdk::lang::marker(line.trim_end_matches('\n'))
        {
            if start.is_none() && name == "payload" {
                start = Some(if rest.is_empty() {
                    offset + line.len()
                } else {
                    offset + line.find(rest).unwrap()
                });
                content = rest;
            } else if let Some(start) = start
                && matches!(
                    name.as_str(),
                    "target"
                        | "metadata"
                        | "headers"
                        | "options"
                        | "payload"
                        | "response"
                        | "responce"
                        | "check"
                        | "request"
                )
            {
                return check.text[start..offset].to_owned();
            }
        }
        if start.is_some() {
            let mut chars = content.chars();
            while let Some(character) = chars.next() {
                match quote {
                    Some(delimiter) if character == '\\' && delimiter != '`' => {
                        chars.next();
                    }
                    Some(delimiter) if character == delimiter => quote = None,
                    Some(_) => {}
                    None if character == '#' => break,
                    None if matches!(character, '"' | '\'' | '`') => quote = Some(character),
                    None => {}
                }
            }
        }
        offset += line.len();
    }
    start.map_or_else(
        || check.text_of("payload").to_owned(),
        |start| check.text[start..].to_owned(),
    )
}

/// Prometheus duration syntax, or a number of seconds.
pub fn duration(value: &str) -> Result<f64, String> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<f64>() {
        if seconds.is_finite() && seconds > 0.0 {
            return Ok(seconds);
        }
        return Err("Duration must be positive and finite".into());
    }
    let mut rest = value;
    let mut seconds = 0.0;
    while !rest.is_empty() {
        let n = rest
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(rest.len());
        let amount = rest[..n]
            .parse::<f64>()
            .map_err(|_| format!("Invalid duration: {value}"))?;
        rest = &rest[n..];
        let (unit, scale) = [
            ("ms", 0.001),
            ("y", 31_536_000.0),
            ("w", 604_800.0),
            ("d", 86_400.0),
            ("h", 3_600.0),
            ("m", 60.0),
            ("s", 1.0),
        ]
        .into_iter()
        .find(|(unit, _)| rest.starts_with(unit))
        .ok_or_else(|| format!("Invalid duration unit: {value}"))?;
        rest = &rest[unit.len()..];
        seconds += amount * scale;
    }
    if seconds.is_finite() && seconds > 0.0 {
        Ok(seconds)
    } else {
        Err("Duration must be positive and finite".into())
    }
}

pub fn time(value: &str, now_ms: f64) -> Result<f64, String> {
    let value = value.trim();
    let seconds = if value == "now" {
        now_ms / 1000.0
    } else if let Some(offset) = value.strip_prefix("now-") {
        (now_ms - duration(offset)? * 1000.0) / 1000.0
    } else if let Some(offset) = value.strip_prefix("now+") {
        (now_ms + duration(offset)? * 1000.0) / 1000.0
    } else if let Ok(seconds) = value.parse::<f64>() {
        seconds
    } else {
        let date = chrono::DateTime::parse_from_rfc3339(value).map_err(|_| {
            format!("Invalid query time: {value}; use RFC3339, Unix seconds or now-1h")
        })?;
        date.timestamp() as f64 + date.timestamp_subsec_nanos() as f64 / 1e9
    };
    if seconds.is_finite() {
        Ok(seconds)
    } else {
        Err("Query time must be finite".into())
    }
}

#[derive(Clone, Debug)]
pub enum Format {
    Table,
    Json,
    Chart,
}

impl Format {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "table" => Ok(Self::Table),
            "json" => Ok(Self::Json),
            "chart" => Ok(Self::Chart),
            _ => Err("Format must be table, json or chart".into()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub request: Request,
    pub format: Format,
    pub columns: Options,
    pub chart: chart::Options,
    pub interval: [f64; 2],
    pub step: Option<f64>,
    pub chart_identity: String,
}

impl Plan {
    pub fn parse(resolved: &Value, now_ms: f64) -> Result<Self, String> {
        let text = |key: &str| {
            resolved[key]
                .as_str()
                .ok_or_else(|| format!("Missing resolved {key}"))
        };
        let payload = text("payload")?;
        if payload.trim().is_empty() {
            return Err("Prometheus needs a #payload block".into());
        }
        let mut base = url::Url::parse(text("target")?.trim())
            .map_err(|_| "Invalid Prometheus #target URL".to_owned())?;
        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            return Err("Prometheus #target must use HTTP or HTTPS".into());
        }
        if base.query().is_some() || base.fragment().is_some() {
            return Err("Prometheus #target must be a base URL without query or fragment".into());
        }
        let headers: Vec<(String, String)> = serde_json::from_value(resolved["headers"].clone())
            .map_err(|_| "Invalid resolved metadata".to_owned())?;
        let mut params = BTreeMap::new();
        let mut sent = vec![];
        let mut seen = BTreeSet::new();
        for (name, value) in headers {
            let key = name.to_ascii_lowercase();
            if !seen.insert(key.clone()) {
                return Err(format!("Duplicate metadata key: {name}"));
            }
            if let Some(key) = key.strip_prefix("query-") {
                if !["mode", "time", "start", "end", "step", "timeout"].contains(&key) {
                    return Err(format!("Unknown Prometheus parameter: {name}"));
                }
                params.insert(key.to_owned(), value);
            } else {
                if name.is_empty()
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                    || value.chars().any(|c| c.is_control() && c != '\t')
                {
                    return Err("Invalid HTTP header name or value".into());
                }
                if key == "content-type" {
                    if !value.eq_ignore_ascii_case("application/x-www-form-urlencoded") {
                        return Err("Content-Type must be application/x-www-form-urlencoded".into());
                    }
                } else {
                    sent.push((name, value));
                }
            }
        }
        let param = |key: &str| params.get(key).map(String::as_str);
        let mode = param("mode")
            .unwrap_or("instant")
            .trim()
            .to_ascii_lowercase();
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("query", payload);
        let (interval, step) = match mode.as_str() {
            "instant" => {
                for key in ["start", "end", "step"] {
                    if params.contains_key(key) {
                        return Err(format!("Query-{key} contradicts Query-Mode: instant"));
                    }
                }
                let time = time(param("time").unwrap_or("now"), now_ms)?;
                form.append_pair("time", &time.to_string());
                ([time, time], None)
            }
            "range" => {
                if params.contains_key("time") {
                    return Err("Query-Time contradicts Query-Mode: range".into());
                }
                let start = time(param("start").ok_or("Range needs Query-Start")?, now_ms)?;
                let end = time(param("end").unwrap_or("now"), now_ms)?;
                let step = duration(param("step").ok_or("Range needs Query-Step")?)
                    .map_err(|e| format!("Invalid Query-Step: {e}"))?;
                if start > end {
                    return Err("Query-Start must not be after Query-End".into());
                }
                form.append_pair("start", &start.to_string())
                    .append_pair("end", &end.to_string())
                    .append_pair("step", &step.to_string());
                ([start, end], Some(step))
            }
            _ => return Err("Query-Mode must be instant or range".into()),
        };
        if let Some(timeout) = param("timeout") {
            form.append_pair(
                "timeout",
                &duration(timeout)
                    .map_err(|e| format!("Invalid Query-Timeout: {e}"))?
                    .to_string(),
            );
        }
        let opts: Vec<(String, String)> = serde_json::from_value(resolved["options"].clone())
            .map_err(|_| "Invalid resolved options".to_owned())?;
        let mut options = BTreeMap::new();
        for (name, value) in opts {
            let key = name.to_ascii_lowercase();
            if ![
                "format",
                "columns-type",
                "columns-size",
                "repeat",
                "repeat-until",
                "chart-width",
                "chart-height",
                "chart-min",
                "chart-max",
            ]
            .contains(&key.as_str())
            {
                return Err(format!("Unknown Prometheus option: {name}"));
            }
            if options.insert(key, value).is_some() {
                return Err(format!("Duplicate option: {name}"));
            }
        }
        let option = |key: &str| options.get(key).map(String::as_str);
        let columns = Options::parse(option("columns-type"), option("columns-size"))?;
        let format = option("format")
            .map(Format::parse)
            .transpose()?
            .unwrap_or(Format::Table);
        if !matches!(format, Format::Chart) && options.keys().any(|key| key.starts_with("chart-")) {
            return Err("Chart-* options require Format: chart".into());
        }
        if matches!(format, Format::Chart) && !columns.is_empty() {
            return Err("Columns-* options require Format: table or json".into());
        }
        let chart = chart::Options::parse(
            option("chart-width"),
            option("chart-height"),
            option("chart-min"),
            option("chart-max"),
        )?;
        let chart_identity = json!({
            "target": resolved["target"],
            "headers": resolved["headers"],
            "options": resolved["options"],
            "payload": payload,
        })
        .to_string();
        let path = format!("{}/", base.path().trim_end_matches('/'));
        base.set_path(&path);
        let endpoint = if mode == "range" {
            "query_range"
        } else {
            "query"
        };
        let url = base
            .join(&format!("api/v1/{endpoint}"))
            .map_err(|_| "Invalid Prometheus API URL".to_owned())?;
        sent.push((
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        ));
        Ok(Self {
            request: Request {
                headers: sent,
                body: Some(form.finish()),
                timeout_ms: Some(TIMEOUT_MS),
                connect_ms: Some(10_000),
                redirects: Some(0),
                ..Request::new("POST", url.as_str())
            },
            format,
            columns,
            chart,
            interval,
            step,
            chart_identity,
        })
    }
}
