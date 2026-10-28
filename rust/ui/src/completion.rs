use crate::console;
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub text: String,
    pub label: Option<String>,
    pub detail: String,
    pub continue_hint: bool,
    pub literal: bool,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub from: usize,
    pub to: usize,
    pub candidates: Vec<Candidate>,
    pub comma: Option<(usize, usize)>,
    pub automatic: bool,
    pub automatic_directives_only: bool,
}
pub fn prepare_insertion(service: &str, candidate: &str, console: bool) -> (String, Option<usize>) {
    let text = if console && candidate.starts_with('#') {
        format!(
            "{candidate}{}",
            if candidate == "#check" { " " } else { "\n" }
        )
    } else {
        candidate.to_owned()
    };
    let inline_arguments = service == "graphql"
        && text.strip_suffix("()").is_some_and(|name| {
            name.trim()
                .strip_prefix('@')
                .unwrap_or(name.trim())
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
        });
    let cursor = inline_arguments
        .then(|| text.len() - 1)
        .or_else(|| {
            text.split_once('\n').and_then(|(first, rest)| {
                let (inner, _) = rest.split_once('\n')?;
                inner
                    .chars()
                    .all(|c| c == ' ' || c == '\t')
                    .then_some(first.len() + 1 + inner.len())
            })
        })
        .or_else(|| (text.ends_with("\"\"") || text.ends_with("[]")).then(|| text.len() - 1));
    let cursor = cursor.or_else(|| {
        (service == "rest" && text.starts_with("{\n"))
            .then(|| text.find(": \"\"").map(|offset| offset + 3))
            .flatten()
    });
    (text, cursor)
}
fn object_snippet(before: &str) -> String {
    block_snippet(before, '{', '}')
}
pub(super) fn block_snippet(before: &str, open: char, close: char) -> String {
    let indent: String = before
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    format!("{open}\n{indent}  \n{indent}{close}")
}
fn utf16_prefix(text: &str, count: usize) -> String {
    let mut n = 0;
    text.chars()
        .take_while(|c| {
            n += c.len_utf16();
            n <= count
        })
        .collect()
}
fn grpc_target_path(endpoint: &str) -> (&str, &str) {
    let offset = endpoint.find("://").map(|i| i + 3).unwrap_or(0);
    match endpoint[offset..].find('/').map(|i| offset + i) {
        Some(path) => (&endpoint[..path], &endpoint[path + 1..]),
        None => (endpoint, ""),
    }
}
pub fn discovery_endpoint(
    service: &str,
    field: &str,
    source: &str,
    line: usize,
    ch: usize,
) -> Option<String> {
    let lines: Vec<_> = source.split('\n').collect();
    if !matches!(service, "rest" | "grpc")
        || !(matches!(
            (service, field),
            ("grpc", "endpointText" | "target") | ("rest", "url")
        ) || field.is_empty()
            && lines[..line.min(lines.len())]
                .iter()
                .rev()
                .map(|line| line.trim())
                .find(|line| line.starts_with('#'))
                .is_some_and(|line| line.eq_ignore_ascii_case("#endpoint")))
    {
        return None;
    }
    let before = utf16_prefix(lines.get(line)?, ch);
    let endpoint = if service == "rest" && field.is_empty() {
        before.split_once(char::is_whitespace)?.1.trim()
    } else {
        before.trim()
    };
    if service == "rest"
        && !endpoint.split_once("://").is_some_and(|(scheme, _)| {
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        })
    {
        return None;
    }
    let (target, _) = grpc_target_path(endpoint);
    let authority = target
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(target);
    (!authority.is_empty() && !endpoint.contains(char::is_whitespace)).then(|| endpoint.to_owned())
}
fn token_start(before: &str) -> usize {
    before
        .char_indices()
        .rev()
        .find(|(_, c)| !c.is_alphanumeric() && *c != '_' && *c != '-')
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0)
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
        automatic_directives_only: false,
    }
}
pub fn suggest(
    service: &str,
    field: &str,
    source: &str,
    line: usize,
    ch: usize,
    hints: &[(String, String)],
    discovery: &Value,
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
                .cloned()
                .collect()
        };
        return finish(
            before[..start].encode_utf16().count(),
            to,
            &before[start..],
            candidates,
        );
    }
    if !console::SERVICES.contains(&service) {
        return finish(ch, ch, "", vec![]);
    }
    let block = if field.is_empty() {
        console::request_at(source, line)
    } else {
        None
    };
    let start_line = block.as_ref().map(|b| b.start).unwrap_or(0);
    let marker = if field.is_empty() {
        (start_line..line)
            .rev()
            .filter_map(|i| lines.get(i))
            .map(|l| l.trim())
            .find(|l| l.starts_with('#'))
            .map(|l| l.trim_start_matches('#').to_ascii_lowercase())
            .unwrap_or_default()
    } else {
        match field {
            "dsn" | "url" | "endpointText" | "target" | "baseUrl" => "endpoint",
            "headersText" => "headers",
            "envText" => "env",
            "requestMessage" => "message",
            "sql" => "query",
            "paramsText" => "params",
            s => s,
        }
        .into()
    };
    let graphql_variables = service == "graphql" && marker == "variables";
    let used: BTreeSet<_> = block
        .as_ref()
        .map(|block| {
            lines
                .iter()
                .enumerate()
                .take(block.end)
                .skip(block.start + 1)
                .filter(|(index, _)| *index != line)
                .filter_map(|(_, text)| text.trim().strip_prefix('#'))
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default();
    let unused_markers = || {
        let sections = if block.is_some() {
            console::markers(service)
        } else {
            &[]
        };
        sections
            .iter()
            .copied()
            .chain(std::iter::once("check"))
            .filter(|marker| !used.contains(*marker))
            .map(|marker| (format!("#{marker}"), "Section".into()))
            .collect::<Vec<_>>()
    };
    let from_marker = field.is_empty()
        && (before.trim_start().starts_with('#')
            || (before.trim().is_empty()
                && ((block.is_none() && !graphql_variables)
                    || matches!(marker.split_whitespace().next(), Some("check" | "request")))));
    if from_marker {
        if before.trim().is_empty() && !full.trim().is_empty() {
            return finish(ch, ch, "", vec![]);
        }
        let offset = before.len() - before.trim_start().len();
        let end = full[offset..]
            .find(char::is_whitespace)
            .map(|length| offset + length)
            .unwrap_or(full.len());
        return finish(
            before[..offset].encode_utf16().count(),
            full[..end].encode_utf16().count(),
            before.trim(),
            unused_markers(),
        );
    }
    let body_start = if field.is_empty() {
        (start_line..line)
            .rev()
            .find(|i| lines[*i].trim().eq_ignore_ascii_case(&format!("#{marker}")))
            .map(|i| i + 1)
            .unwrap_or(start_line)
    } else {
        0
    };
    let section_end = if field.is_empty() {
        (line + 1..lines.len())
            .find(|i| lines[*i].trim_start().starts_with('#'))
            .unwrap_or(lines.len())
    } else {
        lines.len()
    };
    let whole_body = lines[body_start.min(line)..section_end].join("\n");
    let mut body = lines[body_start.min(line)..line].join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    body.push_str(&before);
    if field.is_empty()
        && marker == "endpoint"
        && before.trim().is_empty()
        && lines[body_start.min(line)..line]
            .iter()
            .any(|l| !l.trim().is_empty())
    {
        return finish(0, ch, "", unused_markers());
    }
    if ["headers", "env", "options", "params"].contains(&marker.as_str()) {
        let used: BTreeSet<_> = lines[body_start.min(line)..section_end]
            .iter()
            .enumerate()
            .filter(|(offset, _)| body_start.min(line) + offset != line)
            .filter_map(|(_, l)| l.split_once(':'))
            .map(|(k, _)| k.trim().to_ascii_lowercase())
            .collect();
        let candidates = if marker == "options" {
            let mut options = if service == "mysql" {
                vec![]
            } else {
                vec![
                    ("From: -24h".into(), String::new()),
                    ("To: now".into(), String::new()),
                    ("App: search".into(), String::new()),
                    ("Count: 30".into(), String::new()),
                    ("Preview: false".into(), String::new()),
                    ("Offset: 0".into(), String::new()),
                ]
            };
            options.push((
                "Columns-Type: ".into(),
                "column=json column=datetime column=number column=boolean column=text".into(),
            ));
            if service == "splunk" {
                options.push(("Columns-Size: ".into(), "column=100 (characters)".into()));
            }
            options
        } else if marker == "env" {
            vec![
                ("PATH: ".into(), String::new()),
                ("HOME: ".into(), String::new()),
                ("USER: ".into(), String::new()),
                ("LANG: en_US.UTF-8".into(), String::new()),
                ("NODE_ENV: production".into(), String::new()),
                ("DEBUG: *".into(), String::new()),
            ]
        } else {
            let mut candidates = hints
                .iter()
                .filter(|(s, _)| s.contains(':') && !s.contains("://") && !s.starts_with("{{"))
                .cloned()
                .collect::<Vec<_>>();
            if service == "rest" && marker == "headers" {
                let known: BTreeSet<_> = candidates
                    .iter()
                    .map(|(s, _)| s.split(':').next().unwrap_or("").to_ascii_lowercase())
                    .collect();
                for name in discovery["headers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    if !known.contains(&name.to_ascii_lowercase()) {
                        candidates.push((format!("{name}: "), "OpenAPI header".into()));
                    }
                }
            }
            candidates
        };
        let mut result = finish(
            0,
            ch,
            before.trim(),
            candidates
                .into_iter()
                .filter(|(s, _)| {
                    !used.contains(&s.split(':').next().unwrap_or("").to_ascii_lowercase())
                })
                .collect(),
        );
        if field.is_empty() && before.trim().is_empty() && !used.is_empty() {
            let mut markers = finish(0, ch, "", unused_markers()).candidates;
            markers.extend(result.candidates);
            result.candidates = markers;
        }
        return result;
    }
    if marker == "endpoint" {
        let value_start = if service == "rest" && field.is_empty() {
            before.find(char::is_whitespace).unwrap_or(0)
        } else {
            0
        };
        let value = before[value_start..].trim_start();
        if service == "rest" && field.is_empty() && value_start == 0 {
            return finish(
                0,
                ch,
                &before,
                ["GET ", "POST ", "PUT ", "PATCH ", "DELETE ", "HEAD "]
                    .into_iter()
                    .map(|s| (s.into(), "HTTP method".into()))
                    .collect(),
            );
        }
        let value_start = before[..before.len() - value.len()].encode_utf16().count();
        if service == "grpc" {
            let scheme = value.find("://").map(|i| i + 3).unwrap_or(0);
            let path = value[scheme..].find('/').map(|i| scheme + i + 1);
            if let Some(path) = path {
                let method = value[path..].contains('/');
                if let Some(endpoint) = discovery["discoveryEndpoint"].as_str() {
                    let (target, scoped_path) = grpc_target_path(endpoint);
                    let (current_target, current_path) = grpc_target_path(value);
                    if target != current_target
                        || method && scoped_path.split('/').next() != current_path.split('/').next()
                    {
                        return finish(ch, ch, "", vec![]);
                    }
                }
                let start = if method {
                    value.rfind('/').unwrap() + 1
                } else {
                    path
                };
                let names = discovery[if method { "methods" } else { "services" }]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                return finish(
                    value_start + value[..start].encode_utf16().count(),
                    ch,
                    &value[start..],
                    names
                        .iter()
                        .filter_map(|v| v.as_str().or(v["name"].as_str()))
                        .map(|s| {
                            (
                                if method { s.into() } else { format!("{s}/") },
                                "gRPC reflection".into(),
                            )
                        })
                        .collect(),
                );
            }
        }
        if service == "rest" && value.contains("://") {
            let existing_path = full[before.len()..].starts_with('/');
            let value = if existing_path {
                &full[before.len() - value.len()..]
            } else {
                value
            };
            let scheme = value.find("://").unwrap() + 3;
            if let Some(path) = value[scheme..].find('/').map(|i| i + scheme) {
                let query = &value[path..];
                let candidates = discovery["paths"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .filter(|path| {
                        let method = if field.is_empty() {
                            before.split_whitespace().next().unwrap_or("GET")
                        } else {
                            discovery["requestMethod"].as_str().unwrap_or("GET")
                        };
                        discovery["methodsByPath"]
                            .get(path)
                            .and_then(Value::as_array)
                            .is_none_or(|methods| {
                                methods.iter().any(|m| {
                                    m.as_str().is_some_and(|m| m.eq_ignore_ascii_case(method))
                                })
                            })
                    })
                    .map(|s| (s, "OpenAPI path".into()))
                    .collect();
                return finish(
                    value_start + value[..path].encode_utf16().count(),
                    if existing_path {
                        full.encode_utf16().count()
                    } else {
                        ch
                    },
                    query,
                    candidates,
                );
            }
        }
        let mut candidates = hints
            .iter()
            .filter(|(s, _)| s.contains("://"))
            .cloned()
            .collect::<Vec<_>>();
        candidates.extend(match service {
            "mysql" => vec![("mysql://".into(), String::new())],
            "grpc" => vec![
                ("grpc://".into(), String::new()),
                ("grpcs://".into(), String::new()),
            ],
            "terminal" => vec![
                ("bash -c".into(), String::new()),
                ("zsh -c".into(), String::new()),
                ("ssh ".into(), String::new()),
            ],
            _ => vec![
                ("http://".into(), String::new()),
                ("https://".into(), String::new()),
            ],
        });
        return finish(value_start, ch, value, candidates);
    }
    let start = token_start(&before);
    let query = &before[start..];
    let from = before[..start].encode_utf16().count();
    if graphql_variables {
        let offset = lines[body_start.min(line)..line]
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>()
            + before.len();
        return finish(
            ch,
            ch,
            "",
            json_templates(&whole_body[..offset], &before, &whole_body[offset..]),
        );
    }
    if ["body", "message"].contains(&marker.as_str()) && ["rest", "grpc"].contains(&service) {
        if body.trim().is_empty() {
            let mut candidates = vec![];
            if service == "rest" && whole_body.trim().is_empty() {
                let fields = rest_fields(
                    discovery,
                    block
                        .as_ref()
                        .map(|block| block.text.as_str())
                        .unwrap_or(source),
                    hints,
                );
                if let Some(template) = required_body(&fields)
                    && template
                        .as_object()
                        .is_some_and(|object| !object.is_empty())
                {
                    let template = serde_json::to_string_pretty(&template)
                        .expect("Generated JSON body template is serializable");
                    let indent: String = before.chars().take_while(|c| c.is_whitespace()).collect();
                    candidates.push((
                        template.replace('\n', &format!("\n{indent}")),
                        "Required fields".into(),
                    ));
                }
            }
            candidates.push((object_snippet(&before), "JSON object".into()));
            return finish(ch, ch, "", candidates);
        }
        let root = serde_json::json!({"fields": if service == "grpc" {
            discovery["fields"].clone()
        } else {
            rest_fields(
                discovery,
                block.as_ref().map(|b| b.text.as_str()).unwrap_or(source),
                hints,
            )
        }});
        let offset = lines[body_start.min(line)..line]
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>()
            + before.len();
        if service == "rest"
            && let Some(result) = enum_completion(&root, &whole_body, offset, &before)
        {
            return result;
        }
        let quote = before[..start].ends_with('"');
        let json_start = if quote { from.saturating_sub(1) } else { from };
        let prior = &body[..body.len().saturating_sub(query.len() + usize::from(quote))];
        let trimmed = prior.trim_end();
        let Some((path, used, _)) = json_path(prior) else {
            return finish(ch, ch, "", vec![]);
        };
        let Some(schema) = at_path(&root, &path) else {
            return finish(ch, ch, "", vec![]);
        };
        if schema["kind"] == "array" {
            let offset = lines[body_start.min(line)..line]
                .iter()
                .map(|line| line.len() + 1)
                .sum::<usize>()
                + before.len();
            let next = whole_body[offset..].trim_start().chars().next();
            let candidates = if query.is_empty()
                && !quote
                && trimmed.ends_with(['[', ','])
                && next.is_none_or(|c| matches!(c, ']' | ','))
            {
                let kind = if service == "rest" && schema["items"]["kind"] == "bare" {
                    schema["items"]["type"].as_str()
                } else {
                    schema["items"]["kind"].as_str()
                };
                match kind {
                    Some("object") => vec![(object_snippet(&before), "JSON object".into())],
                    Some("array") => {
                        vec![(block_snippet(&before, '[', ']'), "JSON array".into())]
                    }
                    Some("string") => vec![("\"\"".into(), "JSON string".into())],
                    Some("number" | "integer") => vec![("0".into(), "JSON number".into())],
                    Some("boolean") => vec![
                        ("true".into(), "JSON boolean".into()),
                        ("false".into(), "JSON boolean".into()),
                    ],
                    _ => vec![],
                }
            } else {
                vec![]
            };
            return finish(ch, ch, "", candidates);
        }
        let needs_comma = !trimmed.ends_with(['{', ',']);
        if needs_comma
            && (trimmed.is_empty()
                || trimmed.ends_with(':')
                || !before[..start - usize::from(quote)].trim().is_empty())
        {
            return finish(from, ch, "", vec![]);
        }
        let mut continuations = BTreeSet::new();
        let candidates = schema["fields"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|field| service != "rest" || field["readOnly"] != true)
            .filter_map(|f| {
                let name = f["name"].as_str()?;
                if used.contains(name) {
                    return None;
                }
                let value = if service == "rest"
                    && f["enum"].is_array()
                    && matches!(f["kind"].as_str(), Some("object" | "array"))
                {
                    String::new()
                } else {
                    match f["kind"].as_str().unwrap_or("bare") {
                        "string" => "\"\"".into(),
                        "object" => object_snippet(&before),
                        "array" => "[]".into(),
                        _ => String::new(),
                    }
                };
                let detail = match f["format"].as_str() {
                    Some("date-time") => "date-time (ISO 8601)",
                    Some("date") => "date (YYYY-MM-DD)",
                    Some(format) => format,
                    None if service == "rest" => {
                        f["type"]
                            .as_str()
                            .unwrap_or_else(|| match f["kind"].as_str() {
                                Some("bare") => "value (type unavailable)",
                                Some(kind) => kind,
                                None => "",
                            })
                    }
                    None => f["kind"].as_str().unwrap_or(""),
                };
                let text = format!("{}: {value}", Value::String(name.into()));
                let detail = if service == "rest" && f["enum"].is_array() {
                    continuations.insert(text.clone());
                    if f["type"].is_string() || f["format"].is_string() {
                        format!("{detail} · enum")
                    } else {
                        "enum".into()
                    }
                } else if service == "rest" && f["items"]["enum"].is_array() {
                    continuations.insert(text.clone());
                    "array<enum>".into()
                } else {
                    detail.into()
                };
                Some((text, detail))
            })
            .collect();
        let mut result = finish(json_start, ch, query, candidates);
        for candidate in &mut result.candidates {
            candidate.continue_hint = continuations.contains(&candidate.text);
        }
        if needs_comma {
            let parts: Vec<_> = trimmed.split('\n').collect();
            result.comma = Some((
                body_start + parts.len() - 1,
                parts.last().unwrap_or(&"").encode_utf16().count(),
            ));
        }
        return result;
    }
    if marker == "query" && service == "graphql" {
        let request_variables = if field.is_empty() {
            let scope = block
                .as_ref()
                .map(|block| block.text.clone())
                .unwrap_or_else(|| {
                    lines
                        .iter()
                        .take_while(|line| !console::is_check(line))
                        .copied()
                        .collect::<Vec<_>>()
                        .join("\n")
                });
            console::sections("graphql", &scope)
                .remove("variables")
                .map(Value::String)
                .unwrap_or(Value::Null)
        } else {
            discovery
                .get("requestVariables")
                .cloned()
                .unwrap_or(Value::Null)
        };
        let body_offset = lines[body_start.min(line)..line]
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>()
            + before.len();
        let (fragment, candidates, automatic, automatic_directives_only) =
            crate::graphql_completion::suggest(
                &body,
                &before,
                &whole_body[body_offset..],
                discovery,
                &request_variables,
            );
        let offset = before.len().saturating_sub(fragment.len());
        let mut result = finish(
            before[..offset].encode_utf16().count(),
            ch,
            &fragment,
            candidates,
        );
        result.automatic = automatic;
        result.automatic_directives_only = automatic_directives_only;
        if field.is_empty()
            && before.trim().is_empty()
            && crate::graphql_completion::operation_complete(&body, discovery)
        {
            result.candidates = finish(0, ch, "", unused_markers()).candidates;
            result
                .candidates
                .sort_by_key(|candidate| candidate.text != "#check");
            result.automatic = true;
        }
        let suffix = &full[before.len()..];
        let name_end = if matches!(fragment.as_str(), "." | "..") {
            suffix.bytes().take_while(|c| *c == b'.').count()
        } else {
            suffix
                .bytes()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == b'_')
                .count()
        };
        if !fragment.is_empty() {
            result.to = ch + name_end;
        }
        if result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "... on ")
        {
            let dots = suffix.bytes().take_while(|byte| *byte == b'.').count();
            if let Some(rest) = suffix[dots..].trim_start().strip_prefix("on")
                && !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
            {
                result
                    .candidates
                    .retain(|candidate| candidate.text == "... on ");
                let remaining = rest.trim_start();
                result.to = ch
                    + suffix[..suffix.len() - remaining.len()]
                        .encode_utf16()
                        .count();
            }
        }
        if result
            .candidates
            .iter()
            .all(|candidate| candidate.text.starts_with('$') || candidate.text.ends_with(": "))
            && result
                .candidates
                .iter()
                .any(|candidate| candidate.text.ends_with(": "))
        {
            result.to = ch + name_end;
            if suffix[name_end..].trim_start().starts_with(':') {
                for candidate in &mut result.candidates {
                    if let Some(name) = candidate.text.strip_suffix(": ") {
                        candidate.text = name.to_owned();
                    }
                }
                result
                    .candidates
                    .retain(|candidate| candidate.text != fragment);
            }
        }
        return result;
    }
    if marker == "query" && service == "mysql" {
        let (fragment, candidates) = crate::sql_completion::suggest(&body, &whole_body, discovery);
        let offset = before.len().saturating_sub(fragment.len());
        return finish(
            before[..offset].encode_utf16().count(),
            ch,
            &fragment,
            candidates,
        );
    }
    finish(
        from,
        ch,
        query,
        hints
            .iter()
            .filter(|(s, _)| !s.starts_with("{{") && !s.starts_with('#'))
            .cloned()
            .collect(),
    )
}
enum JsonPathSegment {
    Field(String),
    Item,
}
fn required_body(fields: &Value) -> Option<Value> {
    let mut object = serde_json::Map::new();
    for field in fields
        .as_array()?
        .iter()
        .filter(|field| field["required"] == true && field["readOnly"] != true)
    {
        let value = if let Some(values) = field["enum"].as_array() {
            values.first()?.clone()
        } else {
            match field["kind"].as_str()? {
                "string" => Value::String(String::new()),
                "array" => Value::Array(vec![]),
                "object" => required_body(&field["fields"])?,
                "bare" => match field["type"].as_str()? {
                    "boolean" => Value::Bool(false),
                    "number" | "integer" => Value::from(0),
                    _ => return None,
                },
                _ => return None,
            }
        };
        object.insert(field["name"].as_str()?.to_string(), value);
    }
    Some(Value::Object(object))
}
fn at_path<'a>(root: &'a Value, path: &[JsonPathSegment]) -> Option<&'a Value> {
    let mut schema = root;
    for segment in path {
        schema = match segment {
            JsonPathSegment::Field(name) => schema["fields"]
                .as_array()?
                .iter()
                .find(|f| f["name"] == *name)?,
            JsonPathSegment::Item => schema.get("items")?,
        };
    }
    Some(schema)
}
fn rest_fields(discovery: &Value, request: &str, hints: &[(String, String)]) -> Value {
    let endpoint = console::sections("rest", request)
        .remove("endpoint")
        .unwrap_or_else(|| discovery["requestEndpoint"].as_str().unwrap_or("").into());
    let mut p = endpoint.split_whitespace();
    let method = p.next().unwrap_or("GET");
    let Some(path) = p
        .next()
        .and_then(|u| crate::profiles::resolve_text(u, hints).ok())
        .and_then(|u| url::Url::parse(&u).ok())
        .map(|u| u.path().to_owned())
    else {
        return Value::Null;
    };
    for exact in [true, false] {
        let path = if exact {
            path.as_str()
        } else {
            path.trim_end_matches('/')
        };
        for (key, fields) in discovery["bodySchemas"].as_object().into_iter().flatten() {
            let Some((m, pattern)) = key.split_once(' ') else {
                continue;
            };
            if m != method {
                continue;
            }
            let pattern = if exact {
                pattern
            } else {
                pattern.trim_end_matches('/')
            };
            let matches =
                pattern.split('/').zip(path.split('/')).all(|(a, b)| {
                    a == b || (!b.is_empty() && a.starts_with('{') && a.ends_with('}'))
                }) && pattern.split('/').count() == path.split('/').count();
            if matches {
                return fields.clone();
            }
        }
    }
    Value::Null
}
fn enum_completion(root: &Value, body: &str, offset: usize, before: &str) -> Option<Completion> {
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in body[..offset].char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
        } else if character == '"' {
            quoted = true;
        } else if matches!(character, '{' | '[' | '}' | ']' | ':' | ',') {
            start = index + character.len_utf8();
        }
    }
    start += body[start..offset].len() - body[start..offset].trim_start().len();
    let (mut path, _, pending) = json_path(&body[..start])?;
    if let Some(name) = pending {
        path.push(JsonPathSegment::Field(name));
    } else if at_path(root, &path)?["kind"] == "array" {
        path.push(JsonPathSegment::Item);
    } else {
        return None;
    }
    let values = at_path(root, &path)?["enum"].as_array()?;
    let remaining = &body[start..];
    let end = if let Some(text) = remaining.strip_prefix('"') {
        let mut escaped = false;
        let mut length = remaining.len();
        for (index, character) in text.char_indices() {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' || character == '\n' {
                length = index + if character == '"' { 2 } else { 1 };
                break;
            }
        }
        start + length
    } else if remaining.starts_with(['{', '[']) {
        let mut stream = serde_json::Deserializer::from_str(remaining).into_iter::<Value>();
        stream.next()?.ok()?;
        start + stream.byte_offset()
    } else {
        start
            + remaining
                .find(|character: char| {
                    character.is_whitespace() || matches!(character, ',' | '}' | ']')
                })
                .unwrap_or(remaining.len())
    };
    let line_start = offset - before.len();
    let line_end = offset + body[offset..].find('\n').unwrap_or(body.len() - offset);
    if start < line_start || end > line_end {
        return None;
    }
    let query = &body[start..offset];
    let query = if query == "\""
        || serde_json::from_str::<Value>(&body[start..end])
            .is_ok_and(|value| values.contains(&value))
    {
        ""
    } else {
        query
    };
    let mut result = finish(
        body[line_start..start].encode_utf16().count(),
        body[line_start..end].encode_utf16().count(),
        query,
        values
            .iter()
            .map(|value| (value.to_string(), "enum value".into()))
            .collect(),
    );
    for candidate in &mut result.candidates {
        candidate.literal = true;
    }
    Some(result)
}
fn json_path(source: &str) -> Option<(Vec<JsonPathSegment>, BTreeSet<String>, Option<String>)> {
    let chars: Vec<char> = source.chars().collect();
    let mut path = Vec::new();
    let mut scopes: Vec<(char, BTreeSet<String>, Option<String>)> = Vec::new();
    let mut i = 0;
    let mut root = false;
    while i < chars.len() {
        match chars[i] {
            '"' => {
                let mut end = i + 1;
                while end < chars.len() {
                    if chars[end] == '\\' {
                        end += 2;
                        continue;
                    }
                    if chars[end] == '"' {
                        break;
                    }
                    end += 1;
                }
                if chars.get(end) != Some(&'"') {
                    return None;
                }
                let key =
                    serde_json::from_str::<String>(&chars[i..=end].iter().collect::<String>())
                        .ok()?;
                let mut next = end + 1;
                while next < chars.len() && chars[next].is_whitespace() {
                    next += 1;
                }
                if let Some(('}', used, pending)) = scopes.last_mut()
                    && chars.get(next) == Some(&':')
                {
                    *pending = Some(key.clone());
                    used.insert(key);
                }
                i = end;
            }
            '{' | '[' => {
                if let Some((closing, _, pending)) = scopes.last_mut() {
                    path.push(if *closing == '}' {
                        JsonPathSegment::Field(pending.take()?)
                    } else {
                        JsonPathSegment::Item
                    });
                } else {
                    if root || chars[i] != '{' {
                        return None;
                    }
                    root = true;
                }
                scopes.push((
                    if chars[i] == '{' { '}' } else { ']' },
                    BTreeSet::new(),
                    None,
                ));
            }
            '}' | ']' => {
                if scopes.pop()?.0 != chars[i] {
                    return None;
                }
                path.pop();
            }
            ',' => {
                if let Some((_, _, pending)) = scopes.last_mut() {
                    *pending = None;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let (_, used, pending) = scopes.pop()?;
    Some((path, used, pending))
}

#[derive(Clone, Copy, PartialEq)]
enum JsonSlot {
    Key,
    Colon,
    Value,
    Comma,
}

fn json_templates(source: &str, before: &str, after: &str) -> Vec<(String, String)> {
    let chars: Vec<_> = source.chars().collect();
    let mut stack: Vec<(char, JsonSlot)> = vec![];
    let mut root = false;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            c if c.is_whitespace() => {}
            '{' | '[' => {
                if let Some((_, slot)) = stack.last_mut() {
                    if *slot != JsonSlot::Value {
                        return vec![];
                    }
                    *slot = JsonSlot::Comma;
                } else if root || chars[i] != '{' {
                    return vec![];
                } else {
                    root = true;
                }
                stack.push(if chars[i] == '{' {
                    ('}', JsonSlot::Key)
                } else {
                    (']', JsonSlot::Value)
                });
            }
            '}' | ']' => {
                let Some((closing, slot)) = stack.pop() else {
                    return vec![];
                };
                if closing != chars[i]
                    || !(matches!(slot, JsonSlot::Comma | JsonSlot::Key)
                        || closing == ']' && slot == JsonSlot::Value)
                {
                    return vec![];
                }
            }
            ':' => {
                let Some((_, slot)) = stack.last_mut() else {
                    return vec![];
                };
                if *slot != JsonSlot::Colon {
                    return vec![];
                }
                *slot = JsonSlot::Value;
            }
            ',' => {
                let Some((closing, slot)) = stack.last_mut() else {
                    return vec![];
                };
                if *slot != JsonSlot::Comma {
                    return vec![];
                }
                *slot = if *closing == '}' {
                    JsonSlot::Key
                } else {
                    JsonSlot::Value
                };
            }
            '"' => {
                let start = i;
                i += 1;
                while i < chars.len() {
                    if chars[i] == '\\' {
                        i += 2;
                    } else if chars[i] == '"' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
                if i > chars.len()
                    || serde_json::from_str::<String>(&chars[start..i].iter().collect::<String>())
                        .is_err()
                {
                    return vec![];
                }
                let Some((_, slot)) = stack.last_mut() else {
                    return vec![];
                };
                *slot = match slot {
                    JsonSlot::Key => JsonSlot::Colon,
                    JsonSlot::Value => JsonSlot::Comma,
                    _ => return vec![],
                };
                continue;
            }
            _ => {
                let Some((_, slot)) = stack.last_mut() else {
                    return vec![];
                };
                if *slot != JsonSlot::Value {
                    return vec![];
                }
                let start = i;
                while i < chars.len()
                    && !chars[i].is_whitespace()
                    && !matches!(chars[i], ',' | '}' | ']')
                {
                    i += 1;
                }
                if serde_json::from_str::<Value>(&chars[start..i].iter().collect::<String>())
                    .is_err()
                {
                    return vec![];
                }
                *slot = JsonSlot::Comma;
                continue;
            }
        }
        i += 1;
    }
    let next = after.trim_start().chars().next();
    if !root {
        return if next.is_none() {
            vec![(object_snippet(before), "JSON object".into())]
        } else {
            vec![]
        };
    }
    match stack.last().map(|(_, slot)| slot) {
        Some(JsonSlot::Key) if next.is_none_or(|c| c == '}') => {
            vec![("\"\"".into(), "JSON property".into())]
        }
        Some(JsonSlot::Value) if next.is_none_or(|c| matches!(c, '}' | ']' | ',')) => vec![
            ("\"\"".into(), "JSON string".into()),
            (object_snippet(before), "JSON object".into()),
            (block_snippet(before, '[', ']'), "JSON array".into()),
        ],
        _ => vec![],
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn empty_console_is_safe_and_offers_request() {
        assert!(console::request_at("", 0).is_none());
        for service in console::SERVICES {
            for source in ["", "#"] {
                let hints = suggest(service, "", source, 0, source.len(), &[], &Value::Null);
                assert_eq!(
                    hints
                        .candidates
                        .iter()
                        .map(|c| c.text.as_str())
                        .collect::<Vec<_>>(),
                    vec!["#check"],
                    "{service}: {source}"
                );
            }
        }
    }
    #[test]
    fn newline_after_request_header_offers_unused_sections() {
        for service in console::SERVICES {
            for source in ["#check\n", "#check my query\n", "#check my query\n\n"] {
                let hints = suggest(
                    service,
                    "",
                    source,
                    source.split('\n').count() - 1,
                    0,
                    &[],
                    &Value::Null,
                );
                assert_eq!(
                    hints
                        .candidates
                        .iter()
                        .map(|candidate| candidate.text.clone())
                        .collect::<Vec<_>>(),
                    console::markers(service)
                        .iter()
                        .copied()
                        .chain(std::iter::once("check"))
                        .map(|marker| format!("#{marker}"))
                        .collect::<Vec<_>>(),
                    "{service}: {source}"
                );
            }
        }
        let source = "#check my query\n\n#endpoint\nmysql://db\n";
        let hints = suggest("mysql", "", source, 1, 0, &[], &Value::Null);
        assert_eq!(
            hints
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            vec!["#options", "#query", "#check"]
        );
    }
    #[test]
    fn splunk_suggests_only_options_and_its_parameter_keys() {
        let hints = suggest("splunk", "", "#check Splunk\n#", 1, 1, &[], &Value::Null);
        assert_eq!(
            hints
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            ["#endpoint", "#headers", "#options", "#search", "#check"]
        );
        let hints = suggest(
            "splunk",
            "",
            "#check Splunk\n#options\n",
            2,
            0,
            &[],
            &Value::Null,
        );
        assert_eq!(
            hints
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            [
                "From: -24h",
                "To: now",
                "App: search",
                "Count: 30",
                "Preview: false",
                "Offset: 0",
                "Columns-Type: ",
                "Columns-Size: "
            ]
        );
    }
    #[test]
    fn non_service_editors_never_offer_console_or_service_syntax() {
        let hints = vec![
            ("#check".into(), String::new()),
            ("Accept: */*".into(), String::new()),
            ("GET".into(), String::new()),
            ("SELECT".into(), String::new()),
            ("query".into(), String::new()),
            ("{{prod:TOKEN}}".into(), "fixture".into()),
        ];
        for (service, field) in [("vars", "variables"), ("", "")] {
            for source in [
                "",
                "#",
                "Ac",
                "GET",
                "SEL",
                "qu",
                "NAME: ",
                "#check example\n",
                "#check example\n#headers\n",
            ] {
                let result = suggest(
                    service,
                    field,
                    source,
                    source.split('\n').count() - 1,
                    source
                        .split('\n')
                        .next_back()
                        .unwrap()
                        .encode_utf16()
                        .count(),
                    &hints,
                    &Value::Null,
                );
                assert!(result.candidates.is_empty(), "{service}: {source}");
            }
            let result = suggest(
                service,
                field,
                "NAME: {{prod:TO",
                0,
                15,
                &hints,
                &Value::Null,
            );
            assert_eq!(result.candidates.len(), 1);
            assert_eq!(result.candidates[0].text, "{{prod:TOKEN}}");
        }
    }
    #[test]
    fn unused_markers_only() {
        let source = "#check\n#endpoint\nmysql://db\n#";
        let r = suggest("mysql", "", source, 3, 1, &[], &Value::Null);
        assert!(r.candidates.iter().any(|c| c.text == "#query"));
        assert!(!r.candidates.iter().any(|c| c.text == "#endpoint"));
        assert!(r.candidates.iter().any(|c| c.text == "#check"));
    }
    #[test]
    fn separator_hints_are_repeatable_and_scoped_to_each_request() {
        for service in console::SERVICES {
            let filled = console::markers(service)
                .iter()
                .map(|marker| format!("#{marker}\nvalue\n"))
                .collect::<String>();
            let first = format!("#check first\n{filled}");
            let source = format!("{first}#");
            let line = source.split('\n').count() - 1;
            let result = suggest(service, "", &source, line, 1, &[], &Value::Null);
            assert_eq!(
                result
                    .candidates
                    .iter()
                    .map(|candidate| candidate.text.as_str())
                    .collect::<Vec<_>>(),
                ["#check"],
                "{service}"
            );
            let source = format!("{first}#check second\n#\n#check third\n{filled}");
            let line = first.split('\n').count();
            let result = suggest(service, "", &source, line, 1, &[], &Value::Null);
            let expected = console::markers(service)
                .iter()
                .copied()
                .chain(std::iter::once("check"))
                .map(|marker| format!("#{marker}"))
                .collect::<Vec<_>>();
            assert_eq!(
                result
                    .candidates
                    .iter()
                    .map(|candidate| candidate.text.clone())
                    .collect::<Vec<_>>(),
                expected,
                "{service}"
            );
            let source =
                format!("{first}#check second\n#endpoint\nvalue\n#\n#check third\n{filled}");
            let line = first.split('\n').count() + 2;
            let result = suggest(service, "", &source, line, 1, &[], &Value::Null);
            assert!(
                result
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text == "#check")
            );
            assert!(
                !result
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text == "#endpoint")
            );
            assert_eq!(result.candidates.len(), console::markers(service).len());
        }
    }
    #[test]
    fn existing_separator_completion_preserves_its_suffix_and_other_requests() {
        let source = "#check first\n#endpoint\nurl\n#query\nquery { viewer { name } }\n#check second\n  #endpoint\nurl\n#variables\n{}\n#query\nquery { viewer { name } }";
        let result = suggest("graphql", "", source, 6, 6, &[], &Value::Null);
        assert_eq!((result.from, result.to), (2, 11));
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            ["#endpoint"]
        );
        let result = suggest("graphql", "", source, 5, 4, &[], &Value::Null);
        assert_eq!((result.from, result.to), (0, 6));
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            ["#check"]
        );
        let source = "#check first\n#ENDPOINT\nurl\n#";
        let result = suggest("graphql", "", source, 3, 1, &[], &Value::Null);
        assert!(
            !result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "#endpoint")
        );
    }
    #[test]
    fn completed_graphql_operations_offer_separators_without_polluting_field_hints() {
        let prefix = "#check first\n#endpoint\nhttps://example.test/graphql\n#query\n";
        for operation in [
            "query Q { viewer { name } }",
            "mutation M { update { name } }",
            "subscription S { changed { name } }",
            "{ viewer { name } }",
        ] {
            let source = format!("{prefix}{operation}\n");
            let result = suggest(
                "graphql",
                "",
                &source,
                source.split('\n').count() - 1,
                0,
                &[],
                &Value::Null,
            );
            assert_eq!(
                result
                    .candidates
                    .iter()
                    .map(|candidate| candidate.text.as_str())
                    .collect::<Vec<_>>(),
                ["#check", "#headers", "#variables"]
            );
            assert!(result.automatic);
        }
        for body in [
            "query Q {\n",
            "query Q { viewer {\n",
            "query Q($filter: Input = { nested: {} }\n",
        ] {
            let source = format!("{prefix}{body}");
            let result = suggest(
                "graphql",
                "",
                &source,
                source.split('\n').count() - 1,
                0,
                &[],
                &Value::Null,
            );
            assert!(
                !result
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text.starts_with('#'))
            );
        }
    }
    #[test]
    fn markers_advance_to_their_value_but_request_leaves_room_for_a_title() {
        assert_eq!(
            prepare_insertion("rest", "#check", true),
            ("#check ".into(), None)
        );
        for service in console::SERVICES {
            for marker in console::markers(service) {
                let marker = format!("#{marker}");
                assert_eq!(
                    prepare_insertion("rest", &marker, true),
                    (format!("{marker}\n"), None)
                );
            }
        }
        assert_eq!(
            prepare_insertion("rest", "#endpoint", false),
            ("#endpoint".into(), None)
        );
    }
    #[test]
    fn completed_candidates_are_not_reoffered() {
        let cases = [
            (
                "graphql",
                "query",
                "{\n  name",
                json!({"queryType":"Query","types":{"Query":{"fields":[{"name":"name","type":"String"}]}}}),
                vec![],
            ),
            (
                "grpc",
                "endpointText",
                "grpc://localhost:50051/demo.Greeter/Hello",
                json!({"methods":["Hello"]}),
                vec![],
            ),
            (
                "mysql",
                "sql",
                "SELECT * FROM users u WHERE u.email",
                json!({"tables":[{"name":"users","columns":[{"name":"email"}]}]}),
                vec![],
            ),
            (
                "rest",
                "headersText",
                "X-Trace: fixture",
                Value::Null,
                vec![("X-Trace: fixture".into(), "Saved".into())],
            ),
            ("mysql", "sql", "select", Value::Null, vec![]),
        ];
        for (service, field, source, discovery, hints) in cases {
            let line = source.split('\n').count() - 1;
            let ch = source
                .split('\n')
                .next_back()
                .unwrap()
                .encode_utf16()
                .count();
            assert!(
                suggest(service, field, source, line, ch, &hints, &discovery)
                    .candidates
                    .is_empty(),
                "{service}: {source}"
            );
        }
    }
    #[test]
    fn case_sensitive_names_can_still_be_corrected() {
        let d = json!({"queryType":"Query","types":{"Query":{"fields":[{"name":"name","type":"String"}]}}});
        let r = suggest("graphql", "query", "{\n  Name", 1, 6, &[], &d);
        assert_eq!(r.candidates[0].text, "name");
        let d = json!({"paths":["/Users"]});
        let source = "https://fixture.invalid/users";
        let r = suggest("rest", "url", source, 0, source.len(), &[], &d);
        assert_eq!(r.candidates[0].text, "/Users");
    }
    #[test]
    fn nested_graphql_snippets_preserve_indent_and_place_cursor_inside() {
        let d = json!({"queryType":"Query","types":{
            "Query":{"fields":[{"name":"viewer","type":"User"}]},
            "User":{"fields":[{"name":"address","type":"Address"}]},
            "Address":{"fields":[{"name":"city","type":"String"}]}
        }});
        for indent in ["    ", "\t\t"] {
            let source = format!("{{\n  viewer {{\n{indent}ad");
            let r = suggest("graphql", "query", &source, 2, indent.len() + 2, &[], &d);
            assert_eq!(r.candidates[0].text, "address");
            let source = format!("{{\n  viewer {{\n{indent}address ");
            let r = suggest("graphql", "query", &source, 2, indent.len() + 8, &[], &d);
            let expected = format!("{{\n{indent}  \n{indent}}}");
            assert_eq!(r.candidates.len(), 3);
            assert_eq!(r.candidates[0].text, expected);
            assert_eq!(
                prepare_insertion("graphql", &expected, false),
                (expected.clone(), Some(format!("{{\n{indent}  ").len()))
            );
        }
    }
    #[test]
    fn graphql_variables_offer_only_json_templates() {
        let hints = [
            "query",
            "mutation",
            "subscription",
            "fragment",
            "__typename",
            "products",
            "skus",
            "https://fixture.invalid/graphql",
        ]
        .map(|text| (text.into(), "GraphQL".into()));
        let discovery = json!({
            "queryType":"Query",
            "types":{"Query":{"fields":[{"name":"products","type":"String"}]}},
            "fields":[{"name":"skus","kind":"array"}],
            "bodySchemas":{"GET /":[{"name":"products","kind":"string"}]}
        });
        for (field, prefix) in [
            ("variables", ""),
            ("", "#check g\n#variables\n"),
            ("", "#variables\n"),
        ] {
            let cases: &[(&str, &[&str])] = &[
                ("", &["JSON object"]),
                ("  ", &["JSON object"]),
                ("query", &[]),
                ("pro", &[]),
                ("{\n  ", &["JSON property"]),
                ("{\n  \"sk", &[]),
                ("{\"skus\": ", &["JSON string", "JSON object", "JSON array"]),
                (
                    "{\"skus\": [",
                    &["JSON string", "JSON object", "JSON array"],
                ),
                (
                    "{\"skus\": [\"fixture\", ",
                    &["JSON string", "JSON object", "JSON array"],
                ),
                ("{\"skus\": [{", &["JSON property"]),
                (
                    "{\"skus\": [[",
                    &["JSON string", "JSON object", "JSON array"],
                ),
                (
                    "{\"a\": 1, \"b\": true, \"c\": false, \"d\": null, ",
                    &["JSON property"],
                ),
                ("{\"skus\": [\"fixture\"]}", &[]),
                ("{\"skus\": [\"fixture\"]}\n", &[]),
                ("{\"skus\": \"", &[]),
                ("{\"skus\": \"brace } colon : comma , [ escaped \\\"", &[]),
                ("{\"skus\"", &[]),
                ("{\"skus\": tru", &[]),
                ("{\"skus\": 123", &[]),
                ("{\"skus\": true ", &[]),
                ("{\"skus\": [}", &[]),
                ("{\"skus\" \"", &[]),
                ("{\"skus\": : ", &[]),
                ("{\"skus\": nope, ", &[]),
                ("[]", &[]),
            ];
            for &(body, details) in cases {
                let head = format!("{prefix}{body}");
                let line = head.split('\n').count() - 1;
                let before = head.split('\n').next_back().unwrap();
                let source = if field.is_empty() {
                    format!("{head}\n#query\nquery {{ products }}")
                } else {
                    head.clone()
                };
                let result = suggest(
                    "graphql",
                    field,
                    &source,
                    line,
                    before.encode_utf16().count(),
                    &hints,
                    &discovery,
                );
                assert_eq!(
                    result
                        .candidates
                        .iter()
                        .map(|c| c.detail.as_str())
                        .collect::<Vec<_>>(),
                    details,
                    "{source}"
                );
                assert_eq!(result.from, before.encode_utf16().count());
                assert_eq!(result.to, result.from);
                for candidate in result.candidates {
                    let expected = match candidate.detail.as_str() {
                        "JSON object" => object_snippet(before),
                        "JSON array" => block_snippet(before, '[', ']'),
                        _ => "\"\"".into(),
                    };
                    assert_eq!(candidate.text, expected, "{source}");
                    assert!(
                        prepare_insertion("graphql", &candidate.text, field.is_empty())
                            .1
                            .is_some()
                    );
                }
            }
        }
    }
    #[test]
    fn graphql_json_templates_preserve_existing_values_and_delimiters() {
        for (marked, count) in [
            ("{|}", 1),
            ("{\n  |\"existing\": 1\n}", 0),
            ("{\"😀имя\": |}", 3),
            ("{\"key\": |\"existing\"}", 0),
            ("{\"key\": |false}", 0),
            ("{\"key\": |{}}", 0),
            ("{\"key\": \n  |[]}", 0),
            ("{\"key\": [|]}", 3),
            ("{\"key\": [|\"existing\"]}", 0),
            ("|{}", 0),
            ("{\"a\": \"\\\"}:,[\", |}", 1),
        ] {
            for (field, prefix) in [("variables", ""), ("", "#check g\n#variables\n\n")] {
                let offset = prefix.len() + marked.find('|').unwrap();
                let source = format!("{prefix}{}", marked.replace('|', ""));
                let source = if field.is_empty() {
                    format!("{source}\n#query\nquery {{ products }}")
                } else {
                    source
                };
                let before = &source[..offset];
                let line = before.bytes().filter(|c| *c == b'\n').count();
                let ch = before.rsplit('\n').next().unwrap().encode_utf16().count();
                let result = suggest("graphql", field, &source, line, ch, &[], &Value::Null);
                assert_eq!(result.candidates.len(), count, "{source}");
                assert_eq!((result.from, result.to), (ch, ch));
                if count == 3 {
                    for candidate in result.candidates {
                        let mut inserted = source.clone();
                        inserted.insert_str(offset, &candidate.text);
                        let body = inserted
                            .strip_prefix(prefix)
                            .unwrap()
                            .split("\n#query")
                            .next()
                            .unwrap();
                        assert!(serde_json::from_str::<Value>(body).is_ok(), "{inserted}");
                    }
                }
            }
        }
    }
    #[test]
    fn json_object_snippets_preserve_indent_and_value_cursor_positions() {
        let d = json!({"fields":[{"name":"address","kind":"object"}]});
        let r = suggest("grpc", "requestMessage", "{\n    ad", 1, 6, &[], &d);
        assert_eq!(r.candidates[0].text, "\"address\": {\n      \n    }");
        for text in ["\"city\": \"\"", "\"values\": []", "\"имя\": \"\""] {
            assert_eq!(
                prepare_insertion("grpc", text, false),
                (text.into(), Some(text.len() - 1))
            );
        }
        for service in ["grpc", "graphql"] {
            let field = if service == "grpc" {
                "requestMessage"
            } else {
                "query"
            };
            let r = suggest(service, field, "    ", 0, 4, &[], &Value::Null);
            let index = if service == "graphql" {
                assert_eq!(
                    r.candidates[..3]
                        .iter()
                        .map(|c| c.text.as_str())
                        .collect::<Vec<_>>(),
                    ["query", "mutation", "subscription"]
                );
                assert_eq!(r.candidates[3].text, "fragment");
                4
            } else {
                0
            };
            assert_eq!(r.candidates[index].text, "{\n      \n    }");
        }
    }
    #[test]
    fn splunk_endpoint_only_offers_endpoints_not_headers_or_searches() {
        let hints = vec![
            ("Authorization: fixture".into(), "Header".into()),
            ("index=http".into(), "SPL".into()),
            ("https://splunk.example.test".into(), "Endpoint".into()),
        ];
        let r = suggest("splunk", "baseUrl", "http", 0, 4, &hints, &Value::Null);
        assert!(!r.candidates.is_empty());
        assert!(r.candidates.iter().all(|c| c.text.contains("://")));
        assert!(
            r.candidates
                .iter()
                .any(|c| c.text == "https://splunk.example.test")
        );
    }
    #[test]
    fn placeholders_replace_existing_close() {
        let r = suggest(
            "rest",
            "body",
            "{{:TOK}}",
            0,
            6,
            &[("{{:TOKEN}}".into(), "value".into())],
            &Value::Null,
        );
        assert_eq!(r.to, 8);
        assert_eq!(r.from, 0);
        assert_eq!(r.candidates[0].text, "{{:TOKEN}}");
    }
    #[test]
    fn grpc_nested_fields_not_ancestors() {
        let d = json!({"fields":[{"name":"address","kind":"object","fields":[{"name":"city","kind":"string"}]},{"name":"other","kind":"string"}]});
        let r = suggest(
            "grpc",
            "requestMessage",
            "{\"address\": {\n  ",
            1,
            2,
            &[],
            &d,
        );
        assert_eq!(r.candidates[0].text, "\"city\": \"\"");
        assert_eq!(r.candidates.len(), 1);
    }
    #[test]
    fn grpc_parent_fields_after_a_closed_object_offer_the_missing_comma() {
        let d = json!({"fields":[
            {"name":"products_by_store_view","kind":"array","items":{"kind":"object","fields":[
                {"name":"store_view_id","kind":"object","fields":[{"name":"store_code","kind":"string"}]},
                {"name":"skus","kind":"array","items":{"kind":"string"}}
            ]}}
        ]});
        for (field, prefix) in [("requestMessage", ""), ("", "#check comma\n#message\n")] {
            let body = "{\n  \"products_by_store_view\": [{\n    \"store_view_id\": {\n      \"store_code\": \"{{:store_code}}\"\n    }\n    ";
            let source = format!("{prefix}{body}\n  }}]\n}}");
            let line = prefix.lines().count() + 5;
            let result = suggest("grpc", field, &source, line, 4, &[], &d);
            assert_eq!(
                result
                    .candidates
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>(),
                ["\"skus\": []"]
            );
            assert_eq!(result.comma, Some((line - 1, 5)));
            let source =
                format!("{prefix}{{\"products_by_store_view\":[{{\"store_view_id\":{{}},\n    ");
            let line = prefix.lines().count() + 1;
            let result = suggest("grpc", field, &source, line, 4, &[], &d);
            assert_eq!(result.candidates[0].text, "\"skus\": []");
            assert_eq!(result.comma, None);
            for body in [
                "{\"products_by_store_view\": []}\n  ",
                "{\"products_by_store_view\": [{\"store_view_id\": \n  ",
            ] {
                let source = format!("{prefix}{body}");
                let line = source.lines().count() - 1;
                let result = suggest("grpc", field, &source, line, 2, &[], &d);
                assert!(result.candidates.is_empty(), "{source}");
                assert_eq!(result.comma, None);
            }
        }
    }
    #[test]
    fn grpc_array_items_follow_the_schema_and_isolate_each_objects_fields() {
        let d = json!({"fields":[
            {"name":"products_by_store_view","kind":"array","items":{"kind":"object","fields":[
                {"name":"store_view_code","kind":"string"},
                {"name":"products","kind":"array","items":{"kind":"object","fields":[
                    {"name":"sku","kind":"string"},
                    {"name":"tags","kind":"array","items":{"kind":"string"}}
                ]}}
            ]}},
            {"name":"root","kind":"string"},
            {"name":"tags","kind":"array","items":{"kind":"string"}},
            {"name":"flags","kind":"array","items":{"kind":"boolean"}},
            {"name":"ids","kind":"array","items":{"kind":"number"}},
            {"name":"unknown","kind":"array"}
        ]});
        let cases: &[(&str, &[&str])] = &[
            (
                "{\n  \"products_by_store_view\": [\n    |",
                &["{\n      \n    }"],
            ),
            ("{\"products_by_store_view\": [|]}", &["{\n  \n}"]),
            (
                "{\"products_by_store_view\": [{\n  |",
                &["\"store_view_code\": \"\"", "\"products\": []"],
            ),
            (
                "{\"products_by_store_view\": [{\"store_view_code\": \"en\"\n  |",
                &["\"products\": []"],
            ),
            (
                "{\"products_by_store_view\": [{\"store_view_code\": \"en\"}, {\n  |",
                &["\"store_view_code\": \"\"", "\"products\": []"],
            ),
            (
                "{\"products_by_store_view\": [{\"products\": [{\n  |",
                &["\"sku\": \"\"", "\"tags\": []"],
            ),
            (
                "{\"products_by_store_view\": [{\"products\": [{\"tags\": [|",
                &["\"\""],
            ),
            (
                "{\"products_by_store_view\": [{\"products\": [{\"sku\":\"x\"}],\n  |",
                &["\"store_view_code\": \"\""],
            ),
            (
                "{\"products_by_store_view\": [{\"store_view_code\":\"😀\"}],\n  ro|",
                &["\"root\": \"\""],
            ),
            ("{\"tags\": [|", &["\"\""]),
            (
                "{\"tags\": [\"comma, brace } escape \\\" 😀\", |",
                &["\"\""],
            ),
            ("{\"flags\": [|", &["true", "false"]),
            ("{\"ids\": [|", &["0"]),
            ("{\"unknown\": [|", &[]),
            ("{\"unknown\": [{\n  |", &[]),
            ("{\"products_by_store_view\": [|{}]}", &[]),
            (
                "{\"products_by_store_view\": [{\"store_view_code\": \"|",
                &[],
            ),
            ("{\"products_by_store_view\": [\"unfinished, |", &[]),
            ("{\"products_by_store_view\": [tru|", &[]),
            ("{\"products_by_store_view\": [}|", &[]),
            ("{\"tags\": []}|", &[]),
            ("{\"tags\": []} {|", &[]),
        ];
        for (field, prefix) in [
            ("requestMessage", ""),
            (
                "",
                "#check array\n#endpoint\ngrpcs://{{catalog_domain:cds_paas1}}/ProductService/GetProducts\n#message\n",
            ),
        ] {
            for (body, expected) in cases {
                let (before, after) = body.split_once('|').unwrap();
                let before = format!("{prefix}{before}");
                let source = format!("{before}{after}");
                let line = before.split('\n').count() - 1;
                let ch = before
                    .split('\n')
                    .next_back()
                    .unwrap()
                    .encode_utf16()
                    .count();
                let result = suggest("grpc", field, &source, line, ch, &[], &d);
                assert_eq!(
                    result
                        .candidates
                        .iter()
                        .map(|c| c.text.as_str())
                        .collect::<Vec<_>>(),
                    *expected,
                    "{field}: {body}"
                );
            }
        }
    }
    #[test]
    fn graphql_type_resolution() {
        let d = json!({"queryType":"Query","types":{"Query":{"fields":[{"name":"viewer","type":"User"}]},"User":{"fields":[{"name":"id","type":"ID"},{"name":"name","type":"String"}]}}});
        let r = suggest("graphql", "query", "query { viewer {\n na", 1, 3, &[], &d);
        assert_eq!(r.candidates[0].text, "name");
    }
    #[test]
    fn mysql_alias_scope() {
        let d = json!({"tables":[{"name":"users","columns":[{"name":"id"},{"name":"email"}]},{"name":"orders","columns":[{"name":"total"}]}]});
        let s = "SELECT * FROM users u WHERE u.em";
        let r = suggest("mysql", "sql", s, 0, s.len(), &[], &d);
        assert_eq!(r.candidates[0].text, "email");
    }
    #[test]
    fn profiles_then_scoped_variables() {
        let hints = vec![
            ("{{prod:TOKEN}}".into(), "secret".into()),
            ("{{other:HOST}}".into(), "host".into()),
            ("{{:TOKEN}}".into(), "global".into()),
        ];
        for (service, service_field) in [
            ("mysql", "sql"),
            ("rest", "url"),
            ("graphql", "query"),
            ("grpc", "requestMessage"),
            ("splunk", "search"),
            ("terminal", "command"),
            ("vars", "variables"),
        ] {
            for field in [service_field, ""] {
                let profiles = suggest(service, field, "{{", 0, 2, &hints, &Value::Null);
                assert!(profiles.candidates.iter().any(|c| c.text == "{{prod:"));
                assert!(profiles.candidates.iter().any(|c| c.text == "{{:"));
                assert!(!profiles.candidates.iter().any(|c| c.text.ends_with("}}")));
                let variables = suggest(service, field, "{{prod:TK", 0, 9, &hints, &Value::Null);
                assert_eq!(variables.candidates.len(), 1);
                assert_eq!(variables.candidates[0].text, "{{prod:TOKEN}}");
            }
        }
    }
    #[test]
    fn rest_post_fields_match_a_trailing_slash_after_an_open_quote() {
        let discovery = json!({"bodySchemas":{"POST /api/v1/Books":[
            {"name":"id","kind":"bare"},{"name":"title","kind":"string"},
            {"name":"description","kind":"string"},{"name":"pageCount","kind":"bare"},
            {"name":"excerpt","kind":"string"},{"name":"publishDate","kind":"string"}
        ],"GET /api/v1/Books":[{"name":"wrongMethod","kind":"string"}]}});
        for suffix in ["", "/"] {
            let prefix = format!(
                "#check test1\n#endpoint\nPOST https://fakerestapi.azurewebsites.net/api/v1/Books{suffix}\n#headers\nContent-Type: application/json\nAccept: application/json\n#body\n"
            );
            for body in ["{\n  ", "{\n  \""] {
                let source = format!("{prefix}{body}");
                let result = suggest(
                    "rest",
                    "",
                    &source,
                    8,
                    source.lines().last().unwrap().encode_utf16().count(),
                    &[],
                    &discovery,
                );
                assert_eq!(
                    result
                        .candidates
                        .iter()
                        .map(|candidate| candidate.text.as_str())
                        .collect::<Vec<_>>(),
                    vec![
                        "\"id\": ",
                        "\"title\": \"\"",
                        "\"description\": \"\"",
                        "\"pageCount\": ",
                        "\"excerpt\": \"\"",
                        "\"publishDate\": \"\""
                    ],
                    "{suffix:?}: {body:?}"
                );
            }
            let mut form = discovery.clone();
            form["requestEndpoint"] = json!(format!(
                "POST https://fakerestapi.azurewebsites.net/api/v1/Books{suffix}"
            ));
            assert_eq!(
                suggest("rest", "body", "{\n  \"", 1, 3, &[], &form)
                    .candidates
                    .len(),
                6
            );
        }
    }
    #[test]
    fn rest_empty_body_offers_a_required_only_json_template() {
        let fields = json!([
            {"name":"name","kind":"string","required":true},
            {"name":"count","kind":"bare","type":"integer","required":true},
            {"name":"enabled","kind":"bare","type":"boolean","required":true},
            {"name":"tags","kind":"array","required":true},
            {"name":"settings","kind":"object","required":true,"fields":[
                {"name":"locale","kind":"string","required":true},
                {"name":"optionalNested","kind":"string"}
            ]},
            {"name":"optional","kind":"string"},
            {"name":"readOnly","kind":"string","required":true,"readOnly":true}
        ]);
        let discovery = json!({
            "requestEndpoint":"POST http://{{var:HOST}}/token",
            "bodySchemas":{"POST /token":fields}
        });
        let hints = vec![("{{var:HOST}}".into(), "fixture.invalid".into())];
        for field in ["", "body"] {
            let source = if field.is_empty() {
                "#check token\n#endpoint\nPOST http://{{var:HOST}}/token\n#body\n  "
            } else {
                "  "
            };
            let result = suggest(
                "rest",
                field,
                source,
                if field.is_empty() { 4 } else { 0 },
                2,
                &hints,
                &discovery,
            );
            assert_eq!(result.candidates.len(), 2);
            let template = &result.candidates[0];
            assert_eq!(template.detail, "Required fields");
            assert_eq!(
                serde_json::from_str::<Value>(&template.text).unwrap(),
                json!({
                    "name":"","count":0,"enabled":false,"tags":[],"settings":{"locale":""}
                })
            );
            assert!(template.text.contains("\n    \""));
            assert_eq!(result.candidates[1].text, "{\n    \n  }");
            let (text, cursor) = prepare_insertion("rest", &template.text, field.is_empty());
            assert_eq!(&text[cursor.unwrap() - 1..cursor.unwrap() + 1], "\"\"");
        }
        let existing = suggest(
            "rest",
            "body",
            "\n{\"name\":\"kept\"}",
            0,
            0,
            &hints,
            &discovery,
        );
        assert!(
            !existing
                .candidates
                .iter()
                .any(|c| c.detail == "Required fields"),
            "A template must not be offered in front of an existing body"
        );
        for discovery in [
            json!({"requestEndpoint":"GET http://fixture.invalid/token","bodySchemas":{"POST /token":fields}}),
            json!({"requestEndpoint":"POST http://fixture.invalid/token","bodySchemas":{"POST /token":[{"name":"optional","kind":"string"}]}}),
            json!({"warning":"No schema"}),
        ] {
            let result = suggest("rest", "body", "", 0, 0, &[], &discovery);
            assert_eq!(result.candidates.len(), 1);
            assert_eq!(result.candidates[0].detail, "JSON object");
        }
        let result = suggest("rest", "body", "{\n  ", 1, 2, &hints, &discovery);
        assert!(
            result
                .candidates
                .iter()
                .any(|c| c.text == "\"optional\": \"\"")
        );
        assert!(
            !result
                .candidates
                .iter()
                .any(|c| c.text.contains("readOnly"))
        );
        assert!(
            !result
                .candidates
                .iter()
                .any(|c| c.detail == "Required fields")
        );
    }
    #[test]
    fn rest_body_fields_resolve_variable_urls_in_console_and_form() {
        let discovery = json!({"bodySchemas":{
            "POST /":[{"name":"rootOnly","kind":"string"}],
            "POST /rest/default/V1/integration/admin/token":[
                {"name":"username","kind":"string"},
                {"name":"password","kind":"string"}
            ],
            "GET /rest/default/V1/integration/admin/token":[
                {"name":"wrongMethod","kind":"string"}
            ]
        }});
        let hints = crate::profiles::hints(&json!({
            "order":["var"],
            "profiles":{"var":{
                "magento":"store.fixture.invalid",
                "base":"http://store.fixture.invalid",
                "store":"default",
                "token":"http://store.fixture.invalid/rest/default/V1/integration/admin/token"
            }},
            "env":{"MAGENTO":"http://store.fixture.invalid"}
        }));
        for endpoint in [
            "http://{{var:magento}}/rest/default/V1/integration/admin/token",
            "http://{{:magento}}/rest/default/V1/integration/admin/token",
            "http://{{var:magento}}/rest/{{var:store}}/V1/integration/admin/token",
            "{{var:base}}/rest/default/V1/integration/admin/token",
            "{{env:MAGENTO}}/rest/default/V1/integration/admin/token",
            "{{var:token}}",
        ] {
            for suffix in ["", "/"] {
                let endpoint = format!("{endpoint}{suffix}");
                for body in ["{\n  ", "{\n  \""] {
                    let source = format!(
                        "#check magento\n#endpoint\nPOST {endpoint}\n#headers\nContent-Type: application/json\n#body\n{body}"
                    );
                    let ch = body.lines().last().unwrap().encode_utf16().count();
                    let console = suggest("rest", "", &source, 7, ch, &hints, &discovery);
                    let mut form_discovery = discovery.clone();
                    form_discovery["requestEndpoint"] = json!(format!("POST {endpoint}"));
                    let form = suggest("rest", "body", body, 1, ch, &hints, &form_discovery);
                    for result in [console, form] {
                        assert_eq!(
                            result
                                .candidates
                                .iter()
                                .map(|c| c.text.as_str())
                                .collect::<Vec<_>>(),
                            vec!["\"username\": \"\"", "\"password\": \"\""],
                            "{endpoint}: {body:?}"
                        );
                    }
                }
            }
        }
        for endpoint in [
            "http://{{missing:magento}}/rest/default/V1/integration/admin/token",
            "{{var:base",
        ] {
            let mut discovery = discovery.clone();
            discovery["requestEndpoint"] = json!(format!("POST {endpoint}"));
            let result = suggest("rest", "body", "{\n  ", 1, 2, &hints, &discovery);
            assert!(
                result.candidates.is_empty(),
                "An unresolved URL must not borrow a schema from a previous connection"
            );
        }
    }
    #[test]
    fn rest_body_schema_prefers_exact_paths_and_requires_nonempty_parameters() {
        let discovery = json!({"bodySchemas":{
            "POST /api/v1/Books":[{"name":"book","kind":"string"}],
            "POST /api/v1/Books/":[{"name":"slashBook","kind":"string"}],
            "POST /api/v1/Books/{id}":[{"name":"byId","kind":"string"}]
        }});
        for (path, field) in [
            ("Books", "book"),
            ("Books/", "slashBook"),
            ("Books/42/", "byId"),
        ] {
            let source = format!(
                "#check test1\n#endpoint\nPOST https://fixture.invalid/api/v1/{path}\n#body\n{{\n  "
            );
            let result = suggest("rest", "", &source, 5, 2, &[], &discovery);
            assert_eq!(result.candidates[0].text, format!("\"{field}\": \"\""));
        }
        let mut discovery = discovery;
        discovery["bodySchemas"]
            .as_object_mut()
            .unwrap()
            .remove("POST /api/v1/Books/");
        let source =
            "#check test1\n#endpoint\nPOST https://fixture.invalid/api/v1/Books/\n#body\n{\n  ";
        assert_eq!(
            suggest("rest", "", source, 5, 2, &[], &discovery).candidates[0].text,
            "\"book\": \"\""
        );
    }
    #[test]
    fn rest_enum_fields_are_labeled_and_continue_with_value_choices() {
        let discovery = json!({"requestEndpoint":"POST https://fixture.invalid/item","bodySchemas":{
            "POST /item":[
                {"name":"status","kind":"string","type":"string","enum":["pending","active"]},
                {"name":"priority","kind":"bare","type":"integer","enum":[1,2]},
                {"name":"mixed","kind":"string","enum":[true,null,1,"one"]},
                {"name":"title","kind":"string"}
            ]
        }});
        let result = suggest("rest", "body", "{\n  ", 1, 2, &[], &discovery);
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|candidate| (candidate.detail.as_str(), candidate.continue_hint))
                .collect::<Vec<_>>(),
            vec![
                ("string · enum", true),
                ("integer · enum", true),
                ("enum", true),
                ("string", false)
            ]
        );
        let template = required_body(&json!([
            {"name":"status","kind":"string","required":true,"enum":["pending","active"]},
            {"name":"priority","kind":"bare","type":"integer","required":true,"enum":[2,3]},
            {"name":"value","kind":"string","required":true,"enum":[null,true]}
        ]))
        .unwrap();
        assert_eq!(
            template,
            json!({"status":"pending","priority":2,"value":null})
        );
        assert!(
            required_body(
                &json!([{"name":"impossible","kind":"string","required":true,"enum":[]}])
            )
            .is_none()
        );
    }
    #[test]
    fn rest_enum_selection_replaces_complete_values_with_valid_json() {
        let values = json!(["pending","active","ки😀\"\\",1,true,null,["a"],{"key":"value"}]);
        let discovery = json!({"requestEndpoint":"POST https://fixture.invalid/item","bodySchemas":{
            "POST /item":[{"name":"status","kind":"string","enum":values},{"name":"title","kind":"string"}]
        }});
        for marked in [
            "{\"status\": |, \"title\":\"kept\"}",
            "{\"status\": \"|\", \"title\":\"kept\"}",
            "{\"status\": \"ac|tive\", \"title\":\"kept\"}",
            "{\"status\": t|rue, \"title\":\"kept\"}",
            "{\"status\": |null, \"title\":\"kept\"}",
            "{\"status\": \"ки😀\\\"|\\\\\", \"title\":\"kept\"}",
        ] {
            let cursor = marked.find('|').unwrap();
            let source = marked.replacen('|', "", 1);
            let result = suggest(
                "rest",
                "body",
                &source,
                0,
                source[..cursor].encode_utf16().count(),
                &[],
                &discovery,
            );
            assert_eq!(
                result.candidates.len(),
                values.as_array().unwrap().len(),
                "{marked}"
            );
            let byte_at = |count| utf16_prefix(&source, count).len();
            for candidate in result.candidates {
                let mut edited = source.clone();
                edited.replace_range(byte_at(result.from)..byte_at(result.to), &candidate.text);
                let parsed: Value = serde_json::from_str(&edited).unwrap();
                assert_eq!(parsed["title"], "kept");
                assert!(values.as_array().unwrap().contains(&parsed["status"]));
                assert!(!candidate.continue_hint);
            }
        }
    }
    #[test]
    fn rest_enum_partial_values_filter_and_keep_adjacent_fields() {
        let discovery = json!({"requestEndpoint":"POST https://fixture.invalid/item","bodySchemas":{
            "POST /item":[{"name":"status","kind":"string","enum":["pending","active"]}]
        }});
        for marked in [
            "{\"status\": \"pe|\", \"other\":0}",
            "{\"status\": pe|, \"other\":0}",
            "{\"status\": \"pe|",
        ] {
            let cursor = marked.find('|').unwrap();
            let source = marked.replacen('|', "", 1);
            let result = suggest("rest", "body", &source, 0, cursor, &[], &discovery);
            assert_eq!(
                result
                    .candidates
                    .iter()
                    .map(|candidate| candidate.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["\"pending\""]
            );
            assert_eq!(result.from, 11);
        }
    }
    #[test]
    fn rest_enum_values_work_in_nested_array_items_and_console_sections() {
        let discovery = json!({"requestEndpoint":"POST https://fixture.invalid/item","bodySchemas":{
            "POST /item":[{"name":"entries","kind":"array","items":{"kind":"object","fields":[
                {"name":"status","kind":"string","enum":["pending","active"]},
                {"name":"tags","kind":"array","items":{"kind":"string","enum":["red","blue"]}}
            ]}}]
        }});
        for (body, expected) in [
            (
                "{\"entries\":[{\"status\": ",
                vec!["\"pending\"", "\"active\""],
            ),
            ("{\"entries\":[{\"tags\":[", vec!["\"red\"", "\"blue\""]),
            (
                "{\"entries\":[{\"tags\":[\"red\", ",
                vec!["\"red\"", "\"blue\""],
            ),
        ] {
            for field in ["", "body"] {
                let source = if field.is_empty() {
                    format!(
                        "#check enums\n#endpoint\nPOST https://fixture.invalid/item\n#body\n{body}"
                    )
                } else {
                    body.into()
                };
                let result = suggest(
                    "rest",
                    field,
                    &source,
                    source.lines().count() - 1,
                    body.encode_utf16().count(),
                    &[],
                    &discovery,
                );
                assert_eq!(
                    result
                        .candidates
                        .iter()
                        .map(|candidate| candidate.text.as_str())
                        .collect::<Vec<_>>(),
                    expected
                );
            }
        }
    }
    #[test]
    fn rest_scalar_hints_show_actual_types_instead_of_internal_insertion_kinds() {
        let discovery = json!({"requestEndpoint":"POST https://fixture.invalid/item","bodySchemas":{
            "POST /item":[
                {"name":"count","kind":"bare","type":"integer"},
                {"name":"price","kind":"bare","type":"number"},
                {"name":"enabled","kind":"bare","type":"boolean"},
                {"name":"id","kind":"bare","type":"integer","format":"int32"},
                {"name":"legacy","kind":"bare"}
            ]
        }});
        for field in ["", "body"] {
            let source = if field.is_empty() {
                "#check types\n#endpoint\nPOST https://fixture.invalid/item\n#body\n{\n  \""
            } else {
                "{\n  \""
            };
            let result = suggest(
                "rest",
                field,
                source,
                source.lines().count() - 1,
                3,
                &[],
                &discovery,
            );
            assert_eq!(
                result
                    .candidates
                    .iter()
                    .map(|candidate| (candidate.text.as_str(), candidate.detail.as_str()))
                    .collect::<Vec<_>>(),
                vec![
                    ("\"count\": ", "integer"),
                    ("\"price\": ", "number"),
                    ("\"enabled\": ", "boolean"),
                    ("\"id\": ", "int32"),
                    ("\"legacy\": ", "value (type unavailable)")
                ]
            );
        }
    }
    #[test]
    fn rest_date_hints_show_the_schema_format_without_choosing_a_value() {
        let discovery = json!({"requestEndpoint":"POST https://fixture.invalid/api/v1/Books/","bodySchemas":{
            "POST /api/v1/Books":[
                {"name":"publishDate","kind":"string","format":"date-time"},
                {"name":"startDate","kind":"string","format":"date"},
                {"name":"title","kind":"string"}
            ]
        }});
        let result = suggest("rest", "body", "{\n  \"", 1, 3, &[], &discovery);
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|candidate| (candidate.text.as_str(), candidate.detail.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("\"publishDate\": \"\"", "date-time (ISO 8601)"),
                ("\"startDate\": \"\"", "date (YYYY-MM-DD)"),
                ("\"title\": \"\"", "string")
            ]
        );
    }
    #[test]
    fn rest_nested_schema_uses_operation_and_path() {
        let d = json!({"requestEndpoint":"POST https://example.test/users/42","bodySchemas":{
            "GET /users/{id}":[{"name":"incorrect","kind":"string"}],
            "POST /users/{id}":[{"name":"address","kind":"object","fields":[{"name":"city","kind":"string"},{"name":"zip","kind":"bare"}]},{"name":"topLevel","kind":"bare"}]
        }});
        let source = "{\"address\": {\n  \"city\": \"Paris\"\n  zi";
        let r = suggest("rest", "body", source, 2, 4, &[], &d);
        assert_eq!(r.candidates.len(), 1);
        assert_eq!(r.candidates[0].text, "\"zip\": ");
        assert_eq!(r.comma, Some((1, 17)));
    }
    #[test]
    fn json_completions_do_not_interrupt_values() {
        let d = json!({"fields":[{"name":"flag","kind":"bare"}]});
        for source in ["{\"value\": ", "{\"value\": 42 f", "{\"value\": {}}\n"] {
            let line = source.split('\n').count() - 1;
            let ch = source.split('\n').next_back().unwrap().len();
            assert!(
                suggest("grpc", "requestMessage", source, line, ch, &[], &d)
                    .candidates
                    .is_empty(),
                "{source}"
            );
        }
    }
    #[test]
    fn rest_path_completion_after_correcting_host_reuses_existing_slash() {
        let path = "/api/pets";
        let discovery = json!({"paths":[path],"methodsByPath":{path:["GET"]}});
        for (field, prefix) in [
            ("", "GET https://fixture.invalid"),
            ("", "GET \u{a0}https://пример.рф"),
            ("url", "https://fixture.invalid"),
        ] {
            let endpoint = format!("{prefix}/api/");
            let source = if field.is_empty() {
                format!("#endpoint\n{endpoint}")
            } else {
                endpoint.clone()
            };
            let result = suggest(
                "rest",
                field,
                &source,
                usize::from(field.is_empty()),
                prefix.encode_utf16().count(),
                &[],
                &discovery,
            );
            assert_eq!(result.candidates.len(), 1, "{endpoint}");
            assert_eq!(result.from, prefix.encode_utf16().count());
            assert_eq!(result.to, endpoint.encode_utf16().count());
            let mut units = endpoint.encode_utf16().collect::<Vec<_>>();
            units.splice(
                result.from..result.to,
                result.candidates[0].text.encode_utf16(),
            );
            assert_eq!(
                String::from_utf16(&units).unwrap(),
                format!("{prefix}{path}")
            );
        }
    }
    #[test]
    fn rest_path_completion_preserves_method_spacing_and_entire_host() {
        let path = "/api/v3/pet/findByStatus";
        let discovery = json!({"paths":[path],"methodsByPath":{path:["GET"]}});
        for spacing in [" ", "  ", "\t", "\u{a0}", " \u{a0}", "\u{2003}\t"] {
            let prefix = format!("GET{spacing}https://petstore3.swagger.io");
            let endpoint = format!("{prefix}/api/v3/");
            let source = format!("#check test1\n#endpoint\n{endpoint}");
            let result = suggest(
                "rest",
                "",
                &source,
                2,
                endpoint.encode_utf16().count(),
                &[],
                &discovery,
            );
            assert_eq!(result.candidates.len(), 1, "{spacing:?}");
            assert_eq!(result.from, prefix.encode_utf16().count(), "{spacing:?}");
            let mut units = endpoint.encode_utf16().collect::<Vec<_>>();
            units.splice(
                result.from..result.to,
                result.candidates[0].text.encode_utf16(),
            );
            assert_eq!(
                String::from_utf16(&units).unwrap(),
                format!("{prefix}{path}"),
                "{spacing:?}"
            );
        }
    }
    #[test]
    fn endpoint_completion_offsets_use_utf16_after_trimming() {
        for (service, field, endpoint, expected, discovery) in [
            (
                "rest",
                "url",
                "\u{2003}https://пример.рф/api/",
                "\u{2003}https://пример.рф/api/pets",
                json!({"paths":["/api/pets"]}),
            ),
            (
                "rest",
                "",
                "GET \u{a0}https://petst",
                "GET \u{a0}https://petstore3.swagger.io/api/v3/",
                Value::Null,
            ),
            (
                "grpc",
                "endpointText",
                "\u{2003}grpc://fixture.invalid/",
                "\u{2003}grpc://fixture.invalid/fixture.Echo/",
                json!({"services":["fixture.Echo"]}),
            ),
        ] {
            let hints = vec![("https://petstore3.swagger.io/api/v3/".into(), String::new())];
            let source = if field.is_empty() {
                format!("#endpoint\n{endpoint}")
            } else {
                endpoint.into()
            };
            let result = suggest(
                service,
                field,
                &source,
                usize::from(field.is_empty()),
                endpoint.encode_utf16().count(),
                &hints,
                &discovery,
            );
            assert_eq!(result.candidates.len(), 1, "{service}: {endpoint}");
            let mut units = endpoint.encode_utf16().collect::<Vec<_>>();
            units.splice(
                result.from..result.to,
                result.candidates[0].text.encode_utf16(),
            );
            assert_eq!(String::from_utf16(&units).unwrap(), expected);
        }
    }
    #[test]
    fn rest_path_candidates_respect_method() {
        let d =
            json!({"paths":["/alpha","/beta"],"methodsByPath":{"/alpha":["GET"],"/beta":["POST"]}});
        let source = "#check\n#endpoint\nPOST https://example.test/";
        let r = suggest(
            "rest",
            "",
            source,
            2,
            source.lines().last().unwrap().len(),
            &[],
            &d,
        );
        assert_eq!(
            r.candidates
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            vec!["/beta"]
        );
    }
    #[test]
    fn grpc_discovery_triggers_only_in_endpoint_contexts_with_an_authority() {
        for (field, source, expected) in [
            (
                "endpointText",
                "grpc://localhost:50051/",
                Some("grpc://localhost:50051/"),
            ),
            (
                "endpointText",
                "localhost:50051/demo.Greeter/",
                Some("localhost:50051/demo.Greeter/"),
            ),
            (
                "target",
                "grpcs://localhost:443/demo.Greeter/Hello",
                Some("grpcs://localhost:443/demo.Greeter/Hello"),
            ),
            ("endpointText", "grpc://", None),
            ("endpointText", "grpcs:///", None),
            ("endpointText", "", None),
            ("requestMessage", "grpc://localhost:50051/", None),
            (
                "",
                "#check demo\n#endpoint\n  localhost:50051/",
                Some("localhost:50051/"),
            ),
            (
                "",
                "#check demo\n#ENDPOINT\nlocalhost:50051/demo.Greeter/",
                Some("localhost:50051/demo.Greeter/"),
            ),
            (
                "",
                "#check demo\n#endpoint\nlocalhost:50051/\n#message\nlocalhost:50051/",
                None,
            ),
            (
                "",
                "#check demo\n#endpoint\nlocalhost:50051/\n#responce OK\nlocalhost:50051/",
                None,
            ),
        ] {
            let line = source.split('\n').count() - 1;
            let ch = source.rsplit('\n').next().unwrap().encode_utf16().count();
            assert_eq!(
                discovery_endpoint("grpc", field, source, line, ch).as_deref(),
                expected,
                "{source}"
            );
        }
    }
    #[test]
    fn rest_slash_discovery_only_reads_the_endpoint_at_the_caret() {
        for (service, field, source, expected) in [
            (
                "rest",
                "url",
                "https://fixture.invalid/",
                Some("https://fixture.invalid/"),
            ),
            (
                "rest",
                "",
                "#endpoint\nGET \u{a0}https://пример.рф/api/",
                Some("https://пример.рф/api/"),
            ),
            ("rest", "url", "https://", None),
            ("rest", "url", "https:/", None),
            (
                "rest",
                "headersText",
                "Referer: https://fixture.invalid/",
                None,
            ),
            (
                "rest",
                "",
                "#endpoint\nGET https://fixture.invalid/\n#headers\nReferer: https://fixture.invalid/",
                None,
            ),
            (
                "rest",
                "body",
                "{\"url\":\"https://fixture.invalid/\"}",
                None,
            ),
            (
                "rest",
                "",
                "#endpoint\nGET https://fixture.invalid/\n#body\nhttps://fixture.invalid/",
                None,
            ),
            ("graphql", "url", "https://fixture.invalid/", None),
            ("mysql", "dsn", "mysql://fixture.invalid/", None),
        ] {
            let line = source.split('\n').count() - 1;
            let ch = source.rsplit('\n').next().unwrap().encode_utf16().count();
            assert_eq!(
                discovery_endpoint(service, field, source, line, ch).as_deref(),
                expected,
                "{service} {field}: {source}"
            );
        }
        let source = "#endpoint\nGET https://fixture.invalid/api/";
        let endpoint =
            discovery_endpoint("rest", "", source, 1, "GET https://fixture.invalid".len()).unwrap();
        assert!(
            !endpoint.ends_with('/'),
            "A slash to the right of the caret is not a newly entered slash"
        );
    }
    #[test]
    fn grpc_methods_are_scoped_to_the_discovered_target_and_service() {
        let discovery = json!({
            "discoveryEndpoint":"grpc://localhost:50051/demo.Greeter/",
            "services":["demo.Greeter"],"methods":["Hello"]
        });
        for (source, expected) in [
            ("grpc://localhost:50051/de", vec!["demo.Greeter/"]),
            ("grpc://localhost:50051/demo.Greeter/", vec!["Hello"]),
            ("grpc://localhost:50051/demo.Other/", vec![]),
            ("grpc://other.invalid:50051/de", vec![]),
            ("grpc://other.invalid:50051/demo.Greeter/", vec![]),
        ] {
            let result = suggest(
                "grpc",
                "endpointText",
                source,
                0,
                source.len(),
                &[],
                &discovery,
            );
            assert_eq!(
                result
                    .candidates
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>(),
                expected,
                "{source}"
            );
        }
    }
    #[test]
    fn grpc_plain_target_completes_services_and_methods() {
        let d = json!({"services":["demo.Greeter"],"methods":["Hello"]});
        let r = suggest("grpc", "endpointText", "localhost:50051/de", 0, 18, &[], &d);
        assert_eq!(r.candidates[0].text, "demo.Greeter/");
        let source = "localhost:50051/demo.Greeter/H";
        let r = suggest("grpc", "endpointText", source, 0, source.len(), &[], &d);
        assert_eq!(r.candidates[0].text, "Hello");
    }
    #[test]
    fn mysql_alias_can_be_declared_after_cursor() {
        let d = json!({"tables":[{"name":"users","columns":[{"name":"email"}]},{"name":"orders","columns":[{"name":"total"}]}]});
        let source = "SELECT u.em\nFROM users u";
        let r = suggest("mysql", "sql", source, 0, 11, &[], &d);
        assert_eq!(r.candidates[0].text, "email");
        assert_eq!(r.candidates.len(), 1);
    }
    #[test]
    fn fuzzy_saved_headers_are_isolated_and_used_names_are_hidden() {
        let hints = vec![("X-Correlation-Id: ".into(), "saved REST".into())];
        let r = suggest("rest", "headersText", "xcid", 0, 4, &hints, &Value::Null);
        assert_eq!(r.candidates[0].text, "X-Correlation-Id: ");
        let r = suggest("graphql", "headersText", "xcid", 0, 4, &[], &Value::Null);
        assert!(r.candidates.is_empty());
        let r = suggest(
            "rest",
            "headersText",
            "xcid\nX-Correlation-Id: used",
            0,
            4,
            &hints,
            &Value::Null,
        );
        assert!(r.candidates.is_empty());
    }
    #[test]
    fn discovered_headers_merge_without_overriding_saved_values() {
        let hints = vec![("X-Auth: token".into(), "Saved hint".into())];
        let d = json!({"headers":["X-Auth","X-Trace"]});
        let r = suggest("rest", "headersText", "", 0, 0, &hints, &d);
        assert_eq!(
            r.candidates
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            vec!["X-Auth: token", "X-Trace: "]
        );
        let r = suggest("rest", "headersText", "trace", 0, 5, &hints, &d);
        assert_eq!(r.candidates[0].text, "X-Trace: ");
    }
}
