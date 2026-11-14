//! The Artemis check language: `#action` commands and clauses, the endpoint, headers,
//! options and JSON body; completion of commands, clauses and discovered queue names.

use crate::action::COMMANDS;
use serde_json::Value;
use services_sdk::lang::check::At;
use services_sdk::lang::{endpoint, finish, item, json, key_value, marker, table, token_start};
use services_sdk::{Check, Completions, Item, Line, Token};

pub const SECTIONS: [&str; 5] = ["target", "metadata", "action", "options", "payload"];

const KEYWORDS: [&str; 16] = [
    "WHERE", "LIMIT", "SET", "TO", "AND", "OR", "NOT", "IN", "LIKE", "BETWEEN", "IS", "NULL",
    "ESCAPE", "CONTAINS", "TRUE", "FALSE",
];
const SYMBOLS: &str = "=<>!,()";

const CLAUSES: [(&str, &str); 20] = [
    (
        "WHERE ",
        "Lists: one condition, messages > 0; messages: a JMS selector",
    ),
    (
        "LIMIT ",
        "Lists 100, BROWSE 10, RECEIVE 1 by default; up to 1000",
    ),
    (
        "SET ",
        "SEND, PUBLISH: message properties, name = 'value', ...",
    ),
    ("TO ", "MOVE: the target /queue"),
    ("AND", "Selector"),
    ("OR", "Selector"),
    ("NOT", "Selector"),
    ("LIKE", "Selector"),
    ("IN", "Selector"),
    ("BETWEEN", "Selector"),
    ("IS NULL", "Selector"),
    ("CONTAINS", "Lists: the column contains the text"),
    ("NOT CONTAINS", "Lists: the column lacks the text"),
    ("_AMQ_GROUP_ID", "SET: message group"),
    ("_AMQ_DUPL_ID", "SET: duplicate detection id"),
    ("_AMQ_ORIG_ADDRESS", "WHERE: address before DLQ or expiry"),
    ("_AMQ_ORIG_QUEUE", "WHERE: queue before DLQ or expiry"),
    ("AMQPriority", "WHERE: 0-9"),
    ("AMQTimestamp", "WHERE: milliseconds since 1970"),
    ("AMQDurable", "WHERE: 'DURABLE' or 'NON_DURABLE'"),
];

const OPTIONS: [(&str, &str); 4] = [
    (
        "Columns-Type: ",
        "column=json column=datetime column=number column=boolean column=text",
    ),
    ("Columns-Size: ", "column=100 (characters)"),
    ("Format: table", "Option"),
    ("Format: json", "Option"),
];

fn token(start: usize, len: usize, kind: &str) -> Token {
    Token {
        start: start as u32,
        len: len as u32,
        kind: kind.into(),
    }
}

/// The tokens of one `#action` line; `command` is whether the command was seen.
pub fn action(line: &str, command: &mut bool) -> Vec<Token> {
    let mut out = vec![];
    let mut pos = 0;
    while pos < line.len() {
        let rest = &line[pos..];
        let first = rest.chars().next().unwrap_or(' ');
        if first.is_whitespace() {
            pos += first.len_utf8();
            continue;
        }
        let is_command = !std::mem::replace(command, true);
        if first == '\'' || first == '"' {
            let mut end = rest.len();
            let mut chars = rest.char_indices().skip(1).peekable();
            while let Some((i, c)) = chars.next() {
                if c == first {
                    if chars.peek().is_some_and(|(_, next)| *next == first) {
                        chars.next();
                        continue;
                    }
                    end = i + 1;
                    break;
                }
            }
            out.push(token(pos, end, "string"));
            pos += end;
            continue;
        }
        if SYMBOLS.contains(first) {
            let pair = rest.starts_with("<=")
                || rest.starts_with(">=")
                || rest.starts_with("!=")
                || rest.starts_with("<>");
            let len = if pair { 2 } else { 1 };
            out.push(token(pos, len, "operator"));
            pos += len;
            continue;
        }
        let len = rest
            .char_indices()
            .find(|(i, c)| {
                c.is_whitespace()
                    || SYMBOLS.contains(*c)
                    || *c == '\''
                    || *c == '"'
                    || rest[*i..].starts_with("{{")
            })
            .map_or(rest.len(), |(i, _)| i)
            .max(first.len_utf8());
        let word = &rest[..len];
        let kind = if is_command {
            Some("property")
        } else if first == '/' {
            Some("def")
        } else if word.parse::<f64>().is_ok() {
            Some("number")
        } else if KEYWORDS
            .iter()
            .any(|keyword| keyword.eq_ignore_ascii_case(word))
        {
            Some("keyword")
        } else {
            None
        };
        if let Some(kind) = kind {
            out.push(token(pos, len, kind));
        }
        pos += len;
    }
    out
}

/// The part of a check a line is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Area {
    /// Before the first marker, or a rendered response highlighted alone: table text.
    Prelude,
    Endpoint,
    Headers,
    Action,
    Body,
    Other,
    Response,
}

impl Area {
    const ALL: [Area; 7] = [
        Area::Prelude,
        Area::Endpoint,
        Area::Headers,
        Area::Action,
        Area::Body,
        Area::Other,
        Area::Response,
    ];
}

/// Highlighting of an Artemis check, and of its result tables.
pub fn highlight(
    state: &[u8],
    lines: &[String],
    columns: &[services_sdk::LineColumns],
) -> Vec<Line> {
    let (mut area, rest) = match state.split_first() {
        Some((area, rest)) => (
            Area::ALL
                .get(*area as usize)
                .copied()
                .unwrap_or(Area::Other),
            rest,
        ),
        None => (Area::Prelude, &[][..]),
    };
    let (mut response, rest) = match rest {
        [] => (
            table::State {
                guess_json: true,
                ..table::State::default()
            },
            rest,
        ),
        _ => table::State::read(rest),
    };
    let mut command = rest.first() == Some(&1);
    let mut body = json::State::decode(rest);
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let named = marker(line).map(|(name, _)| name);
            let is_marker = named.as_deref().is_some_and(|name| {
                area != Area::Response || matches!(name, "responce" | "response")
            });
            let tokens = if is_marker {
                area = match named.as_deref() {
                    Some("target") => Area::Endpoint,
                    Some("metadata" | "headers" | "options") => Area::Headers,
                    Some("action") => Area::Action,
                    Some("payload") => Area::Body,
                    Some("responce" | "response") => Area::Response,
                    _ => Area::Other,
                };
                command = false;
                body = json::State::default();
                response = response.restart();
                if let Some(reported) = columns.iter().find(|at| at.line == index as u32) {
                    response.report(&reported.columns);
                }
                if line.is_empty() {
                    vec![]
                } else {
                    vec![token(0, line.len(), "comment")]
                }
            } else {
                match area {
                    Area::Endpoint => endpoint::tokens(line, false),
                    Area::Headers => {
                        response.declare(line);
                        key_value::tokens(line)
                    }
                    Area::Action => action(line, &mut command),
                    Area::Body => json::tokens(line, &mut body),
                    Area::Prelude | Area::Response => table::tokens(line, &mut response),
                    Area::Other => vec![],
                }
            };
            let mut state = vec![area as u8];
            response.encode(&mut state);
            match area {
                Area::Action => state.push(u8::from(command)),
                Area::Body => body.encode(&mut state),
                _ => {}
            }
            Line { tokens, state }
        })
        .collect()
}

/// A queue or address as `#action` writes it: `/name`, or quoted when the name would split
/// into several words.
pub fn action_name(name: &str) -> String {
    if name.is_empty()
        || name
            .chars()
            .any(|c| c.is_whitespace() || "'\"=<>!,()".contains(c))
    {
        format!("'{}'", name.replace('\'', "''"))
    } else {
        format!("/{name}")
    }
}

fn pairs(at: &At, hints: Vec<Item>) -> Completions {
    let used = key_value::used(at.other_lines());
    let items: Vec<Item> = hints
        .into_iter()
        .filter(|item| !used.contains(&key_value::name(&item.insert)))
        .collect();
    let line_start = at.line_start(at.line);
    let mut result = finish(line_start, at.pos, at.before.trim(), items);
    if at.before.trim().is_empty() && !used.is_empty() {
        let mut items = finish(line_start, at.pos, "", at.unused(&SECTIONS)).items;
        items.append(&mut result.items);
        result.items = items;
    }
    result
}

/// Completion in an Artemis check; `discovered` holds the broker's queue and address names
/// (`{"names": {"queues": [..]}, "after": {"BROWSE": "queues"}}`), `null` when unknown.
pub fn complete(check: &Check, pos: usize, discovered: &Value) -> Completions {
    let at = At::new(&check.text, pos);
    if at.in_placeholder() || at.in_response() {
        return at.none();
    }
    if let Some(markers) = at.markers(&SECTIONS) {
        return markers;
    }
    match at.name() {
        Some("target") => {
            if let Some(next) = at.next_section(&SECTIONS) {
                return next;
            }
            let value = at.before.trim_start();
            let from = at.line_start(at.line) + at.before.len() - value.len();
            let items = ["http://localhost:8161", "https://"]
                .map(|url| item(url, "Target", "url"))
                .into();
            finish(from, at.pos, value, items)
        }
        Some("metadata" | "headers") => pairs(
            &at,
            key_value::items(
                &["Authorization: Basic ", "Message-Persistent: false"],
                &[],
                &Default::default(),
            ),
        ),
        Some("options") => pairs(
            &at,
            OPTIONS
                .iter()
                .map(|(insert, detail)| item(*insert, detail, "header"))
                .collect(),
        ),
        Some("action") => {
            let before = at.before;
            let start = token_start(before);
            let (start, query) = if before[..start].ends_with('/') {
                (start - 1, &before[start - 1..])
            } else {
                (start, &before[start..])
            };
            let from = at.line_start(at.line) + start;
            let body = at.body();
            let typed = body[..body.len() - query.len()].trim_end();
            let previous = typed
                .rsplit(char::is_whitespace)
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            let names = discovered["after"][previous.as_str()]
                .as_str()
                .and_then(|list| discovered["names"][list].as_array());
            let items = if typed.is_empty() {
                COMMANDS
                    .iter()
                    .map(|command| item(format!("{} ", command.name()), "Command", "keyword"))
                    .collect()
            } else if let Some(names) = names {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|name| item(format!("{} ", action_name(name)), "", "value"))
                    .collect()
            } else {
                let mut items = if before.trim().is_empty() {
                    at.unused(&SECTIONS)
                } else {
                    vec![]
                };
                items.extend(
                    CLAUSES
                        .iter()
                        .map(|(insert, detail)| item(*insert, detail, "keyword")),
                );
                items
            };
            finish(from, at.pos, query, items)
        }
        _ => at.none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kinds(line: &str, command: &mut bool) -> Vec<(String, String)> {
        action(line, command)
            .into_iter()
            .map(|t| {
                let at = t.start as usize;
                (line[at..at + t.len as usize].to_owned(), t.kind)
            })
            .collect()
    }

    #[test]
    fn action_colors_command_resource_clauses_and_quotes_across_lines() {
        let mut command = false;
        let first = kinds("BROWSE /DLQ", &mut command);
        assert_eq!(
            first,
            [
                ("BROWSE".into(), "property".into()),
                ("/DLQ".into(), "def".into())
            ]
        );
        let next = kinds("WHERE name = 'it''s' AND n >= 4 LIMIT 20", &mut command);
        assert!(next.contains(&("WHERE".into(), "keyword".into())));
        assert!(!next.iter().any(|(text, _)| text == "name"));
        assert!(next.contains(&("'it''s'".into(), "string".into())));
        assert!(next.contains(&(">=".into(), "operator".into())));
        assert!(next.contains(&("20".into(), "number".into())));
        let lines: Vec<String> = ["#action", "BROWSE", "/q"].map(String::from).into();
        let out = highlight(&[], &lines, &[]);
        assert_eq!(out[2].tokens[0].kind, "def");
        assert_eq!(
            highlight(&out[1].state, &lines[2..], &[])[0].tokens,
            out[2].tokens
        );
    }

    fn at(text: &str, discovered: &Value) -> (u32, Vec<String>) {
        let check = Check {
            columns: Vec::new(),
            text: text.into(),
            ..Check::default()
        };
        let completions = complete(&check, text.len(), discovered);
        (
            completions.from,
            completions.items.into_iter().map(|i| i.insert).collect(),
        )
    }

    #[test]
    fn action_offers_commands_first_then_clauses_never_headers() {
        let commands = at("#target\nhttp://b\n#action\n", &Value::Null).1;
        assert_eq!(commands.len(), 13);
        assert!(commands.iter().all(|text| text.ends_with(' ')));
        let clauses = at("#action\nBROWSE\n/orders W", &Value::Null).1;
        assert_eq!(clauses[0], "WHERE ");
        assert!(!clauses.iter().any(|text| text.contains(':')));
        assert!(
            at("#action\nSEND /q\n#payload\nA", &Value::Null)
                .1
                .is_empty()
        );
    }

    #[test]
    fn action_offers_discovered_names_after_their_command() {
        let discovered = json!({
            "names":{"queues":["orders","my queue","it's"],"addresses":["events"]},
            "after":{"BROWSE":"queues","PUBLISH":"addresses","TO":"queues"},
        });
        let text = "#action\nBROWSE ";
        assert_eq!(
            at(text, &discovered),
            (
                text.len() as u32,
                vec!["/orders ".into(), "'my queue' ".into(), "'it''s' ".into()]
            )
        );
        assert_eq!(
            at("#action\nbrowse /or", &discovered),
            (15, vec!["/orders ".into()])
        );
        assert_eq!(
            at("#action\nPUBLISH\n", &discovered).1,
            vec!["/events ".to_string()]
        );
        assert_eq!(
            at("#action\nMOVE /orders TO /o", &discovered).1,
            vec!["/orders ".to_string()]
        );
        assert_eq!(at("#action\nBROWSE /orders W", &discovered).1[0], "WHERE ");
        assert_eq!(at("#action\nBROWSE ", &Value::Null).1[0], "WHERE ");
    }
}
