//! A line-based reading of YAML for the `#payload`: tokens of a line, and the path of the key a
//! line is in. Lenient: it works on text that is being typed.

use crate::discovery::openapi::Step;
use services_sdk::lang::placeholder;
use services_sdk::{Token, lang};

/// The tokenizer state between lines: the indent of the key a block scalar (`|`, `>`) belongs to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub block: Option<usize>,
}

impl State {
    pub fn encode(self) -> u8 {
        self.block.map_or(0, |indent| indent.min(254) as u8 + 1)
    }

    pub fn decode(byte: u8) -> Self {
        Self {
            block: byte.checked_sub(1).map(usize::from),
        }
    }
}

/// A line read as YAML: the `- ` list markers, the indent of its content, and its `key: value`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line<'a> {
    /// Byte columns of the list dashes before the content.
    pub dashes: Vec<usize>,
    /// The column the content (key or scalar) starts at.
    pub indent: usize,
    /// The unquoted key and its byte range.
    pub key: Option<(&'a str, usize, usize)>,
    /// The text after the colon, trimmed; with its start column.
    pub value: &'a str,
    pub value_start: usize,
}

/// Reads `line`; `None` for a blank or comment line.
pub fn read(line: &str) -> Option<Line<'_>> {
    let trimmed = line.trim_start_matches(' ');
    if trimmed.trim().is_empty() || trimmed.starts_with('#') || trimmed.starts_with('\t') {
        return None;
    }
    let mut read = Line::default();
    let mut pos = line.len() - trimmed.len();
    while let Some(rest) = line[pos..].strip_prefix('-') {
        if !(rest.is_empty() || rest.starts_with(' ')) {
            break;
        }
        read.dashes.push(pos);
        pos += 1 + (rest.len() - rest.trim_start_matches(' ').len());
    }
    read.indent = pos;
    if let Some((end, colon)) = key_end(&line[pos..]) {
        let raw = &line[pos..pos + end];
        read.key = Some((raw.trim_matches(['"', '\'']), pos, pos + end));
        let after = &line[pos + colon + 1..];
        let value = after.trim_start();
        read.value_start = line.len() - value.len();
        read.value = value.trim_end();
    }
    Some(read)
}

/// The end of a key at the start of `text` and the byte of its colon.
fn key_end(text: &str) -> Option<(usize, usize)> {
    if let Some(quote) = text.chars().next().filter(|c| matches!(c, '"' | '\'')) {
        let close = 1 + text[1..].find(quote)?;
        let rest = &text[close + 1..];
        let colon = close + 1 + (rest.len() - rest.trim_start().len());
        return text[colon..].starts_with(':').then_some((close + 1, colon));
    }
    if text.starts_with(['{', '[', '|', '>', '&', '*', '!']) {
        return None;
    }
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        match c {
            ':' if chars.peek().is_none_or(|(_, next)| next.is_whitespace()) => {
                return Some((at, at));
            }
            '#' if at > 0 && text[..at].ends_with(' ') => return None,
            _ => {}
        }
    }
    None
}

/// The containers a key sits in, as YAML nests them by indentation.
#[derive(Clone, Debug, Default)]
pub struct Walker {
    stack: Vec<(usize, Step, usize)>,
    serial: usize,
}

/// Where a line is: the path to its mapping and which mapping that is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Place {
    pub path: Vec<Step>,
    pub mapping: usize,
}

impl Walker {
    fn enter(&mut self, dashes: &[usize], indent: usize) {
        for dash in dashes {
            while self
                .stack
                .last()
                .is_some_and(|(at, step, _)| *at > *dash || (*at == *dash && *step == Step::Item))
            {
                self.stack.pop();
            }
            self.serial += 1;
            self.stack.push((*dash, Step::Item, self.serial));
        }
        while self.stack.last().is_some_and(|(at, _, _)| *at >= indent) {
            self.stack.pop();
        }
    }

    fn place(&self) -> Place {
        Place {
            path: self.stack.iter().map(|(_, step, _)| step.clone()).collect(),
            mapping: self.stack.last().map_or(0, |(_, _, id)| *id),
        }
    }

    /// Where a line with these `dashes` and `indent` would be, without reading it.
    pub fn peek(&self, dashes: &[usize], indent: usize) -> Place {
        let mut walker = self.clone();
        walker.enter(dashes, indent);
        walker.place()
    }

    /// Reads a line; where it is. A key with no value opens a nested block.
    pub fn feed(&mut self, line: &Line) -> Place {
        self.enter(&line.dashes, line.indent);
        let place = self.place();
        if let Some((name, _, _)) = line.key {
            let value = line.value.split(" #").next().unwrap_or("").trim();
            if value.is_empty() || value.starts_with('#') {
                self.serial += 1;
                self.stack
                    .push((line.indent, Step::Key(name.to_owned()), self.serial));
            }
        }
        place
    }
}

fn push(out: &mut Vec<Token>, start: usize, end: usize, kind: &str) {
    if end > start {
        out.push(Token {
            start: start as u32,
            len: (end - start) as u32,
            kind: kind.into(),
        });
    }
}

/// A scalar value, `{{…}}` placeholders left unstyled.
fn value(line: &str, start: usize, end: usize, out: &mut Vec<Token>) {
    let text = &line[start..end];
    let kind = match text {
        "true" | "false" | "null" | "~" | "True" | "False" | "Null" => "atom",
        _ if text.starts_with(['&', '*']) => "variable-2",
        _ if text.parse::<f64>().is_ok() => "number",
        _ => "string",
    };
    let mut pos = start;
    while pos < end {
        if let Some(stop) = placeholder(line, pos) {
            pos = stop.min(end);
            continue;
        }
        let next = line[pos..end].find("{{").map_or(end, |at| pos + at);
        push(out, pos, next, kind);
        pos = next;
    }
}

/// Tokens of one line of a YAML payload: keys are `property`, scalars `string`, `number` or
/// `atom`, `|`/`>` block scalars `string`, comments `comment`.
pub fn tokens(line: &str, state: &mut State) -> Vec<Token> {
    let mut out = Vec::new();
    let indent = line.len() - line.trim_start_matches(' ').len();
    if let Some(block) = state.block {
        if line.trim().is_empty() || indent > block {
            push(&mut out, 0, line.len(), "string");
            return out;
        }
        state.block = None;
    }
    let trimmed = line.trim_end();
    if trimmed.starts_with('#') || trimmed[indent..].starts_with('#') {
        push(&mut out, indent, line.len(), "comment");
        return out;
    }
    if trimmed == "---" || trimmed == "..." {
        push(&mut out, 0, trimmed.len(), "operator");
        return out;
    }
    let Some(read) = read(line) else {
        return out;
    };
    for dash in &read.dashes {
        push(&mut out, *dash, dash + 1, "operator");
    }
    let mut end = trimmed.len();
    let mut start = read.indent;
    if let Some((_, from, to)) = read.key {
        let quoted = line[from..].starts_with(['"', '\'']);
        let kind = if quoted {
            "string property"
        } else {
            "property"
        };
        push(&mut out, from, to, kind);
        start = read.value_start;
    } else if read.dashes.is_empty() {
        return out;
    }
    if start >= end {
        return out;
    }
    let mut comment = None;
    if let Some(at) = comment_start(&line[start..end]) {
        comment = Some((start + at, end));
        end = start + at;
    }
    end = start + line[start..end].trim_end().len();
    if read.key.is_some() && line[start..end].starts_with(['|', '>']) {
        push(&mut out, start, end, "operator");
        state.block = Some(read.indent);
    } else {
        value(line, start, end, &mut out);
    }
    if let Some((from, to)) = comment {
        push(&mut out, from, to, "comment");
    }
    out
}

/// Where a trailing ` # comment` starts in a value.
fn comment_start(text: &str) -> Option<usize> {
    let mut quote = None;
    let mut previous = ' ';
    for (at, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, '#') if previous.is_whitespace() => return Some(at),
            _ => {}
        }
        previous = c;
    }
    None
}

/// The byte where the word before the cursor starts, for completing a key or a value.
pub fn word_start(before: &str) -> usize {
    lang::token_start(before)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styles(line: &str, state: &mut State) -> Vec<(String, String)> {
        tokens(line, state)
            .iter()
            .map(|t| {
                let start = t.start as usize;
                (
                    line[start..start + t.len as usize].to_owned(),
                    t.kind.clone(),
                )
            })
            .collect()
    }

    fn pair(text: &str, kind: &str) -> (String, String) {
        (text.to_owned(), kind.to_owned())
    }

    #[test]
    fn lines_tokenize_keys_scalars_and_comments() {
        let mut state = State::default();
        assert_eq!(
            styles("  replicas: 3 # more", &mut state),
            [
                pair("replicas", "property"),
                pair("3", "number"),
                pair("# more", "comment")
            ]
        );
        assert_eq!(
            styles("- name: web", &mut state),
            [
                pair("-", "operator"),
                pair("name", "property"),
                pair("web", "string")
            ]
        );
        assert_eq!(
            styles("paused: true", &mut state),
            [pair("paused", "property"), pair("true", "atom")]
        );
        assert_eq!(styles("---", &mut state), [pair("---", "operator")]);
        assert_eq!(
            styles("  image: \"a: b\"", &mut state),
            [pair("image", "property"), pair("\"a: b\"", "string")]
        );
        assert_eq!(
            styles("name: a-{{x}}-b", &mut state),
            [
                pair("name", "property"),
                pair("a-", "string"),
                pair("-b", "string")
            ]
        );
    }

    #[test]
    fn block_scalars_are_strings_until_the_indent_returns() {
        let mut state = State::default();
        let first = styles("  script: |", &mut state);
        assert_eq!(first, [pair("script", "property"), pair("|", "operator")]);
        assert_eq!(State::decode(state.encode()), state);
        assert_eq!(
            styles("    echo: not a key", &mut state),
            [pair("    echo: not a key", "string")]
        );
        assert_eq!(
            styles("  next: 1", &mut state),
            [pair("next", "property"), pair("1", "number")]
        );
    }

    #[test]
    fn the_walker_follows_nesting_and_list_items() {
        let text = "spec:\n  template:\n    spec:\n      containers:\n      - name: a\n        image: x\n      - name: b\n        ";
        let mut walker = Walker::default();
        let mut places = Vec::new();
        for line in text.lines() {
            if let Some(read) = read(line) {
                places.push((read.key.map(|k| k.0.to_owned()), walker.feed(&read)));
            }
        }
        let keys = |place: &Place| {
            place
                .path
                .iter()
                .map(|step| match step {
                    Step::Key(name) => name.as_str(),
                    Step::Item => "-",
                })
                .collect::<Vec<_>>()
                .join("/")
        };
        assert_eq!(keys(&places[4].1), "spec/template/spec/containers/-");
        assert_eq!(places[4].1.mapping, places[5].1.mapping);
        assert_ne!(places[4].1.mapping, places[6].1.mapping);
        let here = walker.peek(&[], 8);
        assert_eq!(keys(&here), "spec/template/spec/containers/-");
        assert_eq!(here.mapping, places[6].1.mapping);
        assert_eq!(
            keys(&walker.peek(&[6], 8)),
            "spec/template/spec/containers/-"
        );
        assert_ne!(walker.peek(&[6], 8).mapping, places[6].1.mapping);
        assert_eq!(keys(&walker.peek(&[], 2)), "spec");
    }
}
