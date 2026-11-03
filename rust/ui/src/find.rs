//! Editor find bar logic that does not need a browser: the JavaScript RegExp a
//! query compiles to, match navigation, which matches may be replaced, and
//! replacement templates. Offsets are UTF-16 code units, as in CodeMirror.

use crate::document::ResponseRegion;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub case: bool,
    pub word: bool,
    pub regex: bool,
}

/// A letter or digit of any script up to the Hangul block, so whole words work
/// beyond ASCII without the RegExp `u` flag that would change user patterns.
const WORD: &str = r"[0-9A-Za-z_\u00C0-\u1FFF\u2C00-\uD7FF]";

/// JavaScript RegExp source and flags for a query; `None` for an empty query.
pub fn pattern(query: &str, options: Options) -> Option<(String, String)> {
    if query.is_empty() {
        return None;
    }
    let body = if options.regex {
        query.to_owned()
    } else {
        escape(query)
    };
    let source = if options.word {
        format!("(?<!{WORD})(?:{body})(?!{WORD})")
    } else {
        body
    };
    let flags = if options.case { "gm" } else { "gim" };
    Some((source, flags.into()))
}

/// Escapes text so a RegExp matches it literally.
pub fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if r"\^$.*+?()[]{}|/".contains(ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub start: usize,
    pub end: usize,
}

/// Line start offsets of a text, mapping offsets to CodeMirror line/ch positions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lines {
    starts: Vec<usize>,
    length: usize,
}

impl Lines {
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0];
        let mut length = 0;
        for byte in text.bytes() {
            // UTF-8 continuation bytes add nothing; a 4-byte sequence is a surrogate pair.
            match byte {
                b'\n' => {
                    length += 1;
                    starts.push(length);
                }
                0x80..=0xBF => {}
                0xF0..=0xFF => length += 2,
                _ => length += 1,
            }
        }
        Self { starts, length }
    }
    pub fn position(&self, offset: usize) -> (usize, usize) {
        let line = self.starts.partition_point(|start| *start <= offset).max(1) - 1;
        (line, offset - self.starts[line])
    }
    pub fn offset(&self, line: usize, ch: usize) -> usize {
        self.starts
            .get(line)
            .map_or(self.length, |start| start + ch)
    }
    /// Offset of the first character of a line, or the text length past the last line.
    pub fn start(&self, line: usize) -> usize {
        self.offset(line, 0)
    }
}

/// The first match at or after `offset`, wrapping to the first one.
pub fn following(matches: &[Match], offset: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let index = matches.partition_point(|m| m.start < offset);
    Some(if index == matches.len() { 0 } else { index })
}

/// The last match that starts before `offset`, wrapping to the last one.
pub fn preceding(matches: &[Match], offset: usize) -> Option<usize> {
    let index = matches.partition_point(|m| m.start < offset);
    match index {
        _ if matches.is_empty() => None,
        0 => Some(matches.len() - 1),
        index => Some(index - 1),
    }
}

/// Whether a match lies outside every response: replacing never edits output.
pub fn editable(lines: &Lines, found: &Match, regions: &[ResponseRegion]) -> bool {
    let (first, _) = lines.position(found.start);
    // A match ending with a line break stops on its own line, not the next one.
    let (last, _) = lines.position(found.end.saturating_sub(1).max(found.start));
    !regions
        .iter()
        .any(|region| first < region.end && last >= region.header)
}

/// Replacement text for one regex match, as JavaScript's `String.replace` reads
/// `$$`, `$&`, `$1`–`$99` and `$<name>`, plus `\n`, `\t` and `\\` escapes.
/// `groups[0]` is the whole match; unmatched groups are `None`.
pub fn expand(
    template: &str,
    groups: &[Option<String>],
    named: Option<&BTreeMap<String, Option<String>>>,
) -> String {
    let chars: Vec<char> = template.chars().collect();
    let group = |index: usize| groups.get(index).and_then(|g| g.as_deref());
    let digit = |index: usize| chars.get(index).and_then(|c| c.to_digit(10));
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < chars.len() {
        match (chars[i], chars.get(i + 1).copied()) {
            ('\\', Some('n')) => out.push('\n'),
            ('\\', Some('t')) => out.push('\t'),
            ('\\', Some('\\')) => out.push('\\'),
            ('$', Some('$')) => out.push('$'),
            ('$', Some('&')) => out.push_str(group(0).unwrap_or_default()),
            ('$', Some('<')) if named.is_some() => {
                let close = chars[i + 2..].iter().position(|c| *c == '>');
                match close {
                    Some(length) => {
                        let name: String = chars[i + 2..i + 2 + length].iter().collect();
                        let value = named.and_then(|named| named.get(&name)).cloned();
                        out.push_str(&value.flatten().unwrap_or_default());
                        i += 3 + length;
                    }
                    None => {
                        out.push_str("$<");
                        i += 2;
                    }
                }
                continue;
            }
            ('$', Some(_)) if digit(i + 1).is_some() => {
                let first = digit(i + 1).unwrap_or_default() as usize;
                let two = digit(i + 2).map(|second| first * 10 + second as usize);
                match two.filter(|n| *n > 0 && *n < groups.len()) {
                    Some(n) => {
                        out.push_str(group(n).unwrap_or_default());
                        i += 3;
                    }
                    None if first > 0 && first < groups.len() => {
                        out.push_str(group(first).unwrap_or_default());
                        i += 2;
                    }
                    None => {
                        out.push('$');
                        i += 1;
                    }
                }
                continue;
            }
            (ch, _) => {
                out.push(ch);
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    out
}

/// The "3 of 120" counter; `capped` means more matches than were counted.
pub fn counter(current: Option<usize>, total: usize, capped: bool) -> String {
    let total = if capped {
        format!("{total}+")
    } else {
        total.to_string()
    };
    match current {
        _ if total == "0" => "No results".into(),
        Some(index) => format!("{} of {total}", index + 1),
        None => format!("{total} found"),
    }
}
