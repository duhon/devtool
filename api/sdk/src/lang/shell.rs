//! Shell: a line tokenizer for commands (`sh -c`, `ssh host`, scripts). Quoted strings may
//! continue on the next line.

use super::{placeholder, run, token};
use crate::Token;

const KEYWORDS: [&str; 17] = [
    "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in", "select",
    "then", "time", "until", "while", "coproc",
];
const BUILTINS: [&str; 27] = [
    "alias", "cd", "command", "echo", "eval", "exec", "export", "false", "getopts", "hash", "jobs",
    "kill", "printf", "pwd", "read", "return", "set", "shift", "source", "test", "trap", "true",
    "type", "unalias", "umask", "unset", "wait",
];

/// The quote a string opened with, the next line continues it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub quote: Option<u8>,
}

impl State {
    pub fn encode(self, out: &mut Vec<u8>) {
        out.push(self.quote.unwrap_or(0));
    }
    pub fn decode(bytes: &[u8]) -> Self {
        Self {
            quote: bytes
                .first()
                .copied()
                .filter(|quote| matches!(quote, b'\'' | b'"')),
        }
    }
}

/// The end of a `$name`, `${…}` or `$(…)` at `pos`.
fn variable(line: &str, pos: usize) -> usize {
    let rest = &line[pos + 1..];
    let close = |open: char, close: char| {
        rest.strip_prefix(open)
            .map(|_| rest.find(close).map_or(pos + 1, |end| pos + 1 + end + 1))
    };
    close('(', ')')
        .or_else(|| close('{', '}'))
        .unwrap_or_else(|| {
            run(line, pos + 1, |c| {
                c.is_ascii_alphanumeric() || "_@#?$*!-".contains(c)
            })
        })
}

fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The tokens of one line; `{{…}}` placeholders are left unstyled.
pub fn tokens(line: &str, state: &mut State) -> Vec<Token> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < line.len() {
        if let Some(end) = placeholder(line, pos) {
            pos = end;
            continue;
        }
        let c = line[pos..].chars().next().expect("a char");
        let next = line[pos + c.len_utf8()..].chars().next();
        if let Some(quote) = state.quote {
            let (end, kind) = if quote == b'"' && c == '$' {
                (variable(line, pos), "variable-2")
            } else if c as u32 == quote as u32 {
                state.quote = None;
                (pos + 1, "string")
            } else if c == '\\' {
                (next.map_or(pos + 1, |n| pos + 1 + n.len_utf8()), "string-2")
            } else {
                let end = line[pos..]
                    .find(|ch: char| {
                        ch as u32 == quote as u32
                            || ch == '\\'
                            || (quote == b'"' && ch == '$')
                            || ch == '{'
                    })
                    .map_or(line.len(), |n| pos + n);
                (end.max(pos + c.len_utf8()), "string")
            };
            out.push(token(pos, end, kind));
            pos = end;
            continue;
        }
        let (end, kind) = match c {
            '#' => (line.len(), Some("comment")),
            '"' | '\'' => {
                state.quote = Some(c as u8);
                (pos + 1, Some("string"))
            }
            '\\' => (
                next.map_or(pos + 1, |n| pos + 1 + n.len_utf8()),
                Some("string-2"),
            ),
            '$' => (variable(line, pos), Some("variable-2")),
            '|' | ';' | '&' | '<' | '>' => {
                let double = next == Some(c) && "|&;".contains(c);
                (pos + 1 + usize::from(double), Some("operator"))
            }
            '-' if next.is_some_and(|n| n.is_ascii_alphabetic() || n == '-' || n == '_') => (
                run(line, pos, |ch| word(ch) || ch == '-'),
                Some("attribute"),
            ),
            '0'..='9' => (
                run(line, pos, |ch| ch.is_ascii_digit() || ch == '.'),
                Some("number"),
            ),
            c if word(c) => {
                let end = run(line, pos, |ch| word(ch) || ch == '.' || ch == '-');
                let text = &line[pos..end];
                let kind = if KEYWORDS.contains(&text) {
                    Some("keyword")
                } else if BUILTINS.contains(&text) {
                    Some("builtin")
                } else {
                    None
                };
                (end, kind)
            }
            c => (pos + c.len_utf8(), None),
        };
        if let Some(kind) = kind {
            out.push(token(pos, end, kind));
        }
        pos = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::styled;

    fn pair(text: &str, kind: &str) -> (String, String) {
        (text.into(), kind.into())
    }

    #[test]
    fn commands_variables_flags_and_operators() {
        let line = "echo \"$HOME\" --flag && printf {{:NAME}} # done";
        let styled = styled(line, &tokens(line, &mut State::default()));
        for expected in [
            pair("echo", "builtin"),
            pair("$HOME", "variable-2"),
            pair("--flag", "attribute"),
            pair("&&", "operator"),
            pair("printf", "builtin"),
            pair("# done", "comment"),
        ] {
            assert!(styled.contains(&expected), "{expected:?} in {styled:?}");
        }
        assert!(!styled.iter().any(|(text, _)| text.contains("NAME")));
    }

    #[test]
    fn quotes_continue_on_the_next_line() {
        let mut state = State::default();
        let first = "if true; then echo 'a";
        let styled_first = styled(first, &tokens(first, &mut state));
        assert!(styled_first.contains(&pair("if", "keyword")));
        assert_eq!(state.quote, Some(b'\''));
        let mut bytes = vec![];
        state.encode(&mut bytes);
        let mut state = State::decode(&bytes);
        let second = "b' ${X} 12";
        let styled_second = styled(second, &tokens(second, &mut state));
        assert_eq!(
            styled_second,
            vec![
                pair("b", "string"),
                pair("'", "string"),
                pair("${X}", "variable-2"),
                pair("12", "number"),
            ]
        );
        assert_eq!(state, State::default());
    }
}
