//! Bracket matching at the cursor.
use super::highlight::{Highlighter, Style};
use super::state::Selection;
use super::text::utf16_len;
use crate::editor::text::Position;

/// How many lines away from the cursor the partner may be.
const MAX_LINES: usize = 1000;
/// A line longer than this (UTF-16 units) ends the search.
const MAX_LINE: usize = 10_000;

/// The bracket next to the cursor and its partner, each with whether they match.
pub fn matching(
    highlighter: &mut Highlighter,
    lines: &[String],
    selection: Selection,
) -> Vec<(Position, bool)> {
    let (from, to) = (selection.from(), selection.to());
    let Some(text) = lines.get(from.line) else {
        return Vec::new();
    };
    if to.line != from.line || to.ch > from.ch + 1 {
        return Vec::new();
    }
    let bracket = |ch: usize| {
        let unit = text.encode_utf16().nth(ch)?;
        char::from_u32(unit.into()).filter(|c| "()[]{}".contains(*c))
    };
    let ch = if from == to && from.ch > 0 && bracket(from.ch - 1).is_some() {
        from.ch - 1
    } else {
        from.ch
    };
    let Some(open) = bracket(ch) else {
        return Vec::new();
    };
    let style = highlighter.style_at(lines, from.line, ch);
    if style.is_some_and(|style| !style.split_whitespace().any(|word| word == "bracket")) {
        return Vec::new();
    }
    let candidate = Position {
        line: from.line,
        ch,
    };
    match partner(highlighter, lines, candidate, open, style) {
        Some((at, close)) if close == pair(open) => vec![(candidate, true), (at, true)],
        _ => vec![(candidate, false)],
    }
}

/// The first unbalanced bracket of `style` after an opener or before a closer at `start`.
fn partner(
    highlighter: &mut Highlighter,
    lines: &[String],
    start: Position,
    bracket: char,
    style: Style,
) -> Option<(Position, char)> {
    let forward = "([{".contains(bracket);
    let range: Box<dyn Iterator<Item = usize>> = if forward {
        Box::new(start.line..lines.len().min(start.line + MAX_LINES + 1))
    } else {
        Box::new((start.line.saturating_sub(MAX_LINES)..=start.line).rev())
    };
    let mut depth = 0;
    for line in range {
        if utf16_len(&lines[line]) > MAX_LINE {
            return None;
        }
        let mut found = brackets(highlighter, lines, line);
        if !forward {
            found.reverse();
        }
        for (ch, c, s) in found {
            let ahead = if forward {
                ch > start.ch
            } else {
                ch < start.ch
            };
            if s != style || (line == start.line && !ahead) {
                continue;
            }
            if "([{".contains(c) == forward {
                depth += 1;
            } else if depth == 0 {
                return Some((Position { line, ch }, c));
            } else {
                depth -= 1;
            }
        }
    }
    None
}

/// The brackets of `line` with their columns and styles; a line without any isn't tokenized.
fn brackets(
    highlighter: &mut Highlighter,
    lines: &[String],
    line: usize,
) -> Vec<(usize, char, Style)> {
    let text = &lines[line];
    if !text.contains(['(', ')', '[', ']', '{', '}']) {
        return Vec::new();
    }
    let spans = highlighter.spans(lines, line);
    let (mut ch, mut span) = (0, 0);
    let mut out = Vec::new();
    for c in text.chars() {
        while spans.get(span).is_some_and(|s| s.end <= ch) {
            span += 1;
        }
        if "()[]{}".contains(c) {
            out.push((ch, c, spans.get(span).and_then(|s| s.style)));
        }
        ch += c.len_utf16();
    }
    out
}

fn pair(bracket: char) -> char {
    match bracket {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        ')' => '(',
        ']' => '[',
        _ => '{',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: usize, ch: usize) -> Position {
        Position { line, ch }
    }

    #[test]
    fn matches_unstyled_brackets_across_lines() {
        let mut highlighter = Highlighter::new("json");
        let lines: Vec<String> = ["{\"a\": \"}\",", "  \"b\": [1]}", "(]"]
            .map(String::from)
            .into();
        let mut check = |cursor| matching(&mut highlighter, &lines, Selection::cursor(cursor));
        assert_eq!(check(at(0, 0)), [(at(0, 0), true), (at(1, 10), true)]);
        assert_eq!(check(at(1, 11)), [(at(1, 10), true), (at(0, 0), true)]);
        assert_eq!(check(at(2, 1)), [(at(2, 0), false)]);
        assert_eq!(check(at(0, 7)), []);
    }
}
