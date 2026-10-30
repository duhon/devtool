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

/// The `#marker` sections of a service, in manifest order.
pub fn markers(service: &str) -> Vec<String> {
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

pub fn parse(service: &str, source: &str) -> Result<Value, String> {
    let Some(manifest) = crate::services::get(service) else {
        return Err(format!("Unsupported console service: {service}"));
    };
    let ui = crate::services::ui_of(&manifest)?;
    ui.check(source)?;
    let s = crate::services::Sections(sections(service, source));
    if s.text("endpoint").is_empty() {
        return Err(format!("{service} request needs a #endpoint block"));
    }
    let payload = &manifest.console.payload;
    if !manifest.console.implicit_payload && !s.has(payload) && !s.text("leftover").is_empty() {
        return Err(format!(
            "{service} request needs an explicit #{payload} marker before its body"
        ));
    }
    ui.request(&s)
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

pub fn transfers(service: &str) -> bool {
    command_line(service).is_some()
}

pub fn import(service: &str, command: &str) -> Result<String, String> {
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
