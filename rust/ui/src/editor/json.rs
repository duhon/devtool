//! JSON highlighting, lenient enough for JSON Lines and JSON-like text (comments, single
//! quotes, bare words).

type Style = Option<&'static str>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// The open `{`, `[` and `(`, innermost last.
    open: Vec<u8>,
    /// In an object, between `{` or `,` and the next `:`.
    key: bool,
    comment: bool,
    /// The quote of a string the previous line ended inside an escape of.
    string: Option<u8>,
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

/// Whitespace, a number, a word, an operator run or any other character.
fn value(text: &str, pos: usize, key: bool) -> (usize, Style) {
    let bytes = text.as_bytes();
    let c = text[pos..].chars().next().unwrap_or(' ');
    let (end, style) = if c.is_whitespace() {
        return (run(text, pos, char::is_whitespace), None);
    } else if let Some(end) = number(bytes, pos) {
        (end, "number")
    } else if is_word(c) {
        let end = run(text, pos, |c| is_word(c) || c.is_ascii_digit());
        let atom = matches!(&text[pos..end], "true" | "false" | "null" | "NaN" | "Infinity" | "undefined");
        (end, if atom { "atom" } else { "variable" })
    } else if is_operator(bytes[pos]) {
        // The run stops before a comment.
        let length = bytes[pos + 1..]
            .iter()
            .enumerate()
            .take_while(|&(i, &b)| is_operator(b) && !(b == b'/' && matches!(bytes.get(pos + i + 2), Some(b'/' | b'*'))))
            .count();
        return (pos + 1 + length, Some("operator"));
    } else {
        return (pos + c.len_utf8(), None);
    };
    (end, Some(if key { "property" } else { style }))
}

fn run(text: &str, pos: usize, f: impl Fn(char) -> bool) -> usize {
    text[pos..].find(|c| !f(c)).map_or(text.len(), |n| pos + n)
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$' || (!c.is_ascii() && !c.is_whitespace())
}

fn is_operator(b: u8) -> bool {
    b"+-*/=<>!&|?~^%@".contains(&b)
}

/// A block comment's text from byte `from` to its `*/` or the line end.
fn block_comment(bytes: &[u8], from: usize, state: &mut State) -> (usize, Style) {
    let close = bytes[from.min(bytes.len())..].windows(2).position(|pair| pair == b"*/");
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
    (end, Some(if state.key { "string property" } else { "string" }))
}

/// The end of a number at byte `pos`: an optional `-`, digits, fraction and exponent, or hex.
fn number(bytes: &[u8], pos: usize) -> Option<usize> {
    let digits = |from: usize| from + bytes[from..].iter().take_while(|b| b.is_ascii_digit()).count();
    let start = pos + usize::from(bytes[pos] == b'-');
    if bytes.get(start) == Some(&b'0') && matches!(bytes.get(start + 1), Some(b'x' | b'X')) {
        let end = start + 2 + bytes[start + 2..].iter().take_while(|b| b.is_ascii_hexdigit()).count();
        if end > start + 2 {
            return Some(end);
        }
    }
    let mut end = digits(start);
    if bytes.get(end) == Some(&b'.') && (end > start || bytes.get(end + 1).is_some_and(u8::is_ascii_digit)) {
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
}
