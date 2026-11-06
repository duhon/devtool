//! JSON: a line-state tokenizer lenient enough for JSON Lines and JSON-like text
//! (comments, single quotes, bare words), bracket folds, and body completion against a
//! field schema (`{name, kind, type, format, enum, required, readOnly, fields, items}`).

use super::{finish, item, placeholder, token_start};
use crate::{Completions, Fold, Item, Token};
use serde_json::Value;
use std::collections::BTreeSet;

/// The tokenizer state between lines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// The open `{`, `[` and `(`, innermost last.
    pub open: Vec<u8>,
    /// In an object, between `{` or `,` and the next `:`.
    pub key: bool,
    pub comment: bool,
    /// The quote of a string the previous line ended inside an escape of.
    pub string: Option<u8>,
}

impl State {
    /// Appends the state's bytes: flags, quote, open brackets.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(u8::from(self.key) | u8::from(self.comment) << 1);
        out.push(self.string.unwrap_or(0));
        out.extend(&self.open);
    }
    /// The state [`State::encode`] wrote; the default for too few bytes.
    pub fn decode(bytes: &[u8]) -> Self {
        let [flags, quote, open @ ..] = bytes else {
            return Self::default();
        };
        Self {
            open: open.to_vec(),
            key: flags & 1 != 0,
            comment: flags & 2 != 0,
            string: (*quote != 0).then_some(*quote),
        }
    }
}

/// The tokens of one line; `{{…}}` placeholders outside strings are left unstyled.
pub fn tokens(line: &str, state: &mut State) -> Vec<Token> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < line.len() {
        if !state.comment
            && state.string.is_none()
            && let Some(end) = placeholder(line, pos)
        {
            pos = end;
            continue;
        }
        let (end, style) = token(line, pos, state);
        if let Some(style) = style {
            out.push(super::token(pos, end, style));
        }
        pos = end;
    }
    out
}

/// One token from byte `pos`; returns its byte end and style.
pub fn token(text: &str, pos: usize, state: &mut State) -> (usize, Option<&'static str>) {
    let bytes = text.as_bytes();
    if state.comment {
        return block_comment(bytes, pos, state);
    }
    if let Some(quote) = state.string {
        return string(bytes, pos, quote, state);
    }
    let b = bytes[pos];
    match b {
        b'"' | b'\'' => return string(bytes, pos + 1, b, state),
        b'/' if bytes.get(pos + 1) == Some(&b'/') => return (text.len(), Some("comment")),
        b'/' if bytes.get(pos + 1) == Some(&b'*') => return block_comment(bytes, pos + 2, state),
        b'{' | b'[' | b'(' => {
            state.open.push(b);
            state.key = b == b'{';
        }
        b'}' | b']' | b')' => {
            let opener = match b {
                b'}' => b'{',
                b']' => b'[',
                _ => b'(',
            };
            if state.open.last() == Some(&opener) {
                state.open.pop();
            }
            state.key = false;
        }
        b',' => state.key = state.open.last() == Some(&b'{'),
        b':' => state.key = false,
        _ => return value(text, pos, state.key),
    }
    (pos + 1, None)
}

type Style = Option<&'static str>;

/// Whitespace, a number, a word, an operator run or any other character.
fn value(text: &str, pos: usize, key: bool) -> (usize, Style) {
    let bytes = text.as_bytes();
    let c = text[pos..].chars().next().unwrap_or(' ');
    let (end, style) = if c.is_whitespace() {
        return (super::run(text, pos, char::is_whitespace), None);
    } else if let Some(end) = number(bytes, pos) {
        (end, "number")
    } else if is_word(c) {
        let end = super::run(text, pos, |c| is_word(c) || c.is_ascii_digit());
        let atom = matches!(
            &text[pos..end],
            "true" | "false" | "null" | "NaN" | "Infinity" | "undefined"
        );
        (end, if atom { "atom" } else { "variable" })
    } else if is_operator(bytes[pos]) {
        // The run stops before a comment.
        let length = bytes[pos + 1..]
            .iter()
            .enumerate()
            .take_while(|&(i, &b)| {
                is_operator(b)
                    && !(b == b'/' && matches!(bytes.get(pos + i + 2), Some(b'/' | b'*')))
            })
            .count();
        return (pos + 1 + length, Some("operator"));
    } else {
        return (pos + c.len_utf8(), None);
    };
    (end, Some(if key { "property" } else { style }))
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$' || (!c.is_ascii() && !c.is_whitespace())
}

fn is_operator(b: u8) -> bool {
    b"+-*/=<>!&|?~^%@".contains(&b)
}

/// A block comment's text from byte `from` to its `*/` or the line end.
fn block_comment(bytes: &[u8], from: usize, state: &mut State) -> (usize, Style) {
    let close = bytes[from.min(bytes.len())..]
        .windows(2)
        .position(|pair| pair == b"*/");
    state.comment = close.is_none();
    (close.map_or(bytes.len(), |n| from + n + 2), Some("comment"))
}

/// A string's text from byte `i` to its closing `quote` or the line end; only a line that
/// ends inside an escape continues it.
fn string(bytes: &[u8], mut i: usize, quote: u8, state: &mut State) -> (usize, Style) {
    state.string = None;
    let end = loop {
        match bytes.get(i) {
            Some(b'\\') if i + 1 == bytes.len() => {
                state.string = Some(quote);
                break bytes.len();
            }
            Some(b'\\') => i += 2,
            Some(&b) if b == quote => break i + 1,
            Some(_) => i += 1,
            None => break bytes.len(),
        }
    };
    let style = if state.key {
        "string property"
    } else {
        "string"
    };
    (end.min(bytes.len()), Some(style))
}

/// The end of a number at byte `pos`: an optional `-`, digits, fraction and exponent, or hex.
fn number(bytes: &[u8], pos: usize) -> Option<usize> {
    let digits = |from: usize| {
        from + bytes[from..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count()
    };
    let start = pos + usize::from(bytes[pos] == b'-');
    if bytes.get(start) == Some(&b'0') && matches!(bytes.get(start + 1), Some(b'x' | b'X')) {
        let end = start
            + 2
            + bytes[start + 2..]
                .iter()
                .take_while(|b| b.is_ascii_hexdigit())
                .count();
        if end > start + 2 {
            return Some(end);
        }
    }
    let mut end = digits(start);
    if bytes.get(end) == Some(&b'.')
        && (end > start || bytes.get(end + 1).is_some_and(u8::is_ascii_digit))
    {
        end = digits(end + 1);
    }
    if end == start {
        return None;
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let sign = end + 1 + usize::from(matches!(bytes.get(end + 1), Some(b'+' | b'-')));
        let exponent = digits(sign);
        if exponent > sign {
            end = exponent;
        }
    }
    Some(end)
}

/// Folds of `{`/`[` blocks spanning lines: from the line of a line's first opener to the
/// line of its partner. Quotes are skipped; a string ends at its line's end.
pub fn folds<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<Fold> {
    let mut open: Vec<(usize, bool)> = Vec::new();
    let mut out = Vec::new();
    for (i, line) in lines.into_iter().enumerate() {
        let mut quote = None;
        let mut escape = false;
        let mut first = true;
        for c in line.chars() {
            if let Some(q) = quote {
                if c == q && !escape {
                    quote = None;
                }
                escape = c == '\\' && !escape;
                continue;
            }
            match c {
                '"' | '\'' => quote = Some(c),
                '{' | '[' => {
                    open.push((i, first));
                    first = false;
                }
                '}' | ']' => {
                    if let Some((start, true)) = open.pop()
                        && start < i
                    {
                        out.push(Fold {
                            start: start as u32,
                            end: i as u32,
                        });
                    }
                }
                _ => {}
            }
        }
    }
    out.sort_by_key(|fold| fold.start);
    out
}

/// A step from a JSON value to a nested one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    Field(String),
    Item,
}

/// The path to the innermost value open at the end of `source`, the keys its object
/// already uses and the key awaiting a value; `None` unless `source` starts a single object.
pub fn path(source: &str) -> Option<(Vec<Segment>, BTreeSet<String>, Option<String>)> {
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
                        Segment::Field(pending.take()?)
                    } else {
                        Segment::Item
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

/// The schema at `path` under `root` (`{fields: [...]}`).
pub fn at_path<'a>(root: &'a Value, path: &[Segment]) -> Option<&'a Value> {
    let mut schema = root;
    for segment in path {
        schema = match segment {
            Segment::Field(name) => schema["fields"]
                .as_array()?
                .iter()
                .find(|f| f["name"] == *name)?,
            Segment::Item => schema.get("items")?,
        };
    }
    Some(schema)
}

/// An object of the required, writable fields with placeholder values; `None` when one
/// has no obvious value.
pub fn required_body(fields: &Value) -> Option<Value> {
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

fn indent(before: &str) -> &str {
    &before[..before.len() - before.trim_start_matches([' ', '\t']).len()]
}

/// `{}` across three lines, indented like the line `before` the cursor, with the cursor
/// inside.
pub fn object_snippet(before: &str) -> String {
    block_snippet(before, '{', '}')
}

/// `open` and `close` across three lines with the cursor on the middle one.
pub fn block_snippet(before: &str, open: char, close: char) -> String {
    let indent = indent(before);
    format!("{open}\n{indent}  $0\n{indent}{close}")
}

/// Completion in a JSON `body` at byte `offset` from the body's fields (`[{name, kind,
/// …}]`): a required-fields template for an empty body, enum values, array items and
/// unused fields. Offsets are within `body`.
pub fn complete(fields: &Value, body: &str, offset: usize) -> Completions {
    let line_start = body[..offset].rfind('\n').map_or(0, |n| n + 1);
    let before = &body[line_start..offset];
    let prefix = &body[..offset];
    let none = || finish(offset, offset, "", vec![]);
    if prefix.trim().is_empty() {
        let mut items = vec![];
        if body.trim().is_empty()
            && let Some(template) = required_body(fields)
            && template.as_object().is_some_and(|o| !o.is_empty())
        {
            let text = serde_json::to_string_pretty(&template)
                .unwrap_or_default()
                .replace('\n', &format!("\n{}", indent(before)))
                .replacen(": \"\"", ": \"$0\"", 1);
            items.push(item(text, "Required fields", "snippet"));
        }
        items.push(item(object_snippet(before), "JSON object", "snippet"));
        return finish(offset, offset, "", items);
    }
    let root = serde_json::json!({ "fields": fields });
    if let Some(result) = enum_completion(&root, body, offset) {
        return result;
    }
    let start = token_start(before);
    let query = &before[start..];
    let quote = before[..start].ends_with('"');
    let from = line_start + start - usize::from(quote);
    let prior = &prefix[..from];
    let trimmed = prior.trim_end();
    let Some((path, used, _)) = path(prior) else {
        return none();
    };
    let Some(schema) = at_path(&root, &path) else {
        return none();
    };
    if schema["kind"] == "array" {
        let next = body[offset..].trim_start().chars().next();
        let items = if query.is_empty()
            && !quote
            && trimmed.ends_with(['[', ','])
            && next.is_none_or(|c| matches!(c, ']' | ','))
        {
            let kind = if schema["items"]["kind"] == "bare" {
                schema["items"]["type"].as_str()
            } else {
                schema["items"]["kind"].as_str()
            };
            match kind {
                Some("object") => vec![item(object_snippet(before), "JSON object", "snippet")],
                Some("array") => vec![item(
                    block_snippet(before, '[', ']'),
                    "JSON array",
                    "snippet",
                )],
                Some("string") => vec![item("\"$0\"", "JSON string", "value")],
                Some("number" | "integer") => vec![item("0", "JSON number", "value")],
                Some("boolean") => vec![
                    item("true", "JSON boolean", "value"),
                    item("false", "JSON boolean", "value"),
                ],
                _ => vec![],
            }
        } else {
            vec![]
        };
        return finish(offset, offset, "", items);
    }
    let needs_comma = !trimmed.ends_with(['{', ',']);
    if needs_comma
        && (trimmed.is_empty()
            || trimmed.ends_with(':')
            || !before[..start - usize::from(quote)].trim().is_empty())
    {
        return finish(from, offset, "", vec![]);
    }
    let items = schema["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|field| field["readOnly"] != true)
        .filter_map(|f| {
            let name = f["name"].as_str()?;
            if used.contains(name) {
                return None;
            }
            let value =
                if f["enum"].is_array() && matches!(f["kind"].as_str(), Some("object" | "array")) {
                    String::new()
                } else {
                    match f["kind"].as_str().unwrap_or("bare") {
                        "string" => "\"$0\"".into(),
                        "object" => object_snippet(before),
                        "array" => "[$0]".into(),
                        _ => String::new(),
                    }
                };
            let detail = match f["format"].as_str() {
                Some("date-time") => "date-time (ISO 8601)",
                Some("date") => "date (YYYY-MM-DD)",
                Some(format) => format,
                None => f["type"]
                    .as_str()
                    .unwrap_or_else(|| match f["kind"].as_str() {
                        Some("bare") => "value (type unavailable)",
                        Some(kind) => kind,
                        None => "",
                    }),
            };
            let choice = f["enum"].is_array() || f["items"]["enum"].is_array();
            let detail = if f["enum"].is_array() {
                if f["type"].is_string() || f["format"].is_string() {
                    format!("{detail} · enum")
                } else {
                    "enum".into()
                }
            } else if f["items"]["enum"].is_array() {
                "array<enum>".into()
            } else {
                detail.into()
            };
            let mut field = item(
                format!("{}: {value}", Value::String(name.into())),
                &detail,
                "field",
            );
            if needs_comma {
                field.insert = format!(",{}{}", &prior[trimmed.len()..], field.insert);
            }
            field.next = choice;
            Some(field)
        })
        .collect();
    let from = if needs_comma { trimmed.len() } else { from };
    finish(from, offset, query, items)
}

/// Enum values for the value at byte `offset` of `body`, replacing the whole value.
pub fn enum_completion(root: &Value, body: &str, offset: usize) -> Option<Completions> {
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
    let (mut path, _, pending) = path(&body[..start])?;
    if let Some(name) = pending {
        path.push(Segment::Field(name));
    } else if at_path(root, &path)?["kind"] == "array" {
        path.push(Segment::Item);
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
                .find(|c: char| c.is_whitespace() || matches!(c, ',' | '}' | ']'))
                .unwrap_or(remaining.len())
    };
    let line_start = body[..offset].rfind('\n').map_or(0, |n| n + 1);
    let line_end = offset + body[offset..].find('\n').unwrap_or(body.len() - offset);
    if start < line_start || end > line_end {
        return None;
    }
    let query = &body[start..offset];
    let query = if query == "\""
        || serde_json::from_str::<Value>(&body[start..end]).is_ok_and(|v| values.contains(&v))
    {
        ""
    } else {
        query
    };
    Some(finish(
        start,
        end,
        query,
        values
            .iter()
            .map(|value| item(value.to_string(), "enum value", "value"))
            .collect(),
    ))
}

#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Key,
    Colon,
    Value,
    Comma,
}

/// Schema-free snippets for the cursor between `source` (the text before it) and `after`;
/// `before` is the cursor line's text before it.
pub fn templates(source: &str, before: &str, after: &str) -> Vec<Item> {
    let chars: Vec<_> = source.chars().collect();
    let mut stack: Vec<(char, Slot)> = vec![];
    let mut root = false;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            c if c.is_whitespace() => {}
            '{' | '[' => {
                if let Some((_, slot)) = stack.last_mut() {
                    if *slot != Slot::Value {
                        return vec![];
                    }
                    *slot = Slot::Comma;
                } else if root || chars[i] != '{' {
                    return vec![];
                } else {
                    root = true;
                }
                stack.push(if chars[i] == '{' {
                    ('}', Slot::Key)
                } else {
                    (']', Slot::Value)
                });
            }
            '}' | ']' => {
                let Some((closing, slot)) = stack.pop() else {
                    return vec![];
                };
                if closing != chars[i]
                    || !(matches!(slot, Slot::Comma | Slot::Key)
                        || closing == ']' && slot == Slot::Value)
                {
                    return vec![];
                }
            }
            ':' => {
                let Some((_, slot)) = stack.last_mut() else {
                    return vec![];
                };
                if *slot != Slot::Colon {
                    return vec![];
                }
                *slot = Slot::Value;
            }
            ',' => {
                let Some((closing, slot)) = stack.last_mut() else {
                    return vec![];
                };
                if *slot != Slot::Comma {
                    return vec![];
                }
                *slot = if *closing == '}' {
                    Slot::Key
                } else {
                    Slot::Value
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
                    Slot::Key => Slot::Colon,
                    Slot::Value => Slot::Comma,
                    _ => return vec![],
                };
                continue;
            }
            _ => {
                let Some((_, slot)) = stack.last_mut() else {
                    return vec![];
                };
                if *slot != Slot::Value {
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
                *slot = Slot::Comma;
                continue;
            }
        }
        i += 1;
    }
    let next = after.trim_start().chars().next();
    if !root {
        return if next.is_none() {
            vec![item(object_snippet(before), "JSON object", "snippet")]
        } else {
            vec![]
        };
    }
    match stack.last().map(|(_, slot)| slot) {
        Some(Slot::Key) if next.is_none_or(|c| c == '}') => {
            vec![item("\"$0\"", "JSON property", "snippet")]
        }
        Some(Slot::Value) if next.is_none_or(|c| matches!(c, '}' | ']' | ',')) => vec![
            item("\"$0\"", "JSON string", "snippet"),
            item(object_snippet(before), "JSON object", "snippet"),
            item(block_snippet(before, '[', ']'), "JSON array", "snippet"),
        ],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The styled tokens of `text`, lines tokenized in order with one state.
    fn styled(text: &str) -> Vec<(&str, &'static str)> {
        let mut state = State::default();
        let mut out = Vec::new();
        for line in text.split('\n') {
            let mut pos = 0;
            while pos < line.len() {
                let (end, style) = token(line, pos, &mut state);
                assert!(end > pos && line.is_char_boundary(end), "{line:?} at {pos}");
                if let Some(style) = style {
                    out.push((&line[pos..end], style));
                }
                pos = end;
            }
        }
        out
    }

    #[test]
    fn keys_and_values_in_nested_objects_and_arrays() {
        assert_eq!(
            styled(r#"{"a": {"b": [1, "x", {"c": true}]}, "d": ("e", null), "f": [}, "g"]}"#),
            [
                (r#""a""#, "string property"),
                (r#""b""#, "string property"),
                ("1", "number"),
                (r#""x""#, "string"),
                (r#""c""#, "string property"),
                ("true", "atom"),
                (r#""d""#, "string property"),
                (r#""e""#, "string"),
                ("null", "atom"),
                (r#""f""#, "string property"),
                (r#""g""#, "string"),
            ]
        );
    }

    #[test]
    fn bare_word_keys_and_negative_numbers() {
        assert_eq!(
            styled("{key: 'v', 2: [-1.5e3, -.5, 0x1F, -x], $k\u{e9}: -Infinity}"),
            [
                ("key", "property"),
                ("'v'", "string"),
                ("2", "property"),
                ("-1.5e3", "number"),
                ("-.5", "number"),
                ("0x1F", "number"),
                ("-", "operator"),
                ("x", "variable"),
                ("$k\u{e9}", "property"),
                ("-", "operator"),
                ("Infinity", "atom"),
            ]
        );
    }

    #[test]
    fn comments_span_lines_and_strings_continue_only_after_a_trailing_backslash() {
        assert_eq!(
            styled("{/* a\n b */ \"k\": // c\n[\"one\\\ntwo\", \"x\n3, \"p\\\\\n4]}"),
            [
                ("/* a", "comment"),
                (" b */", "comment"),
                ("\"k\"", "string property"),
                ("// c", "comment"),
                ("\"one\\", "string"),
                ("two\"", "string"),
                ("\"x", "string"),
                ("3", "number"),
                ("\"p\\\\", "string"),
                ("4", "number"),
            ]
        );
    }

    #[test]
    fn arbitrary_text_never_panics() {
        styled("é\u{1F600}\\ -- 0x 1e+ - . @@ x\u{301}\n''' \"\\\n/*/\n*/ \"\\\\\n'");
    }

    #[test]
    fn placeholders_are_skipped_and_states_round_trip() {
        let mut state = State::default();
        let line = r#"{"id": {{var:id}}, "n": "{{x}}"}"#;
        assert_eq!(
            crate::lang::styled(line, &tokens(line, &mut state)),
            [
                ("\"id\"", "string property"),
                ("\"n\"", "string property"),
                ("\"{{x}}\"", "string"),
            ]
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
        );
        assert_eq!(state, State::default());
        for state in [
            State::default(),
            State {
                open: b"{[(".to_vec(),
                key: true,
                comment: true,
                string: Some(b'\''),
            },
        ] {
            let mut bytes = vec![];
            state.encode(&mut bytes);
            assert_eq!(State::decode(&bytes), state);
        }
    }

    #[test]
    fn folds_span_lines_from_the_first_opener() {
        let text = "{\n  \"a\": [1,\n    2], \"s\": \"{\",\n  \"b\": {\"c\": {\n  }}\n}\n[]";
        assert_eq!(
            folds(text.lines())
                .iter()
                .map(|f| (f.start, f.end))
                .collect::<Vec<_>>(),
            [(0, 5), (1, 2), (3, 4)]
        );
    }

    fn fields() -> Value {
        json!([
            {"name": "name", "kind": "string", "type": "string", "required": true},
            {"name": "id", "kind": "bare", "type": "integer", "readOnly": true},
            {"name": "status", "kind": "string", "type": "string", "enum": ["available", "sold"], "required": true},
            {"name": "createdAt", "kind": "string", "type": "string", "format": "date-time"},
            {"name": "tags", "kind": "array", "items": {"kind": "string", "enum": ["a", "b"]}},
            {"name": "owner", "kind": "object", "required": true, "fields": [
                {"name": "email", "kind": "string", "required": true}
            ]},
            {"name": "count", "kind": "bare", "type": "integer"},
            {"name": "photos", "kind": "array", "items": {"kind": "object", "fields": []}}
        ])
    }

    fn inserts(c: &Completions) -> Vec<&str> {
        c.items.iter().map(|i| i.insert.as_str()).collect()
    }

    #[test]
    fn empty_bodies_offer_the_required_fields() {
        let c = complete(&fields(), "", 0);
        assert_eq!(c.items[0].detail, "Required fields");
        assert_eq!(
            c.items[0].insert,
            "{\n  \"name\": \"$0\",\n  \"status\": \"available\",\n  \"owner\": {\n    \"email\": \"\"\n  }\n}"
        );
        assert_eq!(
            c.items[0].label,
            r#"{"name": "", "status": "available", "owner": {"email": ""}}"#
        );
        assert_eq!(c.items[1].insert, "{\n  $0\n}");
        assert_eq!(inserts(&complete(&Value::Null, "", 0)), ["{\n  $0\n}"]);
    }

    #[test]
    fn fields_skip_used_and_read_only_ones() {
        let body = "{\n  \"name\": \"x\",\n  \n}";
        let c = complete(&fields(), body, 19);
        assert_eq!((c.from, c.to), (19, 19));
        assert_eq!(
            inserts(&c),
            [
                "\"status\": \"$0\"",
                "\"createdAt\": \"$0\"",
                "\"tags\": [$0]",
                "\"owner\": {\n    $0\n  }",
                "\"count\": ",
                "\"photos\": [$0]"
            ]
        );
        // The value of a field that takes one of a list is asked for next.
        let next: Vec<_> = c.items.iter().map(|i| i.next).collect();
        assert_eq!(next, [true, false, true, false, false, false]);
        let details: Vec<_> = c.items.iter().map(|i| i.detail.as_str()).collect();
        assert_eq!(
            details,
            [
                "string · enum",
                "date-time (ISO 8601)",
                "array<enum>",
                "object",
                "integer",
                "array"
            ]
        );
        let c = complete(&fields(), "{\"st", 4);
        assert_eq!((c.from, c.to), (1, 4));
        assert_eq!(inserts(&c), ["\"status\": \"$0\""]);
        let c = complete(&fields(), "{\"owner\": {", 11);
        assert_eq!(inserts(&c), ["\"email\": \"$0\""]);
    }

    #[test]
    fn a_field_after_a_value_adds_the_comma() {
        let body = "{\n  \"name\": \"x\"\n  co\n}";
        let c = complete(&fields(), body, 20);
        assert_eq!((c.from, c.to), (15, 20));
        assert_eq!(inserts(&c), [",\n  \"count\": "]);
        assert_eq!(c.items[0].label, "\"count\": ");
        assert!(complete(&fields(), "{\"name\": x", 10).items.is_empty());
    }

    #[test]
    fn enums_and_array_items() {
        let body = "{\"status\": \"so\"}";
        let c = complete(&fields(), body, 14);
        assert_eq!((c.from, c.to), (11, 15));
        assert_eq!(inserts(&c), ["\"sold\""]);
        assert_eq!(c.items[0].detail, "enum value");
        let c = complete(&fields(), "{\"status\": ", 11);
        assert_eq!(inserts(&c), ["\"available\"", "\"sold\""]);
        let c = complete(&fields(), "{\"tags\": [", 10);
        assert_eq!(inserts(&c), ["\"a\"", "\"b\""]);
        let c = complete(&fields(), "{\"photos\": [", 12);
        assert_eq!(inserts(&c), ["{\n  $0\n}"]);
    }

    #[test]
    fn templates_follow_the_json_grammar() {
        let labels = |source: &str, after: &str| {
            templates(source, "", after)
                .into_iter()
                .map(|i| i.detail)
                .collect::<Vec<_>>()
        };
        assert_eq!(labels("", ""), ["JSON object"]);
        assert_eq!(labels("{", "}"), ["JSON property"]);
        assert_eq!(
            labels("{\"a\": ", "}"),
            ["JSON string", "JSON object", "JSON array"]
        );
        assert!(labels("{\"a\" ", "").is_empty());
        assert!(labels("{\"a\": 1 ", "").is_empty());
    }
}
