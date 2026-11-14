//! Highlighting and folds of a GraphQL check: the query (also before the first marker) as
//! GraphQL, `#variables` and JSON responses as JSON, the endpoint as an address.

use serde_json::Value;
use services_sdk::lang::{endpoint, json, key_value, marker, placeholder};
use services_sdk::{Fold, Line, Token};

/// The part of a check a line is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Area {
    /// Before the first marker: the query.
    Prelude,
    Endpoint,
    Headers,
    Variables,
    Query,
    Options,
    Other,
    /// After `#responce`, before its first non-blank line.
    Response,
    ResponseJson,
    ResponseText,
}

impl Area {
    const ALL: [Area; 10] = [
        Area::Prelude,
        Area::Endpoint,
        Area::Headers,
        Area::Variables,
        Area::Query,
        Area::Options,
        Area::Other,
        Area::Response,
        Area::ResponseJson,
        Area::ResponseText,
    ];

    fn json(self) -> bool {
        matches!(self, Area::Variables | Area::ResponseJson)
    }

    fn graphql(self) -> bool {
        matches!(self, Area::Prelude | Area::Query)
    }

    fn response(self) -> bool {
        matches!(
            self,
            Area::Response | Area::ResponseJson | Area::ResponseText
        )
    }

    /// The area of `line` after `self`, and whether the line is a marker.
    fn next(self, line: &str) -> (Area, bool) {
        if let Some((name, _)) = marker(line) {
            let response = matches!(name.as_str(), "responce" | "response");
            if response || !self.response() {
                let area = match name.as_str() {
                    _ if response => Area::Response,
                    "target" => Area::Endpoint,
                    "metadata" | "headers" => Area::Headers,
                    "variables" => Area::Variables,
                    "payload" => Area::Query,
                    "options" => Area::Options,
                    _ => Area::Other,
                };
                return (area, true);
            }
        }
        if self == Area::Response && !line.trim().is_empty() {
            let json = line.trim_start().starts_with(['{', '[']);
            return (
                if json {
                    Area::ResponseJson
                } else {
                    Area::ResponseText
                },
                false,
            );
        }
        (self, false)
    }
}

/// What the previous GraphQL token makes of the next name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
enum Context {
    #[default]
    None,
    FragmentName,
    FragmentOn,
    SpreadName,
    TypeCondition,
}

/// The GraphQL tokenizer's state between lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    context: Context,
    pub(crate) block_string: bool,
    depth: u32,
}

impl State {
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.context as u8);
        out.push(self.block_string as u8);
        out.extend(self.depth.to_le_bytes());
    }

    pub(crate) fn decode(bytes: &[u8]) -> Self {
        let context = match bytes.first() {
            Some(1) => Context::FragmentName,
            Some(2) => Context::FragmentOn,
            Some(3) => Context::SpreadName,
            Some(4) => Context::TypeCondition,
            _ => Context::None,
        };
        let depth = bytes
            .get(2..6)
            .and_then(|depth| depth.try_into().ok())
            .map_or(0, u32::from_le_bytes);
        Self {
            context,
            block_string: bytes.get(1) == Some(&1),
            depth,
        }
    }
}

const KEYWORDS: [&str; 16] = [
    "query",
    "mutation",
    "subscription",
    "fragment",
    "on",
    "schema",
    "scalar",
    "type",
    "interface",
    "union",
    "enum",
    "input",
    "directive",
    "extend",
    "implements",
    "repeatable",
];

const OBJECT_FIELD: &str = "graphql-object-field";

fn token(start: usize, end: usize, kind: &str) -> Token {
    Token {
        start: start as u32,
        len: (end - start) as u32,
        kind: kind.into(),
    }
}

fn word(text: &str, pos: usize) -> usize {
    text[pos..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .map_or(text.len(), |n| pos + n)
}

/// The end of a `"…"` string that starts at `pos`; the line's end when it is not closed.
fn quoted(text: &str, pos: usize) -> usize {
    let mut escaped = false;
    for (i, c) in text[pos + 1..].char_indices() {
        if c == '"' && !escaped {
            return pos + 1 + i + 1;
        }
        escaped = c == '\\' && !escaped;
    }
    text.len()
}

/// Whether a selection set opens at `pos` of `line`, after the field's arguments and
/// directives.
fn opens_selection(line: &str, mut pos: usize) -> bool {
    loop {
        let rest = line[pos..].trim_start_matches(|c: char| c.is_whitespace() || c == ',');
        pos = line.len() - rest.len();
        if rest.starts_with('@') {
            pos = word(line, pos + 1);
        } else if rest.starts_with('(') {
            let mut depth = 0;
            let mut at = pos;
            loop {
                match line[at..].chars().next() {
                    None | Some('#') => return false,
                    Some('"') => {
                        at = quoted(line, at);
                        continue;
                    }
                    Some('(') => depth += 1,
                    Some(')') => {
                        depth -= 1;
                        if depth == 0 {
                            pos = at + 1;
                            break;
                        }
                    }
                    Some(_) => {}
                }
                at += line[at..].chars().next().map_or(1, char::len_utf8);
            }
        } else {
            return rest.starts_with('{');
        }
    }
}

/// Tokens of a GraphQL line: keywords, names (`property`, arguments and object keys
/// `attribute`, fields followed by their selection set `graphql-object-field`),
/// `$variables`, `@directives`, strings, numbers, fragment names and type conditions.
pub fn tokens(line: &str, state: &mut State) -> Vec<Token> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < line.len() {
        if state.block_string {
            match line[pos..].find("\"\"\"") {
                Some(end) => {
                    state.block_string = false;
                    out.push(token(pos, pos + end + 3, "string"));
                    pos += end + 3;
                    continue;
                }
                None => {
                    out.push(token(pos, line.len(), "string"));
                    break;
                }
            }
        }
        if let Some(end) = placeholder(line, pos) {
            pos = end;
            continue;
        }
        let rest = &line[pos..];
        let c = rest.chars().next().unwrap_or(' ');
        if c.is_whitespace() {
            pos += c.len_utf8();
            continue;
        }
        if c == '#' {
            out.push(token(pos, line.len(), "comment"));
            break;
        }
        if c == ',' {
            pos += 1;
            continue;
        }
        if rest.starts_with("...") {
            state.context = Context::SpreadName;
            pos += 3;
            continue;
        }
        let context = std::mem::take(&mut state.context);
        match c {
            '{' => state.depth += 1,
            '}' => state.depth = state.depth.saturating_sub(1),
            _ => {}
        }
        if let Some(content) = rest.strip_prefix("\"\"\"") {
            let end = match content.find("\"\"\"") {
                Some(end) => pos + 3 + end + 3,
                None => {
                    state.block_string = true;
                    line.len()
                }
            };
            out.push(token(pos, end, "string"));
            pos = end;
            continue;
        }
        if c == '"' {
            let end = quoted(line, pos);
            out.push(token(pos, end, "string"));
            pos = end;
            continue;
        }
        if c == '$' || c == '@' {
            let end = word(line, pos + 1);
            let kind = if c == '$' { "variable-3" } else { "atom" };
            out.push(token(pos, end, kind));
            pos = end;
            continue;
        }
        let digit = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit());
        if c.is_ascii_digit() || (c == '-' && digit(rest.chars().nth(1))) {
            let end = rest
                .find(|c: char| !(c.is_ascii_digit() || ".-+eE".contains(c)))
                .map_or(line.len(), |n| pos + n);
            out.push(token(pos, end, "number"));
            pos = end;
            continue;
        }
        let end = word(line, pos);
        if end == pos {
            pos += c.len_utf8();
            continue;
        }
        let value = &line[pos..end];
        let kind = match context {
            Context::FragmentName => {
                state.context = Context::FragmentOn;
                "graphql-fragment"
            }
            Context::SpreadName | Context::FragmentOn if value == "on" => {
                state.context = Context::TypeCondition;
                "keyword"
            }
            Context::SpreadName => "graphql-fragment",
            Context::TypeCondition => "graphql-type-condition",
            _ => {
                if value == "fragment" && state.depth == 0 {
                    state.context = Context::FragmentName;
                }
                if KEYWORDS.contains(&value) {
                    "keyword"
                } else if ["true", "false", "null"].contains(&value) {
                    "atom"
                } else if line[end..].trim_start().starts_with(':') {
                    "attribute"
                } else if state.depth > 0 && opens_selection(line, end) {
                    OBJECT_FIELD
                } else {
                    "property"
                }
            }
        };
        out.push(token(pos, end, kind));
        pos = end;
    }
    out
}

fn decode(state: &[u8]) -> (Area, json::State, State) {
    match state.split_first() {
        Some((area, rest)) => {
            let area = Area::ALL
                .get(*area as usize)
                .copied()
                .unwrap_or(Area::Other);
            if area.graphql() {
                (area, json::State::default(), State::decode(rest))
            } else {
                (area, json::State::decode(rest), State::default())
            }
        }
        None => (Area::Prelude, json::State::default(), State::default()),
    }
}

fn whole(line: &str, kind: &str) -> Vec<Token> {
    if line.is_empty() {
        return vec![];
    }
    vec![token(0, line.len(), kind)]
}

pub fn highlight(state: &[u8], lines: &[String]) -> Vec<Line> {
    let (mut area, mut json, mut graphql) = decode(state);
    lines
        .iter()
        .map(|line| {
            let (next, is_marker) = area.next(line);
            area = next;
            let tokens = if is_marker {
                json = json::State::default();
                graphql = State::default();
                whole(line, "comment")
            } else {
                match area {
                    Area::Prelude | Area::Query => tokens(line, &mut graphql),
                    Area::Variables | Area::ResponseJson => json::tokens(line, &mut json),
                    Area::Endpoint => endpoint::tokens(line, false),
                    Area::Headers | Area::Options => key_value::tokens(line),
                    _ => vec![],
                }
            };
            let mut state = vec![area as u8];
            if area.json() {
                json.encode(&mut state);
            } else if area.graphql() {
                graphql.encode(&mut state);
            }
            Line { tokens, state }
        })
        .collect()
}

/// The fields of a check's queries whose type, by `schema`, has fields — also before their
/// selection set is written: one line of tokens per line of `text`.
pub fn marks(schema: &Value, text: &str) -> Vec<Line> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<Line> = lines.iter().map(|_| Line::default()).collect();
    let mut area = Area::Prelude;
    let mut region: Vec<usize> = Vec::new();
    let flush = |region: &mut Vec<usize>, out: &mut Vec<Line>| {
        let Some(&first) = region.first() else {
            return;
        };
        let source = lines[first..=region[region.len() - 1]].join("\n");
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(i, c)| (c == b'\n').then_some(i + 1)),
        );
        for (start, end) in crate::language::object_fields(&source, schema) {
            let line = starts.partition_point(|at| *at <= start) - 1;
            let at = starts[line];
            out[first + line]
                .tokens
                .push(token(start - at, end - at, OBJECT_FIELD));
        }
        region.clear();
    };
    for (i, line) in lines.iter().enumerate() {
        let (next, is_marker) = area.next(line);
        if is_marker || next != area {
            flush(&mut region, &mut out);
        }
        if next.graphql() && !is_marker {
            region.push(i);
        }
        area = next;
    }
    flush(&mut region, &mut out);
    out
}

/// Folds of a GraphQL region from its first line `first`: each line's first `{` or `[` to
/// the line of its partner.
fn graphql_folds(lines: &[&str], first: usize, out: &mut Vec<Fold>) {
    let mut open: Vec<(usize, char)> = Vec::new();
    let mut folded = std::collections::BTreeSet::new();
    let mut block = false;
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let mut chars = line.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            if block {
                if line[at..].starts_with("\"\"\"") {
                    block = false;
                    chars.nth(1);
                }
                continue;
            }
            match c {
                '#' => break,
                '"' if line[at..].starts_with("\"\"\"") => {
                    block = true;
                    chars.nth(1);
                }
                '"' => {
                    let end = quoted(line, at);
                    while chars.peek().is_some_and(|(next, _)| *next < end) {
                        chars.next();
                    }
                }
                '{' | '[' => open.push((i, c)),
                '}' | ']' => {
                    let partner = if c == '}' { '{' } else { '[' };
                    if let Some(at) = open.iter().rposition(|(_, o)| *o == partner) {
                        let (start, _) = open.remove(at);
                        open.truncate(at);
                        if start < i {
                            found.push((start, i));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    // The outermost bracket of a line opens first and closes last.
    found.sort_by_key(|(start, end)| (*start, std::cmp::Reverse(*end)));
    for (start, end) in found {
        if folded.insert(start) {
            out.push(Fold {
                start: (first + start) as u32,
                end: (first + end) as u32,
            });
        }
    }
}

/// Folds of the query's brackets, the variables and JSON responses.
pub fn folds(text: &str) -> Vec<Fold> {
    let mut area = Area::Prelude;
    let mut out = Vec::new();
    let mut region: Vec<&str> = Vec::new();
    let mut start = 0;
    let mut flush = |region: &mut Vec<&str>, start: usize, graphql: bool| {
        if region.is_empty() {
            return;
        }
        if graphql {
            graphql_folds(region, start, &mut out);
            region.clear();
        } else {
            out.extend(json::folds(region.drain(..)).into_iter().map(|fold| Fold {
                start: fold.start + start as u32,
                end: fold.end + start as u32,
            }));
        }
    };
    for (i, line) in text.split('\n').enumerate() {
        let (next, is_marker) = area.next(line);
        if is_marker || next != area {
            flush(&mut region, start, area.graphql());
        }
        if (next.json() || next.graphql()) && !is_marker {
            if region.is_empty() {
                start = i;
            }
            region.push(line);
        }
        area = next;
    }
    flush(&mut region, start, area.graphql());
    out
}
