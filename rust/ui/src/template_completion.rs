use crate::{
    completion::{Candidate, Completion},
    console,
};
use std::collections::BTreeMap;

pub fn suggest(
    service: &str,
    field: &str,
    source: &str,
    line: usize,
    ch: usize,
    templates: &BTreeMap<String, String>,
) -> Option<Completion> {
    if !field.is_empty() {
        return None;
    }
    let lines: Vec<_> = source.split('\n').collect();
    let current = *lines.get(line)?;
    let mut units = 0;
    let before: String = current
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= ch
        })
        .collect();
    let reference = before.rfind('@');
    let (prefix, query) = match reference {
        Some(start) => (&before[..start], &before[start + 1..]),
        None if before.ends_with(char::is_whitespace) => (before.as_str(), ""),
        None => return None,
    };
    if !query
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return None;
    }
    let section = prefix.trim().strip_prefix('#')?;
    if !crate::services::has_template_section(service, section) {
        return None;
    }
    let entries = services_contracts::templates::parse(templates.get(section)?).ok()?;
    let candidates: Vec<_> = entries
        .into_iter()
        .filter(|entry| console::fuzzy_match(&entry.name, query))
        .take(100)
        .map(|entry| {
            let text = if section == "check"
                && !entry.body.lines().next().is_some_and(console::is_check)
            {
                let title = if entry.description.is_empty() {
                    &entry.name
                } else {
                    &entry.description
                };
                format!("#check {title}\n{}", entry.body)
            } else if section != "check" {
                format!("\n{}", entry.body)
            } else {
                entry.body
            };
            Candidate {
                text,
                label: Some(format!("@{}", entry.name)),
                detail: entry.description,
                literal: true,
                continue_hint: false,
            }
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let from = if section == "check" {
        0
    } else {
        prefix.trim_end().encode_utf16().count()
    };
    let after = &current[before.len()..];
    let (after, reference_length) = match after.strip_prefix('@') {
        Some(suffix) if reference.is_none() => (suffix, 1),
        _ => (after, 0),
    };
    let suffix: String = after
        .chars()
        .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .collect();
    Some(Completion {
        from,
        to: ch + reference_length + suffix.encode_utf16().count(),
        candidates,
        comma: None,
        automatic: true,
        automatic_directives_only: false,
    })
}
