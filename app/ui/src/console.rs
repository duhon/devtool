use serde_json::Value;
use services_contracts::manifest::Shape;
pub use services_module_ui::{curl, imported_body, pairs, pretty, quote};
use std::collections::BTreeMap;

pub fn validate_hints(text: &str) -> Result<(), String> {
    for (index, line) in text.lines().enumerate() {
        if line.trim().starts_with("##") {
            return Err(format!(
                "Line {}: ## service headers are managed automatically; use a single # for sections.",
                index + 1
            ));
        }
        if line.contains('\0') {
            return Err(format!(
                "Line {}: NUL characters are not supported.",
                index + 1
            ));
        }
        let mut rest = line;
        while let Some(start) = rest.find("{{") {
            let Some(end) = rest[start + 2..].find("}}") else {
                return Err(format!(
                    "Line {}: incomplete variable reference.",
                    index + 1
                ));
            };
            let reference = &rest[start + 2..start + 2 + end];
            if reference.contains(['{', '}']) || !reference.contains(':') {
                return Err(format!("Line {}: invalid variable reference.", index + 1));
            }
            rest = &rest[start + 2 + end + 2..];
        }
    }
    Ok(())
}

/// The `#marker` sections of a service, in manifest order; a Kernel service's are what its
/// module completes, then the console's `#options`.
pub fn markers(service: &str) -> Vec<String> {
    if crate::kernel::is_service(service) {
        let mut markers = crate::kernel::markers(service);
        markers.push("options".into());
        return markers;
    }
    crate::services::get(service)
        .map(|service| {
            service
                .console
                .sections
                .iter()
                .map(|section| section.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

pub fn is_marker(service: &str, marker: &str) -> bool {
    if crate::kernel::is_service(service) {
        return markers(service)
            .iter()
            .any(|known| known.eq_ignore_ascii_case(marker));
    }
    crate::services::get(service).is_some_and(|service| service.section(marker).is_some())
}

fn header_shaped(line: &str) -> bool {
    let line = line.trim();
    !line.contains("://")
        && line.split_once(':').is_some_and(|(k, _)| {
            k.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && k.trim()
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

pub fn sections(service: &str, source: &str) -> BTreeMap<String, String> {
    let mut result: BTreeMap<String, String> = BTreeMap::new();
    if crate::kernel::is_service(service) {
        // The module owns the section shapes: a section runs to the next marker.
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
        return result;
    }
    let service = crate::services::get(service);
    let mut current = String::from("leftover");
    let mut single = false;
    let mut headers = false;
    for raw in source.lines() {
        let line = raw.trim();
        let marker = line.strip_prefix('#').unwrap_or("").to_ascii_lowercase();
        if let Some(section) = service.as_ref().and_then(|s| s.section(&marker)) {
            single = section.shape == Shape::Line;
            headers = section.shape == Shape::Pairs;
            current = marker;
            result.entry(current.clone()).or_default();
            continue;
        }
        if single && line.is_empty() {
            continue;
        }
        if headers && (line.is_empty() || !header_shaped(raw)) {
            current = "leftover".into();
            headers = false;
        }
        let entry = result.entry(current.clone()).or_default();
        if !entry.is_empty() {
            entry.push('\n');
        }
        entry.push_str(raw);
        if single {
            current = "leftover".into();
            single = false;
        }
    }
    result.values_mut().for_each(|s| *s = s.trim().to_owned());
    result
}

/// How the console shows a table response (`Format:` in `#options`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Table,
    Vertical,
    Json,
}

/// The `#options` keys the console handles itself; the module never receives them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunOptions {
    /// `Repeat: 5s` (`s`, `m` or `h`): run again that long (here in milliseconds) after each
    /// response, until Stop.
    pub repeat: Option<u32>,
    /// `Repeat-Until: changed`: a repeat stops when the shown response changes.
    pub until_changed: bool,
    pub format: Option<Format>,
}

/// Takes the console's own keys out of `#options`. What remains goes to a module that
/// declares the section; one that does not only has the console's keys.
fn run_options(
    manifest: &services_contracts::manifest::Manifest,
    sections: &mut BTreeMap<String, String>,
) -> Result<RunOptions, String> {
    let Some(text) = sections.remove("options") else {
        return Ok(RunOptions::default());
    };
    let grid = manifest.console.response == services_contracts::manifest::View::Grid;
    let own = manifest
        .section("options")
        .is_some_and(|section| section.param.is_some());
    let (options, rest) = console_options(&text, &manifest.name, grid, own)?;
    if !rest.is_empty() {
        sections.insert("options".into(), rest);
    }
    Ok(options)
}

/// The console's keys of an `#options` text and the lines left for the module.
fn console_options(
    text: &str,
    name: &str,
    grid: bool,
    own: bool,
) -> Result<(RunOptions, String), String> {
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
            "format" if !grid => {
                return Err(format!(
                    "Format applies to table responses; {name} answers with a body"
                ));
            }
            "format" => {
                options.format = Some(match value.to_ascii_lowercase().as_str() {
                    "table" => Format::Table,
                    "vertical" => Format::Vertical,
                    "json" => Format::Json,
                    _ => {
                        return Err(format!(
                            "Format takes table, vertical or json, not {value:?}"
                        ));
                    }
                });
            }
            _ => rest.push(line),
        }
    }
    if options.until_changed && options.repeat.is_none() {
        return Err("Repeat-Until needs Repeat, like Repeat: 5s".into());
    }
    if let Some(line) = rest.first().filter(|_| !own) {
        let option = line
            .split_once(':')
            .map_or(*line, |(option, _)| option)
            .trim();
        return Err(format!(
            "Unknown option {option:?}; {name} takes {}",
            if grid {
                "Repeat, Repeat-Until and Format"
            } else {
                "Repeat and Repeat-Until"
            }
        ));
    }
    Ok((options, rest.join("\n")))
}

/// A Kernel service's check (the text after its `#check` line) as its module gets it, and
/// the options the console runs it with: the console owns `#options`, so it is cut out.
pub fn kernel_run(
    service: &str,
    source: &str,
) -> Result<(services_sdk::Check, RunOptions), String> {
    if !crate::kernel::loaded(service) {
        return Err(crate::kernel::unavailable(service));
    }
    let name = crate::kernel::manifest(service)
        .map_or_else(|| service.to_owned(), |manifest| manifest.name);
    let mut text = Vec::new();
    let mut options = Vec::new();
    let mut in_options = false;
    for line in source.lines() {
        if let Some((marker, _)) = services_kernel::document::marker(line) {
            in_options = marker == "options";
            if in_options {
                continue;
            }
        }
        if in_options {
            options.push(line);
        } else {
            text.push(line);
        }
    }
    let (run, _) = console_options(&options.join("\n"), &name, false, false)?;
    let check = crate::kernel::check(service, &text.join("\n"), &serde_json::json!({}));
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
    if crate::kernel::is_service(service) {
        let (check, options) = kernel_run(service, source)?;
        let call = crate::kernel::with(|modules| modules.prepare(service, check))?;
        return Ok((crate::kernel::call_value(&call), options));
    }
    let Some(manifest) = crate::services::get(service) else {
        return Err(format!("Unsupported console service: {service}"));
    };
    let ui = crate::services::ui_of(&manifest)?;
    ui.check(source)?;
    let mut sections = sections(service, source);
    let options = run_options(&manifest, &mut sections)?;
    let s = crate::services::Sections(sections);
    if s.text("endpoint").is_empty() {
        return Err(format!("{service} request needs a #endpoint block"));
    }
    let payload = &manifest.console.payload;
    if !manifest.console.implicit_payload && !s.has(payload) && !s.text("leftover").is_empty() {
        return Err(format!(
            "{service} request needs an explicit #{payload} marker before its body"
        ));
    }
    ui.request(&s).map(|params| (params, options))
}

#[derive(Clone, Debug, PartialEq)]
pub struct RequestBlock {
    pub start: usize,
    pub end: usize,
    pub response_end: usize,
    pub text: String,
}

fn marker(line: &str, name: &str) -> bool {
    let line = line.trim_start().to_ascii_lowercase();
    line.strip_prefix(&format!("#{name}"))
        .is_some_and(|s| s.is_empty() || s.starts_with(char::is_whitespace))
}

pub fn is_check(line: &str) -> bool {
    marker(line, "check") || marker(line, "request")
}

pub fn request_at(source: &str, cursor_line: usize) -> Option<RequestBlock> {
    let lines: Vec<_> = source.lines().collect();
    if lines.is_empty() {
        return None;
    }
    let start = (0..=cursor_line.min(lines.len().saturating_sub(1)))
        .rev()
        .find(|i| is_check(lines[*i]))?;
    let next = ((start + 1)..lines.len())
        .find(|i| is_check(lines[*i]))
        .unwrap_or(lines.len());
    let end = ((start + 1)..next)
        .find(|i| marker(lines[*i], "responce") || marker(lines[*i], "response"))
        .unwrap_or(next);
    Some(RequestBlock {
        start,
        end,
        response_end: next,
        text: lines[start + 1..end].join("\n").trim().into(),
    })
}

pub fn replace_response(source: &str, request_line: usize, status: &str, body: &str) -> String {
    let Some(block) = request_at(source, request_line) else {
        return source.into();
    };
    let lines: Vec<_> = source.lines().collect();
    let before = lines[..block.end].join("\n");
    let after = lines[block.response_end..].join("\n");
    format!(
        "{}\n\n#responce {}\n{}\n{}{}",
        before.trim_end(),
        status,
        body.trim_end(),
        if after.is_empty() { "" } else { "\n" },
        after
    )
}

/// The program a service imports and exports checks as.
fn command_line(service: &str) -> Option<String> {
    crate::services::get(service)?.console.command.clone()
}

/// Kernel services always import and export (their module does).
/// Whether `service` exports and imports checks now: a Kernel service once its module runs.
pub fn transfers(service: &str) -> bool {
    crate::kernel::loaded(service) || command_line(service).is_some()
}

pub fn import(service: &str, command: &str) -> Result<String, String> {
    if crate::kernel::is_service(service) {
        return crate::kernel::with(|modules| modules.import(service, command.to_owned()));
    }
    if command_line(service).is_none() {
        return Err(format!("Import isn't supported for {service}"));
    }
    let ui = crate::services::ui_of(&crate::services::get(service).expect("command line"))?;
    let imported = ui.import(command)?;
    let mut lines = vec!["#endpoint".into(), imported.endpoint];
    for (section, text) in imported.sections {
        lines.extend([format!("#{section}"), text]);
    }
    Ok(lines.join("\n"))
}

pub fn export(service: &str, source: &str) -> Result<String, String> {
    if crate::kernel::is_service(service) {
        let (check, _) = kernel_run(service, source)?;
        return crate::kernel::with(|modules| modules.export(service, check));
    }
    let p = parse(service, source)?;
    if command_line(service).is_none() {
        return Err(format!("Export isn't supported for {service}"));
    }
    let ui = crate::services::ui_of(&crate::services::get(service).expect("parsed"))?;
    ui.export(&p)
}

pub fn fuzzy_match(candidate: &str, query: &str) -> bool {
    let mut chars = candidate.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .all(|q| chars.by_ref().any(|c| c == q))
}
