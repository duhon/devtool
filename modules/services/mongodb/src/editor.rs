//! Editor support of a MongoDB check: highlighting and completion of markers, the
//! connection string, collection, action, options and the JSON payload, driven by what
//! discovery found.

use crate::action;
use crate::config::OPTIONS;
use crate::payload;
use serde_json::Value;
use services_sdk::lang::check::At;
use services_sdk::lang::table::{self, State as TableState};
use services_sdk::lang::{endpoint, finish, headers, item, json, marker};
use services_sdk::{Check, Completions, Item, Line, LineColumns, Token};
use std::collections::BTreeMap;

pub const SECTIONS: [&str; 5] = ["target", "collection", "action", "payload", "options"];

/// What discovery found about a connection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    pub collections: Vec<String>,
    pub fields: BTreeMap<String, Vec<String>>,
    /// Per collection and field: the values the sample showed, as JSON text.
    pub values: BTreeMap<String, BTreeMap<String, Vec<String>>>,
}

/// Completion in a MongoDB check: markers, the connection string, collection, action,
/// options and payload keys.
pub fn complete(check: &Check, pos: usize, known: &Catalog) -> Completions {
    let at = At::new(&check.text, pos);
    if at.in_placeholder() || at.in_response() {
        return at.none();
    }
    if let Some(markers) = at.markers(&SECTIONS) {
        return markers;
    }
    let line_start = at.line_start(at.line);
    let collection = check.text_of("collection").trim();
    let value_of = |items: Vec<Item>| {
        let value = at.before.trim_start();
        let from = line_start + at.before.len() - value.len();
        finish(from, at.pos, value, items)
    };
    match at.name() {
        Some("target") => {
            if let Some(next) = at.next_section(&SECTIONS) {
                return next;
            }
            value_of(vec![
                item("mongodb://", "Connection string", "url"),
                item("mongodb+srv://", "DNS seed list connection string", "url"),
            ])
        }
        Some("collection") => {
            if let Some(next) = at.next_section(&SECTIONS) {
                return next;
            }
            value_of(
                known
                    .collections
                    .iter()
                    .map(|name| item(name.as_str(), "Collection", "class"))
                    .collect(),
            )
        }
        Some("action") => {
            if let Some(next) = at.next_section(&SECTIONS) {
                return next;
            }
            value_of(
                action::actions(!collection.is_empty())
                    .map(|(name, detail)| item(name, detail, "keyword"))
                    .collect(),
            )
        }
        Some("options") => {
            let used = headers::used(at.other_lines());
            let hints = [
                "Timeout: 30",
                "Max-Documents: 1000",
                "Format: json",
                "Format: table",
                "Format: vertical",
            ];
            let extra: Vec<(&str, &str)> = OPTIONS.iter().map(|name| (*name, "Option")).collect();
            let items = headers::items(&hints, &extra, &used);
            let mut result = finish(line_start, at.pos, at.before.trim(), items);
            if at.before.trim().is_empty() && !used.is_empty() {
                let mut items = finish(line_start, at.pos, "", at.unused(&SECTIONS)).items;
                items.append(&mut result.items);
                result.items = items;
            }
            result
        }
        Some("payload") => {
            let action = check.text_of("action").trim();
            let after = &at.text[at.pos..];
            let after = after.split('\n').next().unwrap_or("");
            let later = at.whole().get(at.body().len()..).unwrap_or("");
            let analysis = payload::analyze(at.body(), after, later, action, collection);
            let items = payload::items(&analysis, known);
            finish(
                at.pos - analysis.fragment.len() - analysis.back,
                at.pos + analysis.replaced(),
                &analysis.fragment,
                items,
            )
        }
        _ => at.none(),
    }
}

/// What a check asks discovery for.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub collections: bool,
    /// The collections whose fields the payload and its collection need.
    pub fields: Vec<String>,
}

/// The collections to sample: the check's own and those its payload names.
pub fn wanted(check: &Check) -> Vec<String> {
    let collection = check.text_of("collection").trim();
    let mut list = Vec::new();
    if !collection.is_empty() && !collection.contains("{{") {
        list.push(collection.to_owned());
    }
    for name in payload::referenced(check.text_of("payload")) {
        if !name.contains("{{") && !list.contains(&name) {
            list.push(name);
        }
    }
    list.truncate(payload::MAX_SAMPLED);
    list
}

/// What discovery a check needs. Nothing while the target is empty or the cursor is
/// still in it; `pos` is the cursor of a hints request, a document has none.
pub fn needs(check: &Check, pos: Option<usize>) -> Option<Plan> {
    if check.text_of("target").trim().is_empty() {
        return None;
    }
    let at = At::new(&check.text, pos.unwrap_or(check.text.len()));
    if at.in_response() || (pos.is_some() && at.in_placeholder()) {
        return None;
    }
    if at.name() == Some("target") && !at.before.trim().is_empty() {
        return None;
    }
    Some(Plan {
        collections: true,
        fields: wanted(check),
    })
}

fn whole(line: &str, kind: &str) -> Vec<Token> {
    if line.is_empty() {
        return vec![];
    }
    vec![Token {
        start: 0,
        len: line.len() as u32,
        kind: kind.into(),
    }]
}

/// The part of a check a line is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Area {
    Prelude,
    Endpoint,
    Options,
    Payload,
    Other,
    Response,
    Collection,
    Action,
}

impl Area {
    const ALL: [Area; 8] = [
        Area::Prelude,
        Area::Endpoint,
        Area::Options,
        Area::Payload,
        Area::Other,
        Area::Response,
        Area::Collection,
        Area::Action,
    ];
}

const RESPONSE_UNKNOWN: u8 = 0;
const RESPONSE_ROWS: u8 = 1;
const RESPONSE_JSON: u8 = 2;

/// Payload tokens of a line: the JSON lexer, with the names of the language set apart.
/// Keys are `property`; `$operators` and stages are `keyword`; the wrappers of Extended
/// JSON are `type`; the fixed fields of a command (`pipeline`, `from`, …) are `attribute`;
/// strings that name a field (`"$storeId"`, `"$$sid"`) are `variable-3`.
fn payload_tokens(line: &str, state: &mut json::State) -> Vec<Token> {
    let mut tokens = json::tokens(line, state);
    for token in &mut tokens {
        let start = token.start as usize;
        let text = &line[start..start + token.len as usize];
        let name = text.trim_matches(['"', '\'']);
        let (key, quoted) = match token.kind.as_str() {
            "string property" => (true, true),
            "property" => (true, false),
            "string" => (false, true),
            _ => continue,
        };
        let style = if key {
            if payload::wrapper(name) {
                "type"
            } else if name.starts_with('$') {
                "keyword"
            } else if payload::field(name) {
                "attribute"
            } else {
                continue;
            }
        } else if text.starts_with(['"', '\'']) && name.starts_with('$') && name.len() > 1 {
            "variable-3"
        } else {
            continue;
        };
        token.kind = if quoted {
            format!("string {style}")
        } else {
            style.into()
        };
    }
    tokens
}

/// Highlighting of a MongoDB check: the endpoint, collection, action, options, payload and results.
pub fn highlight(state: &[u8], lines: &[String], columns: &[LineColumns]) -> Vec<Line> {
    let (mut area, mut shape, rest) = match state.split_first() {
        Some((byte, rest)) => (
            Area::ALL
                .get((*byte & 0x0f) as usize)
                .copied()
                .unwrap_or(Area::Other),
            *byte >> 4,
            rest,
        ),
        None => (Area::Prelude, 0, &[][..]),
    };
    let (mut response, rest) = match rest {
        [] => (
            TableState {
                guess_json: true,
                ..TableState::default()
            },
            rest,
        ),
        _ => TableState::read(rest),
    };
    let mut payload = json::State::decode(rest);
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
                    Some("options") => Area::Options,
                    Some("collection") => Area::Collection,
                    Some("action") => Area::Action,
                    Some("payload") => Area::Payload,
                    Some("responce" | "response") => Area::Response,
                    _ => Area::Other,
                };
                payload = json::State::default();
                shape = 0;
                response = response.restart();
                if let Some(reported) = columns.iter().find(|at| at.line == index as u32) {
                    response.report(&reported.columns);
                }
                whole(line, "comment")
            } else {
                match area {
                    Area::Endpoint => endpoint::tokens(line, false),
                    Area::Options => {
                        response.declare(line);
                        headers::tokens(line)
                    }
                    Area::Collection => endpoint::tokens(line, false),
                    Area::Action => whole(line.trim_end(), "keyword"),
                    Area::Payload => payload_tokens(line, &mut payload),
                    Area::Prelude | Area::Response => {
                        if shape == RESPONSE_UNKNOWN && !line.trim().is_empty() {
                            let rows = line.starts_with(['+', '|', '*']) || line == "Empty set";
                            shape = if rows { RESPONSE_ROWS } else { RESPONSE_JSON };
                        }
                        match shape {
                            RESPONSE_JSON if line.starts_with("ERROR ") => vec![],
                            RESPONSE_JSON => json::tokens(line, &mut payload),
                            _ => table::tokens(line, &mut response),
                        }
                    }
                    Area::Other => vec![],
                }
            };
            let mut state = vec![area as u8 | shape << 4];
            response.encode(&mut state);
            payload.encode(&mut state);
            Line { tokens, state }
        })
        .collect()
}

/// The folds of a response: tables never fold, a JSON reply folds by brackets.
pub fn folds(check: &Check) -> Vec<services_sdk::Fold> {
    let mut folds = table::folds(&check.text, &check.columns);
    let lines: Vec<&str> = check.text.split('\n').collect();
    if let Some(start) = lines.iter().position(|line| {
        marker(line).is_some_and(|(name, _)| matches!(name.as_str(), "responce" | "response"))
    }) {
        let body = &lines[start + 1..];
        if !body
            .iter()
            .any(|line| line.starts_with('+') || line.starts_with('|'))
        {
            folds.extend(json::folds(body.iter().copied()).into_iter().map(|fold| {
                services_sdk::Fold {
                    start: fold.start + start as u32 + 1,
                    end: fold.end + start as u32 + 1,
                }
            }));
        }
    }
    folds
}

/// A schema value the Agent reads: the database, its collections and their sampled fields.
pub fn schema(database: &str, known: &Catalog) -> Value {
    serde_json::json!({
        "database": database,
        "collections": known.collections.iter().map(|name| serde_json::json!({
            "name": name,
            "fields": known.fields.get(name).cloned().unwrap_or_default(),
        })).collect::<Vec<_>>(),
    })
}
