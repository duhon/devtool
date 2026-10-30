use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Headers,
    /// An address: `scheme://host/path`.
    Endpoint,
    /// `METHOD URL` with `{param}` path params and a `?query`.
    Http,
    /// `scheme://target/package.Service/Method`.
    Grpc,
    Shell,
    Spl,
    Graphql,
    Table,
    Plain,
}
impl Language {
    pub fn named(name: &str) -> Option<Self> {
        Some(match name {
            "headers" => Self::Headers,
            "endpoint" => Self::Endpoint,
            "http" => Self::Http,
            "grpc" => Self::Grpc,
            "shell" => Self::Shell,
            "spl" => Self::Spl,
            "graphql" => Self::Graphql,
            "table" => Self::Table,
            "plain" | "proto" => Self::Plain,
            _ => return None,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GraphqlContext {
    FragmentName,
    FragmentOn,
    SpreadName,
    TypeCondition,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub quote: Option<char>,
    pub placeholder: bool,
    pub after_colon: bool,
    pub endpoint_part: u8,
    pub grpc_separators: usize,
    pub after_pipe: bool,
    pub fenced: bool,
    pub block_string: bool,
    pub graphql_context: Option<GraphqlContext>,
    #[serde(default)]
    pub graphql_depth: usize,
    #[serde(default)]
    pub table: crate::tables::Metadata,
    #[serde(default)]
    pub table_cuts: Vec<usize>,
    #[serde(default)]
    pub table_borders: usize,
    #[serde(default)]
    pub table_json: Vec<crate::tables::JsonState>,
    #[serde(default)]
    pub vertical_kind: services_contracts::columns::Kind,
}
fn word(text: &str, extra: &str) -> usize {
    text.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || extra.contains(*c))
        .count()
}
fn quoted(text: &str, quote: char, started: bool) -> (usize, bool) {
    let mut escaped = false;
    let mut n = usize::from(started);
    for c in text.chars().skip(n) {
        n += 1;
        if c == quote && !escaped {
            return (n, true);
        }
        escaped = c == '\\' && !escaped;
    }
    (n, false)
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
pub fn table_line(text: &str, state: &mut State) -> Vec<(usize, Option<&'static str>)> {
    let chars: Vec<_> = text.chars().collect();
    let offsets: Vec<_> = text
        .char_indices()
        .map(|(byte, _)| byte)
        .chain(std::iter::once(text.len()))
        .collect();
    let mut spans = Vec::new();
    let mut pos = 0;
    let mut utf16 = 0;
    while pos < chars.len() {
        let (n, style) = table_token(text, &chars, &offsets, pos, state);
        let end = (pos + n.max(1)).min(chars.len());
        utf16 += chars[pos..end].iter().map(|c| c.len_utf16()).sum::<usize>();
        spans.push((utf16, style));
        pos = end;
    }
    spans
}

fn table_token(
    text: &str,
    chars: &[char],
    offsets: &[usize],
    pos: usize,
    state: &mut State,
) -> (usize, Option<&'static str>) {
    use services_contracts::columns::Kind;
    let rest = &text[offsets[pos]..];
    let remaining = chars.len() - pos;
    if pos == 0
        && text.starts_with('+')
        && text.trim_end().ends_with('+')
        && text.trim().chars().all(|c| c == '+' || c == '-')
    {
        state.table_cuts = chars
            .iter()
            .enumerate()
            .filter_map(|(i, c)| (*c == '+').then_some(i))
            .collect();
        state.table_borders += 1;
        state.table_json =
            vec![crate::tables::JsonState::default(); state.table.columns.len().max(1)];
        return (remaining, Some("mysql-table-border"));
    }
    if text.starts_with('|') && !state.table_cuts.is_empty() {
        if state.table_cuts.contains(&pos) {
            return (1, Some("mysql-table-border"));
        }
        if let Some(column) = state
            .table_cuts
            .windows(2)
            .position(|bounds| pos > bounds[0] && pos < bounds[1])
        {
            let start = state.table_cuts[column] + 2;
            let end = state.table_cuts[column + 1] - 1;
            if pos < start || pos >= end {
                return (1, None);
            }
            let n = (end - pos).min(remaining).max(1);
            if state.table_borders < 2 {
                return (n, None);
            }
            let kind = state
                .table
                .columns
                .get(column)
                .and_then(|name| state.table.types.get(name))
                .copied()
                .unwrap_or_default();
            return match kind {
                Kind::Json => {
                    let cell = &text[offsets[pos]..offsets[pos + n]];
                    if cell.starts_with('N') && cell.trim() == "NULL" {
                        return (n, Some("atom"));
                    }
                    state
                        .table_json
                        .get_mut(column)
                        .map(|json| crate::tables::json_token(cell, json))
                        .unwrap_or((n, None))
                }
                Kind::Number => (n, Some("number")),
                Kind::Boolean => (n, Some("atom")),
                Kind::Datetime => (n, Some("string")),
                Kind::Text => (n, None),
            };
        }
    }
    if pos == 0 && text.starts_with("*****") {
        state.vertical_kind = Kind::Text;
        state.table_json = vec![crate::tables::JsonState::default()];
        return (remaining, Some("mysql-table-border"));
    }
    if chars[..pos].iter().all(|c| c.is_whitespace())
        && let Some((name, _)) = text.split_once(": ")
        && state
            .table
            .columns
            .iter()
            .any(|column| column == name.trim())
    {
        state.vertical_kind = state
            .table
            .types
            .get(name.trim())
            .copied()
            .unwrap_or_default();
        state.table_json = vec![crate::tables::JsonState::default()];
        return (name.chars().count() + 2 - pos, Some("property"));
    }
    if !text.starts_with('|') {
        return match state.vertical_kind {
            Kind::Json => {
                if state.table_json.is_empty() {
                    state.table_json.push(Default::default());
                }
                crate::tables::json_token(rest, &mut state.table_json[0])
            }
            Kind::Number => (remaining, Some("number")),
            Kind::Boolean => (remaining, Some("atom")),
            Kind::Datetime => (remaining, Some("string")),
            Kind::Text => (remaining, None),
        };
    }
    if chars[pos] == '|' {
        return (1, Some("mysql-table-border"));
    }
    (
        chars[pos..]
            .iter()
            .take_while(|c| **c != '|')
            .count()
            .max(1),
        None,
    )
}

pub fn token(
    language: Language,
    text: &str,
    pos: usize,
    state: &mut State,
) -> (usize, Option<&'static str>) {
    if language == Language::Table {
        let chars: Vec<_> = text.chars().collect();
        if pos >= chars.len() {
            return (0, None);
        }
        let offsets: Vec<_> = text
            .char_indices()
            .map(|(byte, _)| byte)
            .chain(std::iter::once(text.len()))
            .collect();
        return table_token(text, &chars, &offsets, pos, state);
    }
    let rest: String = text.chars().skip(pos).collect();
    let chars: Vec<_> = rest.chars().collect();
    if chars.is_empty() {
        return (0, None);
    }
    if pos == 0 {
        state.placeholder = false;
        state.after_colon = false;
        state.endpoint_part = 0;
        state.grpc_separators = 0;
        state.after_pipe = true;
        if language != Language::Shell {
            state.quote = None;
        }
    }
    if language != Language::Table && rest.starts_with("{{") {
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
    if language == Language::Graphql && state.block_string {
        if let Some(end) = rest.find("\"\"\"") {
            state.block_string = false;
            return (rest[..end + 3].chars().count(), Some("string"));
        }
        return (chars.len(), Some("string"));
    }
    if language == Language::Spl && state.fenced {
        if let Some(end) = rest.find("```") {
            state.fenced = false;
            return (rest[..end + 3].chars().count(), Some("comment"));
        }
        return (chars.len(), Some("comment"));
    }
    if language == Language::Headers && pos == 0 && text.trim_start().starts_with('#') {
        return (chars.len(), Some("comment"));
    }
    if language != Language::Table && chars[0].is_whitespace() {
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
        Language::Endpoint | Language::Http | Language::Grpc => {
            let http = language == Language::Http;
            if pos == 0
                && http
                && let Some((method, _)) = rest.split_once(char::is_whitespace)
                && ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"]
                    .contains(&method.to_ascii_uppercase().as_str())
            {
                return (
                    method.chars().count()
                        + rest[method.len()..]
                            .chars()
                            .take_while(|c| c.is_whitespace())
                            .count(),
                    Some("property"),
                );
            }
            if state.endpoint_part == 0 {
                if let Some(end) = rest.find("://") {
                    let scheme = &rest[..end];
                    if !scheme.is_empty()
                        && scheme
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "+.-_".contains(c))
                    {
                        state.endpoint_part = 1;
                        return (scheme.chars().count() + 3, Some("keyword"));
                    }
                }
                state.endpoint_part = 1;
            }
            if state.endpoint_part == 1 {
                state.endpoint_part = 2;
                let n = chars
                    .iter()
                    .take_while(|c| !"/?$".contains(**c) && **c != '{')
                    .count();
                if n > 0 {
                    return (n, Some("def"));
                }
            }
            if http
                && state.endpoint_part == 2
                && chars[0] == '{'
                && !text.chars().take(pos).any(|c| c == '#')
                && let Some(end) = rest.find('}')
                && end > 1
                && rest[1..end]
                    .chars()
                    .all(|c| !c.is_whitespace() && !"/?#{}&=".contains(c))
            {
                return (rest[..=end].chars().count(), Some("rest-path-param"));
            }
            if language == Language::Grpc {
                if chars[0] == '/' {
                    state.grpc_separators += 1;
                    return (1, None);
                }
                let n = chars
                    .iter()
                    .take_while(|c| **c != '/' && **c != '{')
                    .count()
                    .max(1);
                return (
                    n,
                    Some(if state.grpc_separators > 1 {
                        "variable-3"
                    } else {
                        "property"
                    }),
                );
            }
            if http && chars[0] == '?' {
                state.endpoint_part = 3;
                return (1, None);
            }
            let n = chars
                .iter()
                .take_while(|c| {
                    **c != '{' && **c != '$' && !(http && **c == '?' && state.endpoint_part == 2)
                })
                .count()
                .max(1);
            (
                n,
                Some(if state.endpoint_part == 3 {
                    "number"
                } else {
                    "string"
                }),
            )
        }
        Language::Table => unreachable!("Table tokens use the shared line lexer"),
        Language::Plain => (
            rest.find("{{")
                .filter(|i| *i > 0)
                .map(|i| rest[..i].chars().count())
                .unwrap_or(chars.len())
                .max(1),
            None,
        ),
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
        Language::Spl => {
            if let Some(content) = rest.strip_prefix("```") {
                state.after_pipe = false;
                if let Some(end) = content.find("```") {
                    return (rest[..end + 6].chars().count(), Some("comment"));
                }
                state.fenced = true;
                return (chars.len(), Some("comment"));
            }
            if chars[0] == '|' {
                state.after_pipe = true;
                return (1, Some("operator"));
            }
            if chars[0] == '"' || chars[0] == '\'' {
                state.after_pipe = false;
                return (quoted(&rest, chars[0], true).0, Some("string"));
            }
            if "=!<>".contains(chars[0]) {
                state.after_pipe = false;
                return (
                    if chars.get(1) == Some(&'=') { 2 } else { 1 },
                    Some("operator"),
                );
            }
            if chars[0].is_ascii_digit()
                || (chars[0] == '-' && chars.get(1).is_some_and(|c| c.is_ascii_digit()))
            {
                state.after_pipe = false;
                return (
                    chars
                        .iter()
                        .take_while(|c| c.is_ascii_digit() || ".-".contains(**c))
                        .count(),
                    Some("number"),
                );
            }
            let n = word(&rest, ".");
            if n > 0 {
                let value: String = chars[..n].iter().collect();
                let after = state.after_pipe;
                state.after_pipe = false;
                let commands = "search stats eval where table fields sort dedup rex rename lookup join transaction head tail top rare chart timechart bin bucket stream streamstats eventstats fillnull convert foreach makemv mvexpand mvcombine spath xmlkv regex format addinfo appendcols append appendpipe multisearch map tstats inputlookup outputlookup collect delete outputcsv inputcsv iplocation geostats cluster anomalydetection predict trendline gauge highlight loadjob savedsearch sendemail sistats sireduce strcat transpose untable union";
                let keyword = ["and", "or", "not", "xor"]
                    .contains(&value.to_ascii_lowercase().as_str())
                    || (after
                        && commands
                            .split_whitespace()
                            .any(|c| c == value.to_ascii_lowercase()));
                let tail: String = chars[n..].iter().collect();
                return (
                    n,
                    if keyword {
                        Some("keyword")
                    } else if tail.trim_start().starts_with('=')
                        && !tail.trim_start().starts_with("==")
                    {
                        Some("attribute")
                    } else {
                        None
                    },
                );
            }
            state.after_pipe = false;
            (1, None)
        }
        Language::Graphql => {
            if chars[0] == '#' {
                return (chars.len(), Some("comment"));
            }
            if chars[0] == ',' {
                return (1, None);
            }
            if rest.starts_with("...") {
                state.graphql_context = Some(GraphqlContext::SpreadName);
                return (3, None);
            }
            let context = state.graphql_context.take();
            match chars[0] {
                '{' => state.graphql_depth += 1,
                '}' => state.graphql_depth = state.graphql_depth.saturating_sub(1),
                _ => {}
            }
            if let Some(content) = rest.strip_prefix("\"\"\"") {
                if let Some(end) = content.find("\"\"\"") {
                    return (rest[..end + 6].chars().count(), Some("string"));
                }
                state.block_string = true;
                return (chars.len(), Some("string"));
            }
            if chars[0] == '"' {
                return (quoted(&rest, '"', true).0, Some("string"));
            }
            if chars[0] == '$' || chars[0] == '@' {
                return (
                    1 + word(&rest[1..], ""),
                    Some(if chars[0] == '$' {
                        "variable-3"
                    } else {
                        "atom"
                    }),
                );
            }
            if chars[0].is_ascii_digit()
                || (chars[0] == '-' && chars.get(1).is_some_and(|c| c.is_ascii_digit()))
            {
                return (
                    chars
                        .iter()
                        .take_while(|c| c.is_ascii_digit() || ".-+eE".contains(**c))
                        .count(),
                    Some("number"),
                );
            }
            let n = word(&rest, "");
            if n > 0 {
                let value: String = chars[..n].iter().collect();
                let tail: String = chars[n..].iter().collect();
                match context {
                    Some(GraphqlContext::FragmentName) => {
                        state.graphql_context = Some(GraphqlContext::FragmentOn);
                        return (n, Some("graphql-fragment"));
                    }
                    Some(GraphqlContext::SpreadName | GraphqlContext::FragmentOn)
                        if value == "on" =>
                    {
                        state.graphql_context = Some(GraphqlContext::TypeCondition);
                        return (n, Some("keyword"));
                    }
                    Some(GraphqlContext::SpreadName) => {
                        return (n, Some("graphql-fragment"));
                    }
                    Some(GraphqlContext::TypeCondition) => {
                        return (n, Some("graphql-type-condition"));
                    }
                    _ => {}
                }
                if value == "fragment" && state.graphql_depth == 0 {
                    state.graphql_context = Some(GraphqlContext::FragmentName);
                }
                return (
                    n,
                    Some(
                        if [
                            "query",
                            "mutation",
                            "subscription",
                            "fragment",
                            "on",
                            "schema",
                            "scalar",
                            "type",
                            "interface",
                            "union",
                            "enum",
                            "input",
                            "directive",
                            "extend",
                            "implements",
                            "repeatable",
                        ]
                        .contains(&value.as_str())
                        {
                            "keyword"
                        } else if ["true", "false", "null"].contains(&value.as_str()) {
                            "atom"
                        } else if tail.trim_start().starts_with(':') {
                            "attribute"
                        } else {
                            "property"
                        },
                    ),
                );
            }
            (1, None)
        }
    }
}
