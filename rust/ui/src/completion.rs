use crate::console;
use serde_json::Value;
use services_contracts::manifest::DiscoveryKind;
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
    let inline_arguments = crate::services::language(service, "") == "graphql"
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
        (crate::services::discovery(service) == Some(DiscoveryKind::Openapi)
            && text.starts_with("{\n"))
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
    let manifest = crate::services::get(service)?;
    let discovery = manifest
        .console
        .discovery
        .as_ref()
        .filter(|discovery| discovery.kind.paths())?;
    let http = manifest
        .section("endpoint")
        .is_some_and(|section| section.language == "http");
    if !(field == "endpoint"
        || field.is_empty()
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
    let endpoint = if http && field.is_empty() {
        before.split_once(char::is_whitespace)?.1.trim()
    } else {
        before.trim()
    };
    if http
        && !endpoint.split_once("://").is_some_and(|(scheme, _)| {
            discovery
                .schemes
                .iter()
                .any(|known| scheme.eq_ignore_ascii_case(known))
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
/// `field` is empty for Console documents; otherwise the editor holds one
/// section and `field` names its marker (`headers`, `body`, `variables`, ...).
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
    let Some(manifest) = crate::services::get(service) else {
        return finish(ch, ch, "", vec![]);
    };
    let kind = manifest
        .console
        .discovery
        .as_ref()
        .map(|discovery| discovery.kind);
    let openapi = kind == Some(DiscoveryKind::Openapi);
    let http = manifest
        .section("endpoint")
        .is_some_and(|section| section.language == "http");
    let section_language = |marker: &str| {
        manifest
            .section(marker)
            .map(|section| section.language.as_str())
    };
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
        field.into()
    };
    let graphql_variables =
        manifest.language("") == "graphql" && section_language(&marker) == Some("json");
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
            Vec::new()
        };
        sections
            .iter()
            .map(String::as_str)
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
    if section_language(&marker) == Some("headers") {
        let used: BTreeSet<_> = lines[body_start.min(line)..section_end]
            .iter()
            .enumerate()
            .filter(|(offset, _)| body_start.min(line) + offset != line)
            .filter_map(|(_, l)| l.split_once(':'))
            .map(|(k, _)| k.trim().to_ascii_lowercase())
            .collect();
        let declared = section_hints(&manifest, &marker);
        let candidates = if !declared.is_empty() {
            declared
        } else {
            let mut candidates = hints
                .iter()
                .filter(|(s, _)| s.contains(':') && !s.contains("://") && !s.starts_with("{{"))
                .cloned()
                .collect::<Vec<_>>();
            if openapi && marker == "headers" {
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
        let value_start = if http && field.is_empty() {
            before.find(char::is_whitespace).unwrap_or(0)
        } else {
            0
        };
        let value = before[value_start..].trim_start();
        // An `http` endpoint starts with one of the service's keywords (REST methods, STOMP commands).
        if http && field.is_empty() && value_start == 0 {
            return finish(
                0,
                ch,
                &before,
                manifest
                    .console
                    .keywords
                    .iter()
                    .map(|s| (format!("{s} "), "Method".into()))
                    .collect(),
            );
        }
        let value_start = before[..before.len() - value.len()].encode_utf16().count();
        if kind == Some(DiscoveryKind::GrpcReflection) {
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
        if openapi && value.contains("://") {
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
        candidates.extend(section_hints(&manifest, "endpoint"));
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
    if kind.is_some_and(DiscoveryKind::paths) && section_language(&marker) == Some("json") {
        if body.trim().is_empty() {
            let mut candidates = vec![];
            if openapi && whole_body.trim().is_empty() {
                let fields = rest_fields(
                    service,
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
        let root = serde_json::json!({"fields": if openapi {
            rest_fields(
                service,
                discovery,
                block.as_ref().map(|b| b.text.as_str()).unwrap_or(source),
                hints,
            )
        } else {
            discovery["fields"].clone()
        }});
        let offset = lines[body_start.min(line)..line]
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>()
            + before.len();
        if openapi && let Some(result) = enum_completion(&root, &whole_body, offset, &before) {
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
                let kind = if openapi && schema["items"]["kind"] == "bare" {
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
            .filter(|field| !openapi || field["readOnly"] != true)
            .filter_map(|f| {
                let name = f["name"].as_str()?;
                if used.contains(name) {
                    return None;
                }
                let value = if openapi
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
                    None if openapi => {
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
                let detail = if openapi && f["enum"].is_array() {
                    continuations.insert(text.clone());
                    if f["type"].is_string() || f["format"].is_string() {
                        format!("{detail} · enum")
                    } else {
                        "enum".into()
                    }
                } else if openapi && f["items"]["enum"].is_array() {
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
    if section_language(&marker) == Some("graphql") {
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
            console::sections(service, &scope)
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
    if section_language(&marker) == Some("sql") {
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
/// Static candidates the manifest declares for a section.
fn section_hints(
    manifest: &services_contracts::manifest::Manifest,
    marker: &str,
) -> Vec<(String, String)> {
    manifest
        .section(marker)
        .into_iter()
        .flat_map(|section| &section.hints)
        .map(|hint| (hint.text.clone(), hint.detail.clone()))
        .collect()
}
fn rest_fields(
    service: &str,
    discovery: &Value,
    request: &str,
    hints: &[(String, String)],
) -> Value {
    let endpoint = console::sections(service, request)
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
