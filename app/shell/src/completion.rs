use crate::console;
use serde_json::Value;
use services_widgets::editor::document::check_at;
use services_widgets::editor::language::{Candidate, Completion};
use std::collections::BTreeSet;

fn utf16_prefix(text: &str, count: usize) -> String {
    let mut n = 0;
    text.chars()
        .take_while(|c| {
            n += c.len_utf16();
            n <= count
        })
        .collect()
}
fn finish(from: usize, to: usize, query: &str, candidates: Vec<(String, String)>) -> Completion {
    let mut seen = BTreeSet::new();
    Completion {
        from,
        to,
        candidates: candidates
            .into_iter()
            .filter(|(s, _)| {
                s.trim() != query.trim() && console::fuzzy_match(s, query) && seen.insert(s.clone())
            })
            .take(100)
            .map(|(text, detail)| Candidate {
                text,
                label: None,
                detail,
                continue_hint: false,
                literal: false,
            })
            .collect(),
        comma: None,
        automatic: true,
    }
}
/// The Hints editor completes `#section` headers of the edited service that the
/// text does not contain yet; `##` lines are rejected by validation instead.
fn hint_sections(service: &str, lines: &[&str], line: usize, before: &str) -> Completion {
    let full = lines.get(line).copied().unwrap_or("");
    let typed = before.trim_start();
    if !typed.starts_with('#') || typed.starts_with("##") || typed.contains(char::is_whitespace) {
        return finish(0, 0, "", vec![]);
    }
    let offset = before.len() - typed.len();
    let end = full[offset..]
        .find(char::is_whitespace)
        .map_or(full.len(), |length| offset + length);
    let used: BTreeSet<_> = lines
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != line)
        .filter_map(|(_, text)| text.trim().strip_prefix('#'))
        .map(str::to_ascii_lowercase)
        .collect();
    let candidates = console::markers(service)
        .into_iter()
        .filter(|marker| !used.contains(&marker.to_ascii_lowercase()))
        .map(|marker| (format!("#{marker}"), "Section".into()))
        .collect();
    finish(
        before[..offset].encode_utf16().count(),
        full[..end].encode_utf16().count(),
        typed,
        candidates,
    )
}
/// `field` is empty for Console documents; otherwise the editor holds one
/// section and `field` names its marker (`headers`, `body`, `variables`, ...).
/// The Hints editor uses `service` "hints" with the edited service as `field`.
pub fn suggest(
    service: &str,
    field: &str,
    source: &str,
    line: usize,
    ch: usize,
    hints: &[(String, String)],
) -> Completion {
    let lines: Vec<_> = source.split('\n').collect();
    let full = lines.get(line).copied().unwrap_or("");
    let before = utf16_prefix(full, ch);
    if let Some(start) = before.rfind("{{").filter(|i| !before[*i..].contains("}}")) {
        let suffix = &full[before.len()..];
        let to = suffix
            .find("}}")
            .map(|n| ch + suffix[..n + 2].encode_utf16().count())
            .unwrap_or(ch);
        let fragment = &before[start + 2..];
        let candidates = if !fragment.contains(':') {
            let mut profiles = BTreeSet::new();
            hints
                .iter()
                .filter_map(|(s, _)| s.strip_prefix("{{"))
                .filter_map(|s| s.split_once(':'))
                .filter_map(|(profile, _)| {
                    if profiles.insert(profile.to_string()) {
                        Some((
                            format!("{{{{{profile}:"),
                            if profile.is_empty() {
                                "Search all profiles and env".into()
                            } else {
                                format!("Profile {profile}")
                            },
                        ))
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            let profile = fragment.split(':').next().unwrap_or("");
            hints
                .iter()
                .filter(|(s, _)| s.starts_with(&format!("{{{{{profile}:")))
                .map(|(s, detail)| {
                    let detail = if detail.starts_with(SAVED) {
                        SAVED.into()
                    } else {
                        detail.clone()
                    };
                    (s.clone(), detail)
                })
                .collect()
        };
        return finish(
            before[..start].encode_utf16().count(),
            to,
            &before[start..],
            candidates,
        );
    }
    if service == "hints" {
        return hint_sections(field, &lines, line, &before);
    }
    if crate::kernel::is_service(service) {
        return if field.is_empty() {
            kernel_suggest(service, &lines, line, &before, hints)
        } else {
            finish(ch, ch, "", vec![])
        };
    }
    finish(ch, ch, "", vec![])
}
/// The console's `#options` keys.
const OPTION_HINTS: [(&str, &str); 2] = [
    (
        "Repeat: 5s",
        "Run again 5s, 2m or 1h after each response, until Stop",
    ),
    (
        "Repeat-Until: changed",
        "Stop repeating when the response changes",
    ),
];

/// The check of a Kernel service around the cursor at `line`, `ch` (UTF-16) of `source`, and
/// the cursor's UTF-8 offset in the check text; `None` outside a check.
pub fn kernel_check(
    service: &str,
    source: &str,
    line: usize,
    ch: usize,
) -> Option<(services_sdk::Check, u32)> {
    let lines: Vec<&str> = source.split('\n').collect();
    let before = utf16_prefix(lines.get(line)?, ch);
    let block = check_at(source, line).filter(|block| block.start < line)?;
    let start = block.start + 1;
    // `check_at` does not count a last empty line, where the cursor may be.
    let end = block.response_end.max(line + 1).min(lines.len());
    let line_start: usize = lines[start..line].iter().map(|text| text.len() + 1).sum();
    let check = crate::kernel::check(service, &lines[start..end].join("\n"), &Value::Null);
    Some((check, (line_start + before.len()) as u32))
}

/// Items that start on an earlier line with a comma after the previous value (`,\n  "a": `)
/// start at the cursor's word instead, and the comma goes where the console puts it: at the
/// returned line and UTF-16 column. `text` is the check up to the cursor, from `first` line
/// of the source.
fn separate_comma(
    completed: &mut services_sdk::Completions,
    text: &str,
    line_start: usize,
    first: usize,
) -> Option<(usize, usize)> {
    let from = completed.from as usize;
    if from >= line_start || completed.items.is_empty() || !text.is_char_boundary(from) {
        return None;
    }
    let gap = &text[from..];
    let start = from + gap.len() - gap.trim_start().len();
    let prefix = format!(",{}", &text[from..start]);
    if !completed
        .items
        .iter()
        .all(|item| item.insert.starts_with(&prefix))
    {
        return None;
    }
    for item in &mut completed.items {
        item.insert.drain(..prefix.len());
    }
    completed.from = start as u32;
    let line_from = text[..from].rfind('\n').map_or(0, |at| at + 1);
    Some((
        first + text[..from].matches('\n').count(),
        text[line_from..from].encode_utf16().count(),
    ))
}

/// A Kernel service's hints: its module completes the check; the console adds `#check`,
/// `#options` (and its keys) and the saved lines of the section.
fn kernel_suggest(
    service: &str,
    lines: &[&str],
    line: usize,
    before: &str,
    hints: &[(String, String)],
) -> Completion {
    let full = lines.get(line).copied().unwrap_or("");
    let ch = before.encode_utf16().count();
    let typed = before.trim_start();
    let offset = before.len() - typed.len();
    let word_end = full[offset..]
        .find(char::is_whitespace)
        .map_or(full.len(), |length| offset + length);
    let marker_range = (
        before[..offset].encode_utf16().count(),
        full[..word_end].encode_utf16().count(),
    );
    let source = lines.join("\n");
    let Some(block) = check_at(&source, line).filter(|block| block.start < line) else {
        let marker = typed.starts_with('#') || full.trim().is_empty();
        let candidates = if marker {
            vec![("#check".to_owned(), "Section".to_owned())]
        } else {
            vec![]
        };
        return finish(marker_range.0, marker_range.1, typed, candidates);
    };
    let section = (block.start + 1..line)
        .rev()
        .find_map(|at| services_kernel::document::marker(lines[at]))
        .map(|(name, _)| name)
        .unwrap_or_default();
    let used: BTreeSet<String> = (block.start + 1..block.end)
        .filter(|at| *at != line)
        .filter_map(|at| services_kernel::document::marker(lines[at]))
        .map(|(name, _)| name)
        .collect();
    let console_markers = || {
        ["options", "check"]
            .into_iter()
            .filter(|marker| *marker == "check" || !used.contains(*marker))
            .map(|marker| (format!("#{marker}"), "Section".to_owned()))
            .collect::<Vec<_>>()
    };
    let (own, _) = scoped(hints, &section);
    if section == "options" && !typed.starts_with('#') {
        let keys: BTreeSet<String> = (block.start + 1..block.end)
            .filter(|at| *at != line)
            .filter_map(|at| lines[at].split_once(':'))
            .map(|(key, _)| key.trim().to_ascii_lowercase())
            .collect();
        let candidates = OPTION_HINTS
            .iter()
            .map(|(text, detail)| ((*text).to_owned(), (*detail).to_owned()))
            .chain(own)
            .filter(|(text, _)| {
                let key = text.split(':').next().unwrap_or("").to_ascii_lowercase();
                !keys.contains(&key) && (key != "repeat-until" || keys.contains("repeat"))
            })
            .collect();
        return finish(0, ch, before.trim(), candidates);
    }
    let line_start: usize = lines[block.start + 1..line]
        .iter()
        .map(|text| text.len() + 1)
        .sum();
    let checked = kernel_check(service, &source, line, ch);
    let (text, pos) = checked
        .as_ref()
        .map(|(check, pos)| (check.text.clone(), *pos as usize))
        .unwrap_or_default();
    let mut completed = checked
        .and_then(|(check, pos)| {
            crate::kernel::with(|modules| modules.complete(service, check, pos)).ok()
        })
        .unwrap_or_default();
    let comma = separate_comma(&mut completed, &text[..pos], line_start, block.start + 1);
    let column = |at: u32| {
        let at = (at as usize).saturating_sub(line_start).min(full.len());
        let at = (0..=at)
            .rev()
            .find(|at| full.is_char_boundary(*at))
            .unwrap_or(0);
        full[..at].encode_utf16().count()
    };
    let (mut from, mut to) = (column(completed.from), column(completed.to).max(ch));
    if completed.items.is_empty() {
        (from, to) = if typed.starts_with('#') || full.trim().is_empty() {
            marker_range
        } else {
            (marker_range.0, ch)
        };
    }
    let query = utf16_prefix(full, ch)
        .encode_utf16()
        .skip(from)
        .collect::<Vec<_>>();
    let query = String::from_utf16_lossy(&query);
    let sections = completed.items.iter().any(|item| item.detail == "Section");
    let mut candidates: Vec<Candidate> = completed
        .items
        .into_iter()
        .map(|item| {
            let mut text = if item.insert.is_empty() {
                item.label.clone()
            } else {
                item.insert
            };
            if text.starts_with('#') {
                // The console ends a marker line itself.
                text = text.trim_end_matches('\n').to_owned();
            }
            Candidate {
                label: (text != item.label).then_some(item.label),
                // The module puts the cursor with `$0`; other inserts go in as they are.
                literal: !text.contains("$0") && !text.starts_with('#'),
                text,
                detail: item.detail,
                continue_hint: item.next,
            }
        })
        .collect();
    let mut extra = Vec::new();
    if sections || typed.starts_with('#') || (candidates.is_empty() && full.trim().is_empty()) {
        extra.extend(console_markers());
    }
    if !typed.starts_with('#') {
        extra.extend(own);
    }
    let known: BTreeSet<String> = candidates.iter().map(|c| c.text.clone()).collect();
    candidates.extend(
        finish(from, to, &query, extra)
            .candidates
            .into_iter()
            .filter(|candidate| !known.contains(&candidate.text)),
    );
    candidates.truncate(100);
    Completion {
        from,
        to,
        candidates,
        comma,
        automatic: true,
    }
}

const SAVED: &str = "Saved hint";
/// The lines of a service's Hints text. A line under `#section` is tagged with it and is
/// offered in that section only; a line before any section is offered by its shape.
pub fn saved_hints(text: &str) -> Vec<(String, String)> {
    let mut section = None;
    let mut hints = vec![];
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Some(name) = line.strip_prefix('#') {
            section = Some(name.trim().to_ascii_lowercase());
        } else {
            let detail = section
                .as_ref()
                .map_or(SAVED.into(), |section| format!("{SAVED} #{section}"));
            hints.push((line.to_owned(), detail));
        }
    }
    hints
}
/// Hint text and its detail.
type Hints = Vec<(String, String)>;
/// Splits the hints into the saved lines of `marker` and everything not saved for
/// another section.
fn scoped(hints: &[(String, String)], marker: &str) -> (Hints, Hints) {
    let mut own = vec![];
    let mut rest = vec![];
    for (text, detail) in hints {
        match detail
            .strip_prefix(SAVED)
            .and_then(|tag| tag.strip_prefix(" #"))
        {
            Some(section) if section == marker => own.push((text.clone(), SAVED.into())),
            Some(_) => {}
            None => rest.push((text.clone(), detail.clone())),
        }
    }
    (own, rest)
}
