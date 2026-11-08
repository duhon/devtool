//! Highlighting of the App's own single-language editors: `headers` (hints, variables) and
//! `shell` (exported commands). Service checks are highlighted by their modules.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Headers,
    Shell,
}
impl Language {
    pub fn named(name: &str) -> Option<Self> {
        Some(match name {
            "headers" => Self::Headers,
            "shell" => Self::Shell,
            _ => return None,
        })
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub quote: Option<char>,
    pub placeholder: bool,
    pub after_colon: bool,
}
fn word(text: &str, extra: &str) -> usize {
    text.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || extra.contains(*c))
        .count()
}
fn shell_variable(text: &str) -> usize {
    let chars: Vec<_> = text.chars().collect();
    if chars.first() != Some(&'$') {
        return 0;
    }
    if chars.get(1) == Some(&'(') {
        return chars
            .iter()
            .position(|c| *c == ')')
            .map(|i| i + 1)
            .unwrap_or(1);
    }
    if chars.get(1) == Some(&'{') {
        return chars
            .iter()
            .position(|c| *c == '}')
            .map(|i| i + 1)
            .unwrap_or(1);
    }
    1 + chars
        .iter()
        .skip(1)
        .take_while(|c| c.is_ascii_alphanumeric() || "_@#?$*!-".contains(**c))
        .count()
}
pub fn token(
    language: Language,
    text: &str,
    pos: usize,
    state: &mut State,
) -> (usize, Option<&'static str>) {
    let rest: String = text.chars().skip(pos).collect();
    let chars: Vec<_> = rest.chars().collect();
    if chars.is_empty() {
        return (0, None);
    }
    if pos == 0 {
        state.placeholder = false;
        state.after_colon = false;
        if language != Language::Shell {
            state.quote = None;
        }
    }
    if rest.starts_with("{{") {
        state.placeholder = true;
        return (2, Some("gw-placeholder"));
    }
    if state.placeholder {
        if rest.starts_with("}}") {
            state.placeholder = false;
            return (2, Some("gw-placeholder"));
        }
        if chars[0] == ':' {
            return (1, Some("gw-placeholder-colon"));
        }
        let n = word(&rest, "-");
        if n > 0 {
            return (n, Some("gw-placeholder"));
        }
        state.placeholder = false;
    }
    if language == Language::Headers && pos == 0 && text.trim_start().starts_with('#') {
        return (chars.len(), Some("comment"));
    }
    if chars[0].is_whitespace() {
        return (chars.iter().take_while(|c| c.is_whitespace()).count(), None);
    }
    match language {
        Language::Headers => {
            if pos == 0 && text.trim_start().starts_with('#') {
                return (chars.len(), Some("comment"));
            }
            if chars[0] == ':' && !state.after_colon {
                state.after_colon = true;
                return (1, None);
            }
            let stop = rest.find("{{").filter(|i| *i > 0).unwrap_or(rest.len());
            if state.after_colon {
                return (rest[..stop].chars().count().max(1), Some("string"));
            }
            let end = rest.find(':').unwrap_or(stop).min(stop);
            (rest[..end].chars().count().max(1), Some("attribute"))
        }
        Language::Shell => {
            if let Some(quote) = state.quote {
                if quote == '"' && chars[0] == '$' {
                    return (shell_variable(&rest).max(1), Some("variable-2"));
                }
                if chars[0] == quote {
                    state.quote = None;
                    return (1, Some("string"));
                }
                if chars[0] == '\\' {
                    return (chars.len().min(2), Some("string-2"));
                }
                let n = chars
                    .iter()
                    .take_while(|c| {
                        **c != quote && **c != '\\' && !(quote == '"' && **c == '$') && **c != '{'
                    })
                    .count()
                    .max(1);
                return (n, Some("string"));
            }
            if chars[0] == '#' {
                return (chars.len(), Some("comment"));
            }
            if chars[0] == '"' || chars[0] == '\'' {
                state.quote = Some(chars[0]);
                return (1, Some("string"));
            }
            if chars[0] == '\\' {
                return (chars.len().min(2), Some("string-2"));
            }
            if chars[0] == '$' {
                return (shell_variable(&rest).max(1), Some("variable-2"));
            }
            if "|;&<>".contains(chars[0]) {
                return (
                    if chars.len() > 1 && chars[0] == chars[1] {
                        "|&;".contains(chars[0]) as usize + 1
                    } else {
                        1
                    },
                    Some("operator"),
                );
            }
            if chars[0] == '-'
                && chars
                    .get(1)
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == '-' || *c == '_')
            {
                return (word(&rest, "-"), Some("attribute"));
            }
            if chars[0].is_ascii_digit() {
                return (
                    chars
                        .iter()
                        .take_while(|c| c.is_ascii_digit() || **c == '.')
                        .count(),
                    Some("number"),
                );
            }
            let n = word(&rest, ".-");
            if n > 0 {
                let word: String = chars[..n].iter().collect();
                return (
                    n,
                    if [
                        "case", "do", "done", "elif", "else", "esac", "fi", "for", "function",
                        "if", "in", "select", "then", "time", "until", "while", "coproc",
                    ]
                    .contains(&word.as_str())
                    {
                        Some("keyword")
                    } else if [
                        "alias", "cd", "command", "echo", "eval", "exec", "export", "false",
                        "getopts", "hash", "jobs", "kill", "printf", "pwd", "read", "return",
                        "set", "shift", "source", "test", "trap", "true", "type", "unalias",
                        "umask", "unset", "wait",
                    ]
                    .contains(&word.as_str())
                    {
                        Some("builtin")
                    } else {
                        None
                    },
                );
            }
            (1, None)
        }
    }
}
