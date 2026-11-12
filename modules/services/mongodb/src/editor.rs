//! Editor support of a MongoDB check: highlighting and completion of markers, the
//! connection string, collection, action, options and the JSON payload, driven by what
//! discovery found.

use crate::action;
use crate::config::OPTIONS;
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
}

const QUERY_OPERATORS: [&str; 22] = [
    "$eq",
    "$ne",
    "$gt",
    "$gte",
    "$lt",
    "$lte",
    "$in",
    "$nin",
    "$exists",
    "$type",
    "$regex",
    "$options",
    "$not",
    "$elemMatch",
    "$size",
    "$all",
    "$mod",
    "$bitsAllSet",
    "$bitsAnySet",
    "$bitsAllClear",
    "$bitsAnyClear",
    "$expr",
];
const LOGICAL: [&str; 6] = ["$and", "$or", "$nor", "$expr", "$text", "$where"];
const UPDATE_OPERATORS: [&str; 15] = [
    "$set",
    "$unset",
    "$inc",
    "$mul",
    "$min",
    "$max",
    "$rename",
    "$currentDate",
    "$setOnInsert",
    "$push",
    "$pull",
    "$pop",
    "$addToSet",
    "$pullAll",
    "$bit",
];
const STAGES: [&str; 33] = [
    "$match",
    "$group",
    "$project",
    "$sort",
    "$limit",
    "$skip",
    "$unwind",
    "$lookup",
    "$addFields",
    "$set",
    "$unset",
    "$count",
    "$sample",
    "$sortByCount",
    "$facet",
    "$bucket",
    "$bucketAuto",
    "$replaceRoot",
    "$replaceWith",
    "$out",
    "$merge",
    "$unionWith",
    "$graphLookup",
    "$geoNear",
    "$redact",
    "$setWindowFields",
    "$densify",
    "$fill",
    "$indexStats",
    "$collStats",
    "$currentOp",
    "$listSessions",
    "$search",
];
const ACCUMULATORS: [&str; 16] = [
    "$sum",
    "$avg",
    "$min",
    "$max",
    "$first",
    "$last",
    "$push",
    "$addToSet",
    "$count",
    "$stdDevPop",
    "$stdDevSamp",
    "$top",
    "$bottom",
    "$firstN",
    "$lastN",
    "$mergeObjects",
];
const LOOKUP: [&str; 5] = ["from", "localField", "foreignField", "as", "pipeline"];
const COMMANDS: [&str; 24] = [
    "ping",
    "find",
    "aggregate",
    "count",
    "distinct",
    "insert",
    "update",
    "delete",
    "findAndModify",
    "createIndexes",
    "dropIndexes",
    "listIndexes",
    "listCollections",
    "listDatabases",
    "create",
    "drop",
    "dropDatabase",
    "collStats",
    "dbStats",
    "serverStatus",
    "hello",
    "buildInfo",
    "explain",
    "connectionStatus",
];
const INDEX_OPTIONS: [&str; 9] = [
    "key",
    "name",
    "unique",
    "sparse",
    "expireAfterSeconds",
    "partialFilterExpression",
    "collation",
    "hidden",
    "background",
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elem {
    Filter,
    Fields,
    Index,
}

impl Elem {
    fn role(self) -> Role {
        match self {
            Self::Filter => Role::Filter,
            Self::Fields => Role::Fields,
            Self::Index => Role::Index,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The payload object of an action: its fields.
    Root,
    /// The payload of `runCommand`: the command document.
    Command,
    Filter,
    /// An array of documents of a role: `$and`, `documents`.
    Many(Elem),
    FieldQuery,
    Update,
    /// Field names: a projection, a sort, the body of `$set`.
    Fields,
    Pipeline,
    Stage,
    Group,
    Accumulator,
    Lookup,
    Index,
    Plain,
}

/// The fields the payload of `action` takes.
fn root_keys(action: &str) -> &'static [&'static str] {
    match action.to_ascii_lowercase().as_str() {
        "find" => &[
            "filter",
            "projection",
            "sort",
            "limit",
            "skip",
            "hint",
            "collation",
            "batchSize",
            "singleBatch",
            "maxTimeMS",
            "allowDiskUse",
            "comment",
            "min",
            "max",
            "returnKey",
            "showRecordId",
        ],
        "aggregate" => &[
            "pipeline",
            "allowDiskUse",
            "collation",
            "hint",
            "maxTimeMS",
            "comment",
            "let",
        ],
        "count" => &["filter", "limit", "skip", "hint", "collation", "maxTimeMS"],
        "countdocuments" => &["filter", "skip", "limit"],
        "distinct" => &["key", "filter", "collation", "maxTimeMS"],
        "insert" => &[
            "documents",
            "ordered",
            "bypassDocumentValidation",
            "writeConcern",
        ],
        "update" => &[
            "filter",
            "update",
            "upsert",
            "multi",
            "arrayFilters",
            "collation",
            "hint",
            "ordered",
            "writeConcern",
        ],
        "delete" => &[
            "filter",
            "limit",
            "collation",
            "hint",
            "ordered",
            "writeConcern",
        ],
        "findandmodify" => &[
            "filter",
            "update",
            "remove",
            "new",
            "upsert",
            "sort",
            "projection",
            "arrayFilters",
            "collation",
            "hint",
            "maxTimeMS",
        ],
        "createindexes" => &[
            "indexes",
            "key",
            "name",
            "unique",
            "sparse",
            "expireAfterSeconds",
        ],
        "dropindexes" => &["index"],
        "listcollections" => &["filter", "nameOnly", "authorizedCollections"],
        "listdatabases" => &["filter", "nameOnly", "authorizedDatabases"],
        _ => &[],
    }
}

/// The role of the root of a payload that opens with `open`.
fn root(action: &str, open: char) -> Role {
    match (open, action.to_ascii_lowercase().as_str()) {
        ('{', "runcommand") => Role::Command,
        ('{', _) => Role::Root,
        (_, "aggregate") => Role::Pipeline,
        (_, "insert") => Role::Many(Elem::Fields),
        (_, "createindexes") => Role::Many(Elem::Index),
        _ => Role::Plain,
    }
}

/// The role of the value of `key` in a `{}` of `parent`: an object, or an array when `array`.
fn nested(parent: Role, key: Option<&str>, array: bool) -> Role {
    let Some(key) = key else { return Role::Plain };
    let role = match (parent, key) {
        (Role::Root | Role::Command, "filter" | "query" | "q" | "partialFilterExpression") => {
            Role::Filter
        }
        (
            Role::Root | Role::Command,
            "sort" | "projection" | "hint" | "min" | "max" | "fields" | "key",
        ) => Role::Fields,
        (Role::Root | Role::Command, "update" | "u") if array => Role::Pipeline,
        (Role::Root | Role::Command, "update" | "u") => Role::Update,
        (Role::Root | Role::Command, "pipeline") => Role::Pipeline,
        (Role::Root | Role::Command, "documents") => Role::Many(Elem::Fields),
        (Role::Root | Role::Command, "indexes") => Role::Many(Elem::Index),
        (Role::Index, "key") => Role::Fields,
        (Role::Index, "partialFilterExpression") => Role::Filter,
        (Role::Filter, "$and" | "$or" | "$nor") => Role::Many(Elem::Filter),
        (Role::Filter, key) if key.starts_with('$') => Role::Plain,
        (Role::Filter, _) => Role::FieldQuery,
        (Role::FieldQuery, "$elemMatch") => Role::Filter,
        (Role::FieldQuery, "$not") => Role::FieldQuery,
        (Role::Update, _) => Role::Fields,
        (Role::Stage, "$match") => Role::Filter,
        (Role::Stage, "$project" | "$sort" | "$addFields" | "$set" | "$unset") => Role::Fields,
        (Role::Stage, "$group") => Role::Group,
        (Role::Stage, "$lookup") => Role::Lookup,
        (Role::Group, _) => Role::Accumulator,
        (Role::Lookup, "pipeline") => Role::Pipeline,
        _ => Role::Plain,
    };
    if array && !matches!(role, Role::Pipeline | Role::Many(_)) {
        return Role::Plain;
    }
    role
}

/// The role of a value that opens with `open` inside a frame.
fn child(parent: &Frame, open: char) -> Role {
    match (parent.open, open) {
        ('{', _) => nested(parent.role, parent.key.as_deref(), open == '['),
        (_, '{') => match parent.role {
            Role::Pipeline => Role::Stage,
            Role::Many(elem) => elem.role(),
            _ => Role::Plain,
        },
        _ => Role::Plain,
    }
}

#[derive(Debug, PartialEq)]
pub enum Want {
    Nothing,
    /// A key of a document.
    Keys {
        role: Role,
        /// A key started inside quotes.
        quoted: bool,
        /// The first key of the document.
        first: bool,
    },
}

/// What a position of a payload asks for, and the fragment typed before it.
#[derive(Debug, PartialEq)]
pub struct Analysis {
    pub fragment: String,
    pub want: Want,
}

struct Frame {
    open: char,
    role: Role,
    key: Option<String>,
    expect_key: bool,
    first: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Lex<'a> {
    Word(&'a str),
    Text(&'a str),
    Punct(char),
    Other,
}

fn lex(source: &str) -> (Vec<Lex<'_>>, Option<usize>) {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = source[i..].find('\n').map_or(bytes.len(), |n| i + n);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = source[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |n| i + n + 4);
            }
            b'"' | b'\'' => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b {
                    j += if bytes[j] == b'\\' { 2 } else { 1 };
                }
                if j >= bytes.len() {
                    return (out, Some(i));
                }
                out.push(Lex::Text(&source[i + 1..j]));
                i = j + 1;
            }
            b'{' | b'}' | b'[' | b']' | b',' | b':' => {
                out.push(Lex::Punct(b as char));
                i += 1;
            }
            _ if b.is_ascii_alphanumeric() || b == b'_' || b == b'$' => {
                let end = source[i..]
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
                    .map_or(bytes.len(), |n| i + n);
                out.push(Lex::Word(&source[i..end]));
                i = end;
            }
            _ => {
                out.push(Lex::Other);
                i += source[i..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    (out, None)
}

/// The analysis of the payload text before the cursor.
pub fn analyze(prefix: &str, action: &str) -> Analysis {
    let (fragment_start, quoted) = {
        let (_, open) = lex(prefix);
        match open {
            Some(start) => (start + 1, true),
            None => {
                let start = prefix
                    .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
                    .map_or(0, |n| {
                        n + prefix[n..].chars().next().map_or(1, char::len_utf8)
                    });
                (start, false)
            }
        }
    };
    let fragment = prefix[fragment_start..].to_owned();
    let head = if quoted {
        &prefix[..fragment_start - 1]
    } else {
        &prefix[..fragment_start]
    };
    let (tokens, _) = lex(head);
    let want = want(&tokens, quoted, action);
    Analysis { fragment, want }
}

fn want(tokens: &[Lex], quoted: bool, action: &str) -> Want {
    let mut stack: Vec<Frame> = Vec::new();
    for token in tokens {
        match token {
            Lex::Punct(open @ ('{' | '[')) => {
                let role = match stack.last() {
                    Some(parent) => child(parent, *open),
                    None if stack.is_empty() => root(action, *open),
                    None => Role::Plain,
                };
                stack.push(Frame {
                    open: *open,
                    role,
                    key: None,
                    expect_key: *open == '{',
                    first: true,
                });
            }
            Lex::Punct('}' | ']') => {
                stack.pop();
            }
            Lex::Punct(',') => {
                if let Some(frame) = stack.last_mut() {
                    frame.expect_key = frame.open == '{';
                    frame.key = None;
                    frame.first = false;
                }
            }
            Lex::Punct(':') => {
                if let Some(frame) = stack.last_mut() {
                    frame.expect_key = false;
                }
            }
            Lex::Word(word) | Lex::Text(word) => {
                if let Some(frame) = stack.last_mut()
                    && frame.open == '{'
                    && frame.expect_key
                    && frame.key.is_none()
                {
                    frame.key = Some((*word).to_owned());
                }
            }
            _ => {}
        }
    }
    match stack.last() {
        Some(top) if top.open == '{' && top.expect_key && top.key.is_none() => Want::Keys {
            role: top.role,
            quoted,
            first: top.first && stack.len() == 1,
        },
        _ => Want::Nothing,
    }
}

fn key_items(
    role: Role,
    quoted: bool,
    after: &str,
    fields: &[String],
    first: bool,
    action: &str,
) -> Vec<Item> {
    let closed = after.starts_with('"');
    let colon = after.trim_start_matches('"').trim_start().starts_with(':');
    let suffix = |name: &str| match (quoted, closed, colon) {
        (true, true, _) => name.to_owned(),
        (true, false, true) => format!("{name}\""),
        (true, false, false) => format!("{name}\": $0"),
        (false, _, true) => format!("\"{name}\""),
        (false, _, false) => format!("\"{name}\": $0"),
    };
    let names = |list: &[&str], detail: &str, kind: &str| -> Vec<Item> {
        list.iter()
            .map(|name| item(suffix(name), detail, kind))
            .collect()
    };
    let field = |detail: &str| -> Vec<Item> {
        fields
            .iter()
            .map(|name| item(suffix(name), detail, "property"))
            .collect()
    };
    match role {
        Role::Root => names(root_keys(action), "Field", "property"),
        Role::Filter => {
            let mut items = field("Field");
            items.extend(names(&LOGICAL, "Operator", "keyword"));
            items
        }
        Role::FieldQuery => names(&QUERY_OPERATORS, "Query operator", "keyword"),
        Role::Update => names(&UPDATE_OPERATORS, "Update operator", "keyword"),
        Role::Fields => field("Field"),
        Role::Stage => names(&STAGES, "Aggregation stage", "keyword"),
        Role::Group => {
            let mut items = names(&["_id"], "Group key", "property");
            items.extend(field("Field"));
            items
        }
        Role::Accumulator => names(&ACCUMULATORS, "Accumulator", "keyword"),
        Role::Lookup => names(&LOOKUP, "$lookup option", "property"),
        Role::Index => names(&INDEX_OPTIONS, "Index option", "property"),
        Role::Command if first => names(&COMMANDS, "Command", "keyword"),
        Role::Command | Role::Plain | Role::Many(_) | Role::Pipeline => vec![],
    }
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
            let analysis = analyze(at.body(), action);
            let after = &at.text[at.pos..];
            let after = after.split('\n').next().unwrap_or("");
            let items = match analysis.want {
                Want::Keys {
                    role,
                    quoted,
                    first,
                } => {
                    let fields = known.fields.get(collection).map_or(&[][..], Vec::as_slice);
                    key_items(role, quoted, after, fields, first, action)
                }
                Want::Nothing => vec![],
            };
            let after_word = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
                .unwrap_or(after.len());
            finish(
                at.pos - analysis.fragment.len(),
                at.pos + after_word,
                &analysis.fragment,
                items,
            )
        }
        _ => at.none(),
    }
}

/// Whether the cursor of a hints request needs collections and fields of a collection.
pub fn needs(check: &Check, pos: usize) -> Option<(bool, Option<String>)> {
    let at = At::new(&check.text, pos);
    if at.in_response() {
        return None;
    }
    let collection = check.text_of("collection").trim();
    match at.name() {
        Some("collection" | "action") => Some((true, None)),
        Some("payload") if !collection.is_empty() && !collection.contains("{{") => {
            match analyze(at.body(), check.text_of("action").trim()).want {
                Want::Keys {
                    role: Role::Filter | Role::Fields | Role::Group,
                    ..
                } => Some((false, Some(collection.to_owned()))),
                _ => None,
            }
        }
        _ => None,
    }
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

/// Payload tokens of a line: the JSON lexer with `$operators` styled.
fn payload_tokens(line: &str, state: &mut json::State) -> Vec<Token> {
    let mut tokens = json::tokens(line, state);
    for token in &mut tokens {
        let start = token.start as usize;
        let text = &line[start..start + token.len as usize];
        if token.kind == "property" && text.trim_matches(['"', '\'']).starts_with('$') {
            token.kind = "keyword".into();
        }
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
