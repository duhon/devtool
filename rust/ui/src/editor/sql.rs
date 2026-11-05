//! MySQL highlighting.
use std::collections::HashMap;
use std::sync::LazyLock;

type Style = Option<&'static str>;

/// A construct the next line continues.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Code,
    Comment,
    /// A string opened with this quote.
    String(u8),
}

const KEYWORDS: &str = include_str!("mysql-keywords.txt");
const TYPES: &str = include_str!("mysql-types.txt");

/// Lower-case word → style; a later list wins over an earlier one.
static WORDS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let keywords = KEYWORDS.split_whitespace().map(|word| (word, "keyword"));
    let types = TYPES.split_whitespace().map(|word| (word, "type"));
    let atoms = ["true", "false", "null", "unknown"].map(|word| (word, "atom"));
    keywords.chain(types).chain(atoms).collect()
});

/// One token from byte `pos`; returns its byte end and style.
pub fn token(text: &str, pos: usize, state: &mut State) -> (usize, Option<&'static str>) {
    let bytes = text.as_bytes();
    match *state {
        State::Comment => return block_comment(bytes, pos, state),
        State::String(quote) => return string(bytes, pos, quote, state),
        State::Code => {}
    }
    let rest = &text[pos..];
    let c = rest.chars().next().unwrap_or(' ');
    let next = bytes.get(pos + 1).copied();
    if c.is_whitespace() {
        return (run(text, pos, char::is_whitespace), None);
    }
    if c == '#' || (c == '-' && comment_start(bytes, pos)) {
        return (text.len(), Some("comment"));
    }
    if comment_start(bytes, pos) {
        return block_comment(bytes, pos + 2, state);
    }
    match c {
        '\'' | '"' => return string(bytes, pos + 1, c as u8, state),
        '`' => return (quoted(bytes, pos), Some("variable-2")),
        '@' => return variable(bytes, pos),
        _ => {}
    }
    if let Some(end) = number(bytes, pos) {
        return (end, Some("number"));
    }
    if c.is_alphabetic() || c == '_' {
        return word(text, pos);
    }
    let style = match c {
        '?' => "variable-3",
        '(' | ')' | '[' | ']' | '{' | '}' => "bracket",
        ':' if next == Some(b'=') => return (pos + 2, Some("operator")),
        ',' | ';' | '.' | ':' => "punctuation",
        _ if is_operator(bytes[pos]) => {
            let length = bytes[pos + 1..]
                .iter()
                .enumerate()
                .take_while(|&(i, &b)| is_operator(b) && !comment_start(bytes, pos + 1 + i))
                .count();
            return (pos + 1 + length, Some("operator"));
        }
        _ => return (pos + c.len_utf8(), None),
    };
    (pos + 1, Some(style))
}

/// The end of the run of characters matching `f` from `pos`.
fn run(text: &str, pos: usize, f: impl Fn(char) -> bool) -> usize {
    text[pos..].find(|c| !f(c)).map_or(text.len(), |n| pos + n)
}

/// Whether `/*`, or `--` followed by a space, a tab or the line end, starts at byte `i`.
fn comment_start(bytes: &[u8], i: usize) -> bool {
    match bytes.get(i..i + 2) {
        Some(b"/*") => true,
        Some(b"--") => matches!(bytes.get(i + 2), None | Some(b' ' | b'\t')),
        _ => false,
    }
}

fn is_operator(b: u8) -> bool {
    b"=<>!+-*/%&|^~".contains(&b)
}

/// A block comment's text from byte `from` to its `*/` or the line end.
fn block_comment(bytes: &[u8], from: usize, state: &mut State) -> (usize, Style) {
    let close = bytes[from.min(bytes.len())..].windows(2).position(|pair| pair == b"*/");
    *state = if close.is_some() { State::Code } else { State::Comment };
    (close.map_or(bytes.len(), |n| from + n + 2), Some("comment"))
}

/// A string's text from byte `i` to its closing `quote` or the line end.
fn string(bytes: &[u8], mut i: usize, quote: u8, state: &mut State) -> (usize, Style) {
    *state = State::String(quote);
    while let Some(&b) = bytes.get(i) {
        if b == b'\\' || (b == quote && bytes.get(i + 1) == Some(&quote)) {
            i += 2;
        } else if b == quote {
            *state = State::Code;
            return (i + 1, Some("string"));
        } else {
            i += 1;
        }
    }
    (bytes.len(), Some("string"))
}

/// The end of the name quoted at byte `i`, where a doubled quote escapes it; the line end
/// when it isn't closed.
fn quoted(bytes: &[u8], mut i: usize) -> usize {
    let quote = bytes[i];
    i += 1;
    while let Some(&b) = bytes.get(i) {
        match (b == quote, bytes.get(i + 1) == Some(&quote)) {
            (true, true) => i += 2,
            (true, false) => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

/// `@name`, `@'name'` and `@@[scope.]name`.
fn variable(bytes: &[u8], pos: usize) -> (usize, Style) {
    let system = bytes.get(pos + 1) == Some(&b'@');
    let start = pos + 1 + usize::from(system);
    if !system && matches!(bytes.get(start), Some(b'\'' | b'"' | b'`')) {
        return (quoted(bytes, start), Some("variable-2"));
    }
    let name = bytes[start..]
        .iter()
        .take_while(|&&b| b.is_ascii_alphanumeric() || b"_$.".contains(&b))
        .count();
    if name == 0 {
        return (pos + 1, None);
    }
    (start + name, Some("variable-2"))
}

/// The end of a number literal at byte `pos`: decimal, `0x`/`0b` or `X'…'`/`B'…'`.
fn number(bytes: &[u8], pos: usize) -> Option<usize> {
    let digits = |from: usize, accept: fn(&u8) -> bool| from + bytes[from..].iter().take_while(|b| accept(b)).count();
    let binary = |b: &u8| matches!(b, b'0' | b'1');
    let first = bytes[pos].to_ascii_lowercase();
    if matches!(first, b'x' | b'b') && bytes.get(pos + 1) == Some(&b'\'') {
        return Some(quoted(bytes, pos + 1));
    }
    if first == b'0' {
        let prefixed = match bytes.get(pos + 1).map(u8::to_ascii_lowercase) {
            Some(b'x') => digits(pos + 2, u8::is_ascii_hexdigit),
            Some(b'b') => digits(pos + 2, binary),
            _ => 0,
        };
        if prefixed > pos + 2 {
            return Some(prefixed);
        }
    }
    let integer = digits(pos, u8::is_ascii_digit);
    let mut end = integer;
    if bytes.get(end) == Some(&b'.') {
        let fraction = digits(end + 1, u8::is_ascii_digit);
        if integer > pos || fraction > end + 1 {
            end = fraction;
        }
    }
    if end == pos {
        return None;
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let sign = end + 1 + usize::from(matches!(bytes.get(end + 1), Some(b'+' | b'-')));
        let exponent = digits(sign, u8::is_ascii_digit);
        if exponent > sign {
            end = exponent;
        }
    }
    Some(end)
}

/// A word: a charset introducer before a quote, an atom, a type, a keyword or a name.
fn word(text: &str, pos: usize) -> (usize, Style) {
    let end = run(text, pos, |c| {
        c.is_ascii_alphanumeric() || c == '_' || c == '$' || (!c.is_ascii() && c.is_alphabetic())
    });
    let word = &text[pos..end];
    let introducer = (word.starts_with('_') || word.eq_ignore_ascii_case("n"))
        && matches!(text.as_bytes().get(end), Some(b'\'' | b'"'));
    if introducer {
        return (end, Some("keyword"));
    }
    let mut lower = [0; 32];
    let style = lower.get_mut(..word.len()).and_then(|lower| {
        lower.copy_from_slice(word.as_bytes());
        lower.make_ascii_lowercase();
        WORDS.get(std::str::from_utf8(lower).ok()?).copied()
    });
    (end, style)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn block_comments_and_strings_span_lines() {
        assert_eq!(
            styled("a /* one /* two\nthree */ b\n'it''s \\'\nstill' c"),
            [
                ("/* one /* two", "comment"),
                ("three */", "comment"),
                ("'it''s \\'", "string"),
                ("still'", "string"),
            ]
        );
        assert_eq!(styled("/*! x */ `a``b` `open"), [("/*! x */", "comment"), ("`a``b`", "variable-2"), ("`open", "variable-2")]);
    }

    #[test]
    fn words_are_atoms_types_keywords_or_names() {
        assert_eq!(
            styled("Select NULL, Unknown, CAST(x AS UNSIGNED) FROM tbl_é WHERE date = DATE '2024-01-01'"),
            [
                ("Select", "keyword"),
                ("NULL", "atom"),
                (",", "punctuation"),
                ("Unknown", "atom"),
                (",", "punctuation"),
                ("(", "bracket"),
                ("AS", "keyword"),
                ("UNSIGNED", "type"),
                (")", "bracket"),
                ("FROM", "keyword"),
                ("WHERE", "keyword"),
                ("date", "type"),
                ("=", "operator"),
                ("DATE", "type"),
                ("'2024-01-01'", "string"),
            ]
        );
        assert_eq!(
            styled("_utf8mb4'x' N'y' ?;"),
            [
                ("_utf8mb4", "keyword"),
                ("'x'", "string"),
                ("N", "keyword"),
                ("'y'", "string"),
                ("?", "variable-3"),
                (";", "punctuation"),
            ]
        );
    }

    #[test]
    fn variables() {
        assert_eq!(
            styled("@a.b_1 := @'q r' + @@global.max_x + @`b` @"),
            [
                ("@a.b_1", "variable-2"),
                (":=", "operator"),
                ("@'q r'", "variable-2"),
                ("+", "operator"),
                ("@@global.max_x", "variable-2"),
                ("+", "operator"),
                ("@`b`", "variable-2"),
            ]
        );
    }

    #[test]
    fn numbers() {
        let numbers = "42 1.5 .5 1. 1e10 1.5E-3 2e+4 0x1F X'1F' x'1f' 0b101 B'101' b'1'";
        let expected: Vec<_> = numbers.split(' ').map(|n| (n, "number")).collect();
        assert_eq!(styled(numbers), expected);
        assert_eq!(styled("-1e"), [("-", "operator"), ("1", "number")]);
        assert_eq!(styled("0xg"), [("0", "number")]);
    }

    #[test]
    fn dashes_start_a_comment_only_before_a_space_or_the_line_end() {
        assert_eq!(styled("a--b"), [("--", "operator")]);
        assert_eq!(styled("a--\tb"), [("--\tb", "comment")]);
        assert_eq!(styled("1=--\n2 # x"), [("1", "number"), ("=", "operator"), ("--", "comment"), ("2", "number"), ("# x", "comment")]);
        assert_eq!(styled("a<>/* c */"), [("<>", "operator"), ("/* c */", "comment")]);
    }
}
