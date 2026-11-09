use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The `#marker` sections of a service: what its module completes, then the console's
/// `#options` unless the module has its own.
pub fn markers(service: &str) -> Vec<String> {
    if !crate::kernel::is_service(service) {
        return Vec::new();
    }
    let mut markers = crate::kernel::markers(service);
    if !markers.iter().any(|marker| marker == "options") {
        markers.push("options".into());
    }
    markers
}

pub fn is_marker(service: &str, marker: &str) -> bool {
    markers(service)
        .iter()
        .any(|known| known.eq_ignore_ascii_case(marker))
}

/// A check's sections by marker; text before any marker is `leftover`. The module owns the
/// section shapes, so a section runs to the next marker.
pub fn sections(service: &str, source: &str) -> BTreeMap<String, String> {
    let mut result: BTreeMap<String, String> = BTreeMap::new();
    let mut current = String::from("leftover");
    for raw in source.lines() {
        if let Some((marker, _)) = services_kernel::document::marker(raw)
            && is_marker(service, &marker)
        {
            current = marker.to_ascii_lowercase();
            result.entry(current.clone()).or_default();
            continue;
        }
        let entry = result.entry(current.clone()).or_default();
        if !entry.is_empty() {
            entry.push('\n');
        }
        entry.push_str(raw);
    }
    result.values_mut().for_each(|s| *s = s.trim().to_owned());
    result
}

/// The `#options` keys the console handles itself; the module never receives them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunOptions {
    /// `Repeat: 5s` (`s`, `m` or `h`): run again that long (here in milliseconds) after each
    /// response, until Stop.
    pub repeat: Option<u32>,
    /// `Repeat-Until: changed`: a repeat stops when the shown response changes.
    pub until_changed: bool,
}

/// The console's keys of an `#options` text and the lines left for the module.
fn console_options(text: &str, name: &str) -> Result<RunOptions, String> {
    let mut options = RunOptions::default();
    let mut rest = vec![];
    for line in text.lines() {
        let Some((name, value)) = line.split_once(':') else {
            if !line.trim().is_empty() {
                rest.push(line);
            }
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "repeat" => {
                options.repeat = Some(interval(value).ok_or_else(|| {
                    format!("Repeat takes a time from 1s to 24h, like 5s, 2m or 1h, not {value:?}")
                })?);
            }
            "repeat-until" if value.eq_ignore_ascii_case("changed") => options.until_changed = true,
            "repeat-until" => {
                return Err(format!("Repeat-Until takes \"changed\", not {value:?}"));
            }
            _ => rest.push(line),
        }
    }
    if options.until_changed && options.repeat.is_none() {
        return Err("Repeat-Until needs Repeat, like Repeat: 5s".into());
    }
    if let Some(line) = rest.first() {
        let option = line
            .split_once(':')
            .map_or(*line, |(option, _)| option)
            .trim();
        return Err(format!(
            "Unknown option {option:?}; {name} takes Repeat and Repeat-Until"
        ));
    }
    Ok(options)
}

/// A Kernel service's check (the text after its `#check` line) as its module gets it, and
/// the options the console runs it with. The console owns Repeat and Repeat-Until; a module
/// that runs its checks itself gets the rest of `#options`, others none.
pub fn kernel_run(
    service: &str,
    source: &str,
) -> Result<(services_sdk::Check, RunOptions), String> {
    if !crate::kernel::loaded(service) {
        return Err(crate::kernel::unavailable(service));
    }
    let name = crate::kernel::manifest(service)
        .map_or_else(|| service.to_owned(), |manifest| manifest.name);
    let runs = crate::kernel::with(|modules| modules.runs(service));
    let mut text = Vec::new();
    let mut options = Vec::new();
    let mut in_options = false;
    for line in source.lines() {
        if let Some((marker, _)) = services_kernel::document::marker(line) {
            in_options = marker == "options";
            if in_options && !runs {
                continue;
            }
            text.push(line);
            continue;
        }
        let console = line.split_once(':').is_some_and(|(name, _)| {
            matches!(
                name.trim().to_ascii_lowercase().as_str(),
                "repeat" | "repeat-until"
            )
        });
        if in_options && (console || !runs) {
            options.push(line);
        } else {
            text.push(line);
        }
    }
    let run = console_options(&options.join("\n"), &name)?;
    let check = services_kernel::document::check(service, &text.join("\n"), &serde_json::json!({}));
    Ok((check, run))
}

/// `Repeat`'s time in milliseconds: a whole number of seconds, minutes or hours, 1s to 24h.
fn interval(value: &str) -> Option<u32> {
    let digits = value.find(|c: char| !c.is_ascii_digit())?;
    let count: u32 = value[..digits].parse().ok()?;
    let unit = match value[digits..].trim().to_ascii_lowercase().as_str() {
        "s" => 1000,
        "m" => 60_000,
        "h" => 3_600_000,
        _ => return None,
    };
    count
        .checked_mul(unit)
        .filter(|ms| (1000..=86_400_000).contains(ms))
}

pub fn parse(service: &str, source: &str) -> Result<Value, String> {
    parse_run(service, source).map(|(params, _)| params)
}

/// The module request of a check and the options the console runs it with.
pub fn parse_run(service: &str, source: &str) -> Result<(Value, RunOptions), String> {
    let (check, options) = kernel_run(service, source)?;
    if crate::kernel::with(|modules| modules.runs(service)) {
        // The module checks it when it runs; `endpoint` shows in approvals.
        let endpoint = check.text_of("endpoint").to_owned();
        return Ok((json!({"check": check.text, "endpoint": endpoint}), options));
    }
    let call = crate::kernel::with(|modules| modules.prepare(service, check))?;
    Ok((crate::kernel::call_value(&call), options))
}

/// Whether `service` exports and imports checks now: a Kernel service once its module runs.
pub fn transfers(service: &str) -> bool {
    crate::kernel::loaded(service)
}

pub fn import(service: &str, command: &str) -> Result<String, String> {
    crate::kernel::with(|modules| modules.import(service, command.to_owned()))
}

pub fn export(service: &str, source: &str) -> Result<String, String> {
    let (check, _) = kernel_run(service, source)?;
    crate::kernel::with(|modules| modules.export(service, check))
}
