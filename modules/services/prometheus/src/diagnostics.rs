//! PromQL errors of the `#payload` underlined. The parser checks syntax and types (function
//! arity, range vs instant vectors) but reports no position, so the message points at the
//! token it names.
use crate::language::{self, Lexeme, scan};
use services_sdk::{Check, Line, Token, lang};

const PROBLEM: &str = "tpl-problem";

/// The parser's message for the `#payload` query, and its byte range in the query.
pub fn problem(query: &str) -> Option<(String, usize, usize)> {
    let tokens = scan(query);
    // A placeholder outside a string stands for text the parser can't see.
    if tokens.is_empty() || tokens.iter().any(|t| t.kind.is_empty()) {
        return None;
    }
    let message = language::parse(query).err()?;
    let (start, end) = locate(&message, &tokens);
    Some((message, start, end))
}

fn locate(message: &str, tokens: &[Lexeme<'_>]) -> (usize, usize) {
    let span = |t: &Lexeme<'_>| (t.start, t.end);
    let quoted = message
        .split('\'')
        .nth(1)
        .filter(|_| message.contains('\''));
    let named = |name: &str| {
        tokens
            .iter()
            .enumerate()
            .find(|(i, t)| t.text == name && tokens.get(i + 1).is_some_and(|n| n.text == "("))
            .map(|(_, t)| span(t))
    };
    let unmatched = |open: &str, close: &str, last: bool| {
        let mut stack = Vec::new();
        for t in tokens {
            if t.text == open {
                stack.push(t);
            } else if t.text == close && stack.pop().is_none() {
                return (!last).then(|| span(t));
            }
        }
        if last {
            stack.last().map(|t| span(t))
        } else {
            None
        }
    };
    let found = if message.contains("function") || message.contains("call to") {
        quoted.and_then(named)
    } else if message.contains("right parenthesis") {
        unmatched("(", ")", false)
    } else if message.contains("parenthesis") {
        unmatched("(", ")", true)
    } else if message.contains("brace") {
        unmatched("{", "}", true)
    } else if message.contains("bracket") {
        unmatched("[", "]", true)
    } else if message.contains("offset") {
        tokens.iter().rev().find(|t| t.text == "offset").map(span)
    } else {
        None
    };
    found.unwrap_or_else(|| tokens.last().map(span).unwrap_or_default())
}

/// The check's lines with the payload problem underlined over their highlighting.
pub fn marks(check: &Check) -> Vec<Line> {
    let lines: Vec<String> = check.text.split('\n').map(str::to_owned).collect();
    let Some(marker) = lines
        .iter()
        .position(|line| lang::marker(line).is_some_and(|(name, _)| name == "payload"))
    else {
        return Vec::new();
    };
    let end = lines[marker + 1..]
        .iter()
        .position(|line| lang::marker(line).is_some_and(|(name, _)| language::known_marker(&name)))
        .map_or(lines.len(), |n| marker + 1 + n);
    let query = lines[marker + 1..end].join("\n");
    let Some((_, start, stop)) = problem(&query) else {
        return Vec::new();
    };
    let highlighted = language::highlight(&[], &lines, &[]);
    let mut offset = 0;
    let mut out: Vec<Line> = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        let mut tokens = Vec::new();
        if index > marker && index < end {
            let (from, to) = (start.max(offset), stop.min(offset + line.len()));
            if from < to {
                tokens = underline(&highlighted[index].tokens, from - offset, to - offset);
            }
            offset += line.len() + 1;
        }
        out.push(Line {
            tokens,
            state: Vec::new(),
        });
    }
    out
}

/// Tokens covering `from..to` with the problem class added to their own kinds.
fn underline(tokens: &[Token], from: usize, to: usize) -> Vec<Token> {
    let mut out = Vec::new();
    let mut cursor = from;
    for token in tokens {
        let (start, end) = (token.start as usize, (token.start + token.len) as usize);
        let (a, b) = (start.max(cursor), end.min(to));
        if a >= b {
            continue;
        }
        if a > cursor {
            out.push(mark(cursor, a, PROBLEM));
        }
        out.push(mark(a, b, &format!("{} {PROBLEM}", token.kind)));
        cursor = b;
    }
    if cursor < to {
        out.push(mark(cursor, to, PROBLEM));
    }
    out
}

fn mark(start: usize, end: usize, kind: &str) -> Token {
    Token {
        start: start as u32,
        len: (end - start) as u32,
        kind: kind.trim().into(),
    }
}
