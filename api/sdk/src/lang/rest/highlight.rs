//! Highlighting and folds of a REST check: each section in its language, the response
//! region as JSON when it looks like JSON.

use super::Language;
use crate::lang::{endpoint, json, key_value, marker};
use crate::{Fold, Line, Token};

/// The part of a check a line is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Area {
    /// Before the first marker: the payload, JSON.
    Prelude,
    Endpoint,
    Action,
    Headers,
    Body,
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
        Area::Action,
        Area::Headers,
        Area::Body,
        Area::Options,
        Area::Other,
        Area::Response,
        Area::ResponseJson,
        Area::ResponseText,
    ];

    fn json(self) -> bool {
        matches!(self, Area::Prelude | Area::Body | Area::ResponseJson)
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
                    "action" => Area::Action,
                    "headers" => Area::Headers,
                    "payload" => Area::Body,
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

fn decode(state: &[u8]) -> (Area, json::State) {
    match state.split_first() {
        Some((area, rest)) => (
            Area::ALL
                .get(*area as usize)
                .copied()
                .unwrap_or(Area::Other),
            json::State::decode(rest),
        ),
        None => (Area::Prelude, json::State::default()),
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

/// The method of an `#action` line is a `property`.
fn method(lang: &Language, line: &str) -> Vec<Token> {
    let word = line.trim();
    if !lang.methods.iter().any(|m| m.eq_ignore_ascii_case(word)) {
        return vec![];
    }
    let start = line.len() - line.trim_start().len();
    vec![Token {
        start: start as u32,
        len: word.len() as u32,
        kind: "property".into(),
    }]
}

pub fn highlight(lang: &Language, state: &[u8], lines: &[String]) -> Vec<Line> {
    let (mut area, mut json) = decode(state);
    lines
        .iter()
        .map(|line| {
            let (next, is_marker) = area.next(line);
            area = next;
            let tokens = if is_marker {
                json = json::State::default();
                whole(line, "comment")
            } else if area.json() && !area.response() && line.trim_start().starts_with('#') {
                whole(line, "comment")
            } else {
                match area {
                    Area::Prelude | Area::Body | Area::ResponseJson => {
                        json::tokens(line, &mut json)
                    }
                    Area::Endpoint => endpoint::tokens(line, true),
                    Area::Action => method(lang, line),
                    Area::Headers | Area::Options => key_value::tokens(line),
                    _ => vec![],
                }
            };
            let mut state = vec![area as u8];
            if area.json() {
                json.encode(&mut state);
            }
            Line { tokens, state }
        })
        .collect()
}

/// JSON folds of the payload, the body and JSON responses.
pub fn folds(text: &str) -> Vec<Fold> {
    let mut area = Area::Prelude;
    let mut out = Vec::new();
    let mut region: Vec<&str> = Vec::new();
    let mut start = 0;
    let mut flush = |region: &mut Vec<&str>, start: usize| {
        out.extend(json::folds(region.drain(..)).into_iter().map(|fold| Fold {
            start: fold.start + start as u32,
            end: fold.end + start as u32,
        }));
    };
    for (i, line) in text.split('\n').enumerate() {
        let (next, is_marker) = area.next(line);
        if is_marker || next != area {
            flush(&mut region, start);
        }
        if next.json() && !is_marker {
            if region.is_empty() {
                start = i;
            }
            let comment = !next.response() && line.trim_start().starts_with('#');
            region.push(if comment { "" } else { line });
        }
        area = next;
    }
    flush(&mut region, start);
    out
}
