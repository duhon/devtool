use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Headers,
    Endpoint,
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
    variant: &str,
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
        Language::Endpoint => {
            if pos == 0
                && variant == "rest"
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
            if variant == "rest"
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
            if variant == "grpc" {
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
            if variant == "rest" && chars[0] == '?' {
                state.endpoint_part = 3;
                return (1, None);
            }
            let n = chars
                .iter()
                .take_while(|c| {
                    **c != '{'
                        && **c != '$'
                        && !(variant == "rest" && **c == '?' && state.endpoint_part == 2)
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
#[cfg(test)]
mod tests {
    use super::*;
    fn tokens(
        lang: Language,
        text: &str,
        state: &mut State,
    ) -> Vec<(String, Option<&'static str>)> {
        tokens_for(lang, "", text, state)
    }
    fn tokens_for(
        lang: Language,
        variant: &str,
        text: &str,
        state: &mut State,
    ) -> Vec<(String, Option<&'static str>)> {
        let chars: Vec<_> = text.chars().collect();
        let mut pos = 0;
        let mut out = vec![];
        while pos < chars.len() {
            let (n, style) = token(lang, variant, text, pos, state);
            assert!(n > 0);
            out.push((
                chars[pos..(pos + n).min(chars.len())].iter().collect(),
                style,
            ));
            pos += n;
        }
        out
    }
    #[test]
    fn typed_tables_color_cells_without_guessing_from_text_or_pipes() {
        let result = serde_json::json!({"columns":["n","flag","at","payload","text"],
            "columnTypes":{"n":"number","flag":"boolean","at":"datetime","payload":"json"},
            "rows":[{"n":"123.000","flag":true,"at":"2026-10-27","payload":"{\"x\":\"a|b\",\"v\":12}","text":"{\"v\":99}"}]});
        let mut state = State {
            table: crate::tables::Metadata::from_result(&result),
            ..Default::default()
        };
        let table = crate::tables::render(&result, false);
        let all: Vec<_> = table
            .lines()
            .flat_map(|line| tokens(Language::Table, line, &mut state))
            .collect();
        assert!(
            all.iter()
                .any(|(s, c)| s.contains("123.000") && *c == Some("number"))
        );
        assert!(
            all.iter()
                .any(|(s, c)| s.contains("true") && *c == Some("atom"))
        );
        assert!(
            all.iter()
                .any(|(s, c)| s.contains("2026-10-27") && *c == Some("string"))
        );
        assert!(all.contains(&("\"a|b\"".into(), Some("string"))));
        assert!(all.contains(&("12".into(), Some("number"))));
        assert!(
            all.iter()
                .any(|(s, c)| s.contains("{\"v\":99}") && c.is_none())
        );
        assert!(
            all.iter()
                .filter(|(_, c)| *c == Some("mysql-table-border"))
                .all(|(s, _)| !s.contains("a|b"))
        );
    }
    #[test]
    fn wrapped_json_and_vertical_json_keep_flat_token_state() {
        for vertical in [false, true] {
            let result = serde_json::json!({"columns":["payload","text"],"columnTypes":{"payload":"json"},
                "columnSizes":{"payload":7},"rows":[{"payload":"{\"s\":\"abc def ghij\",\"v\":true}","text":"plain"}]});
            let mut state = State {
                table: crate::tables::Metadata::from_result(&result),
                ..Default::default()
            };
            let text = crate::tables::render(&result, vertical);
            let mut output = vec![];
            for line in text.lines() {
                output.extend(tokens(Language::Table, line, &mut state));
                state = serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
            }
            assert!(
                !output.iter().any(|(_, c)| *c == Some("error")),
                "{vertical}: {output:?}"
            );
            assert!(
                output
                    .iter()
                    .any(|(s, c)| s.contains("true") && *c == Some("atom")),
                "{vertical}: {output:?}"
            );
            assert!(
                !output
                    .iter()
                    .any(|(s, c)| s.contains("plain") && c.is_some()),
                "{vertical}: {output:?}"
            );
        }
    }
    #[test]
    fn line_lexer_matches_tokens_and_uses_utf16_without_retaining_per_token_state() {
        let result = serde_json::json!({"columns":["j","n"],"columnTypes":{"j":"json","n":"number"},
            "columnSizes":{"j":9},"rows":[{"j":r#"{"s":"😀a\"b","v":[true,false,null]}"#,"n":42}]});
        for vertical in [false, true] {
            let mut reference = State {
                table: crate::tables::Metadata::from_result(&result),
                ..Default::default()
            };
            let mut batched = reference.clone();
            for line in crate::tables::render(&result, vertical).lines() {
                let expected = tokens(Language::Table, line, &mut reference);
                let spans = table_line(line, &mut batched);
                let mut start = 0;
                let actual: Vec<_> = spans
                    .into_iter()
                    .map(|(end, style)| {
                        let text = String::from_utf16(
                            &line.encode_utf16().collect::<Vec<_>>()[start..end],
                        )
                        .unwrap();
                        start = end;
                        (text, style)
                    })
                    .collect();
                assert_eq!(actual, expected);
                assert_eq!(start, line.encode_utf16().count());
            }
        }
    }
    #[test]
    fn json_escapes_atoms_and_numbers_survive_every_narrow_wrap() {
        for width in 1..=16 {
            let result = serde_json::json!({"columns":["j"],"columnTypes":{"j":"json"},"columnSizes":{"j":width},
                "rows":[{"j":r#"{"s":"a\"b\\c | 😀","v":[true,false,null,-1.25e+10]}"#}]});
            let mut state = State {
                table: crate::tables::Metadata::from_result(&result),
                ..Default::default()
            };
            let all: Vec<_> = crate::tables::render(&result, false)
                .lines()
                .flat_map(|line| tokens(Language::Table, line, &mut state))
                .collect();
            assert!(
                !all.iter().any(|(_, c)| *c == Some("error")),
                "width {width}: {all:?}"
            );
        }
    }
    #[test]
    fn rest_path_parameters_are_distinct_from_variables_and_query_values() {
        let source = "PUT http://{{var:magento}}/rest/default/V1/categories/{categoryId}/move/{имя-id}?filter={query}";
        let result = tokens_for(Language::Endpoint, "rest", source, &mut State::default());
        assert_eq!(
            result
                .iter()
                .filter(|(_, style)| *style == Some("rest-path-param"))
                .map(|(text, _)| text.as_str())
                .collect::<Vec<_>>(),
            vec!["{categoryId}", "{имя-id}"]
        );
        assert!(
            result
                .iter()
                .any(|(_, style)| *style == Some("gw-placeholder"))
        );
        for (variant, text) in [
            ("rest", "http://test.com/categories/42/move"),
            ("rest", "http://test.com/{incomplete"),
            ("rest", "http://test.com/{}"),
            ("rest", "http://test.com/{not a name}"),
            ("rest", "http://test.com/#fragment/{categoryId}"),
            ("grpc", "grpc://test.com/{categoryId}"),
            ("graphql", "http://test.com/{categoryId}"),
        ] {
            assert!(
                !tokens_for(Language::Endpoint, variant, text, &mut State::default())
                    .iter()
                    .any(|(_, style)| *style == Some("rest-path-param")),
                "{variant}: {text}"
            );
        }
    }
    #[test]
    fn shell_commands_variables_and_flags() {
        let t = tokens(
            Language::Shell,
            "echo \"$HOME\" --flag && printf {{:NAME}}",
            &mut State::default(),
        );
        assert!(t.contains(&("echo".into(), Some("builtin"))));
        assert!(t.contains(&("$HOME".into(), Some("variable-2"))));
        assert!(t.contains(&("--flag".into(), Some("attribute"))));
        assert!(t.contains(&(":".into(), Some("gw-placeholder-colon"))));
    }
    #[test]
    fn graphql_scopes_and_block_strings() {
        let mut state = State::default();
        let t = tokens(
            Language::Graphql,
            "query($id: ID!) { alias: viewer(id: $id) @skip(if: false) }",
            &mut state,
        );
        assert!(t.contains(&("$id".into(), Some("variable-3"))));
        assert!(t.contains(&("@skip".into(), Some("atom"))));
        assert!(t.contains(&("alias".into(), Some("attribute"))));
        tokens(Language::Graphql, "\"\"\"start", &mut state);
        assert!(state.block_string);
        assert_eq!(
            tokens(Language::Graphql, "end\"\"\"", &mut state)[0].1,
            Some("string")
        );
        assert!(!state.block_string);
    }
    #[test]
    fn graphql_fragment_definitions_spreads_and_type_conditions_have_distinct_styles() {
        let t = tokens(
            Language::Graphql,
            "fragment PriceTest on Price { amount }\nquery { products { ... on SimpleProductView { price { ... PriceTest amount } } } }",
            &mut State::default(),
        );
        assert_eq!(
            t.iter()
                .filter(|(name, style)| name == "PriceTest" && *style == Some("graphql-fragment"))
                .count(),
            2
        );
        for name in ["Price", "SimpleProductView"] {
            assert!(t.contains(&(name.into(), Some("graphql-type-condition"))));
        }
        for name in ["amount", "products", "price"] {
            assert!(t.contains(&(name.into(), Some("property"))));
        }
    }
    #[test]
    fn graphql_fragment_colors_survive_comments_line_breaks_and_state_copying() {
        let mut state = State::default();
        tokens(Language::Graphql, "fragment # definition", &mut state);
        assert!(
            tokens(Language::Graphql, "PriceTest", &mut state)
                .contains(&("PriceTest".into(), Some("graphql-fragment")))
        );
        tokens(Language::Graphql, "on # type", &mut state);
        let mut state: State =
            serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
        assert!(
            tokens(Language::Graphql, "Price { amount }", &mut state)
                .contains(&("Price".into(), Some("graphql-type-condition")))
        );
        tokens(Language::Graphql, "query { ... # reference", &mut state);
        assert!(
            tokens(Language::Graphql, ", PriceTest", &mut state)
                .contains(&("PriceTest".into(), Some("graphql-fragment")))
        );
        tokens(Language::Graphql, "...", &mut state);
        tokens(Language::Graphql, "# ignored", &mut state);
        tokens(Language::Graphql, "on", &mut state);
        assert!(
            tokens(Language::Graphql, "SimpleProductView { sku } }", &mut state)
                .contains(&("SimpleProductView".into(), Some("graphql-type-condition")))
        );
    }
    #[test]
    fn graphql_fragment_colors_do_not_leak_into_values_comments_or_following_fields() {
        let t = tokens(
            Language::Graphql,
            "{ PriceTest: products(skus: [\"... PriceTest\", $PriceTest]) { ...PriceTest sku ... on Product { name } } } # ... on Hidden",
            &mut State::default(),
        );
        assert!(t.contains(&("PriceTest".into(), Some("attribute"))));
        assert!(t.contains(&("\"... PriceTest\"".into(), Some("string"))));
        assert!(t.contains(&("$PriceTest".into(), Some("variable-3"))));
        assert!(t.contains(&("sku".into(), Some("property"))));
        assert!(t.contains(&("name".into(), Some("property"))));
        assert!(t.contains(&("# ... on Hidden".into(), Some("comment"))));
        assert_eq!(
            t.iter()
                .filter(|(_, style)| *style == Some("graphql-fragment"))
                .count(),
            1
        );
        let fields = tokens(
            Language::Graphql,
            "query { fragment PriceTest on ProductViewPrice }",
            &mut State::default(),
        );
        assert!(fields.iter().all(|(_, style)| !matches!(
            style,
            Some("graphql-fragment" | "graphql-type-condition")
        )));
        for source in [
            "{ ... } PriceTest",
            "{ ... \"text\" PriceTest",
            "{ ... $var PriceTest",
        ] {
            assert!(
                tokens(Language::Graphql, source, &mut State::default())
                    .contains(&("PriceTest".into(), Some("property")))
            );
        }
    }
    #[test]
    fn spl_comments_and_command_boundaries() {
        let mut state = State::default();
        let t = tokens(
            Language::Spl,
            "index=main | stats count by host",
            &mut state,
        );
        assert!(t.contains(&("index".into(), Some("attribute"))));
        assert!(t.contains(&("stats".into(), Some("keyword"))));
        tokens(Language::Spl, "```note", &mut state);
        assert!(state.fenced);
        assert_eq!(
            tokens(Language::Spl, "other```", &mut state)[0].1,
            Some("comment")
        );
        assert!(!state.fenced);
    }
    #[test]
    fn mysql_table_does_not_color_values_as_sql() {
        let t = tokens(Language::Table, "| SELECT | true |", &mut State::default());
        assert!(
            t.iter()
                .filter(|(_, s)| s.is_some())
                .all(|(_, s)| *s == Some("mysql-table-border"))
        );
    }
}
