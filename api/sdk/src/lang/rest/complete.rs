//! Completion in a REST check: section markers, methods, URLs and OpenAPI paths, headers
//! and JSON body fields from the discovered schema.

use super::Language;
use super::discovery::{Discovery, Probe, endpoint_url};
use crate::lang::{self, endpoint, finish, headers, item, json, marker};
use crate::{Check, Completions, Item};
use serde_json::Value;

const URLS: [&str; 2] = ["http://", "https://"];

/// A line of the check: its byte start and text.
struct Lines<'a>(Vec<(usize, &'a str)>);

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        let mut start = 0;
        Self(
            text.split('\n')
                .map(|line| {
                    let at = start;
                    start += line.len() + 1;
                    (at, line)
                })
                .collect(),
        )
    }
    fn text(&self, line: usize) -> &'a str {
        self.0.get(line).map_or("", |(_, text)| text)
    }
}

pub fn complete<P: Probe>(
    lang: &Language,
    discovery: &Discovery<P>,
    check: &Check,
    pos: usize,
) -> Completions {
    let text = check.text.as_str();
    let mut pos = pos.min(text.len());
    while !text.is_char_boundary(pos) {
        pos -= 1;
    }
    let none = || finish(pos, pos, "", vec![]);
    let lines = Lines::new(text);
    let line = lines
        .0
        .iter()
        .rposition(|(start, _)| *start <= pos)
        .unwrap_or(0);
    let (line_start, full) = lines.0[line];
    let before = &full[..pos - line_start];
    // `{{…}}` variables are the App's.
    if before
        .rfind("{{")
        .is_some_and(|start| !before[start..].contains("}}"))
    {
        return none();
    }
    let markers: Vec<_> = (0..lines.0.len())
        .filter(|i| *i != line)
        .filter_map(|i| Some((i, marker(lines.text(i))?.0)))
        .collect();
    let section = markers.iter().rev().find(|(i, _)| *i < line);
    if markers
        .iter()
        .any(|(i, name)| *i < line && matches!(name.as_str(), "responce" | "response"))
    {
        return none();
    }
    let unused = || -> Vec<Item> {
        lang.sections
            .iter()
            .filter(|name| !markers.iter().any(|(_, used)| used == *name))
            .map(|name| item(format!("#{name}\n"), "Section", "section"))
            .collect()
    };
    if before.trim_start().starts_with('#') || (before.trim().is_empty() && section.is_none()) {
        if before.trim().is_empty() && !full.trim().is_empty() {
            return none();
        }
        let offset = before.len() - before.trim_start().len();
        let end = full[offset..]
            .find(char::is_whitespace)
            .map_or(full.len(), |length| offset + length);
        return finish(
            line_start + offset,
            line_start + end,
            before.trim(),
            unused(),
        );
    }
    let Some((marker_line, name)) = section else {
        return none();
    };
    let body_start = marker_line + 1;
    let section_end = (line + 1..lines.0.len())
        .find(|i| lines.text(*i).trim_start().starts_with('#'))
        .unwrap_or(lines.0.len());
    let url = discovery.resolved(endpoint_url(check.text_of("target")).unwrap_or(""));
    match name.as_str() {
        "target" => {
            if before.trim().is_empty()
                && (body_start..line).any(|i| !lines.text(i).trim().is_empty())
            {
                return finish(line_start, pos, "", unused());
            }
            endpoint_line(lang, discovery, line_start, full, before, pos)
        }
        "headers" => {
            let used = headers::used(
                (body_start..section_end)
                    .filter(|i| *i != line)
                    .map(|i| lines.text(i)),
            );
            let discovered: Vec<_> = discovery
                .schema_or_origin(url)
                .and_then(|schema| schema["headers"].as_array())
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|name| (name, lang.discovered_header))
                .collect();
            let mut result = finish(
                line_start,
                pos,
                before.trim(),
                headers::items(lang.headers, &discovered, &used),
            );
            if before.trim().is_empty() && !used.is_empty() {
                let mut items = finish(line_start, pos, "", unused()).items;
                items.append(&mut result.items);
                items.truncate(lang::MAX_ITEMS);
                result.items = items;
            }
            result
        }
        "payload" => {
            let start = lines.0.get(body_start).map_or(text.len(), |(at, _)| *at);
            let end = lines
                .0
                .get(section_end)
                .map_or(text.len(), |(at, _)| at - 1);
            let fields = body_fields(discovery, check.text_of("target"));
            let mut result = json::complete(&fields, &text[start..end], pos - start);
            result.from += start as u32;
            result.to += start as u32;
            result
        }
        _ => none(),
    }
}

fn endpoint_line<P: Probe>(
    lang: &Language,
    discovery: &Discovery<P>,
    line_start: usize,
    full: &str,
    before: &str,
    pos: usize,
) -> Completions {
    let value_start = before.find(char::is_whitespace).unwrap_or(0);
    if value_start == 0 {
        let methods = lang
            .methods
            .iter()
            .map(|method| item(format!("{method} "), "Method", "keyword"))
            .collect();
        return finish(line_start, pos, before, methods);
    }
    let value = before[value_start..].trim_start();
    let value_at = before.len() - value.len();
    if value.contains("://") {
        let existing = full[before.len()..].starts_with('/');
        let value = if existing { &full[value_at..] } else { value };
        let scheme = value.find("://").map_or(0, |i| i + 3);
        if let Some(path) = value[scheme..].find('/').map(|i| i + scheme) {
            let method = before.split_whitespace().next().unwrap_or("GET");
            let schema = discovery.schema_or_origin(discovery.resolved(value));
            let paths = schema
                .and_then(|schema| schema["paths"].as_array())
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|path| {
                    schema
                        .and_then(|schema| schema["methodsByPath"].get(*path))
                        .and_then(Value::as_array)
                        .is_none_or(|methods| {
                            methods
                                .iter()
                                .any(|m| m.as_str().is_some_and(|m| m.eq_ignore_ascii_case(method)))
                        })
                })
                .map(|path| item(path, "OpenAPI path", "path"))
                .collect();
            let to = if existing {
                line_start + full.len()
            } else {
                pos
            };
            return finish(line_start + value_at + path, to, &value[path..], paths);
        }
    }
    let urls = URLS.iter().map(|url| item(*url, "", "url")).collect();
    finish(line_start + value_at, pos, value, urls)
}

/// The body fields of the endpoint's `METHOD path` in a discovered schema.
fn body_fields<P: Probe>(discovery: &Discovery<P>, endpoint: &str) -> Value {
    let mut words = endpoint.split_whitespace();
    let method = words.next().unwrap_or("GET");
    let Some(url) = words.next().map(|url| discovery.resolved(url)) else {
        return Value::Null;
    };
    let path = endpoint::path(url);
    for schema in discovery.schemas(url) {
        for exact in [true, false] {
            let path = if exact {
                path
            } else {
                path.trim_end_matches('/')
            };
            for (key, fields) in schema["bodySchemas"].as_object().into_iter().flatten() {
                let Some((m, pattern)) = key.split_once(' ') else {
                    continue;
                };
                let pattern = if exact {
                    pattern
                } else {
                    pattern.trim_end_matches('/')
                };
                let matches = m.eq_ignore_ascii_case(method)
                    && pattern.split('/').count() == path.split('/').count()
                    && pattern.split('/').zip(path.split('/')).all(|(a, b)| {
                        a == b || (!b.is_empty() && a.starts_with('{') && a.ends_with('}'))
                    });
                if matches {
                    return fields.clone();
                }
            }
        }
    }
    Value::Null
}
