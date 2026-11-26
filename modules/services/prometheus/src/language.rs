//! Error-tolerant editor support; parser diagnostics are advisory, never an execution gate.
use crate::discovery::Discovery;
use crate::sequence;
use promql_parser::parser::{self, Expr};
use services_sdk::lang::{self, check::At, endpoint, key_value, table};
use services_sdk::{Check, Completions, Fold, Line, LineColumns, Token};

const SECTIONS: &[&str] = &["target", "metadata", "payload", "options"];
pub(crate) const FUNCTIONS: &str = "abs absent absent_over_time acos acosh asin asinh atan atanh ceil changes clamp clamp_max clamp_min cos cosh count_over_time day_of_month day_of_week day_of_year days_in_month delta deriv double_exponential_smoothing exp floor histogram_avg histogram_count histogram_fraction histogram_quantile histogram_stddev histogram_stdvar histogram_sum hour idelta increase info irate label_join label_replace last_over_time ln log2 log10 mad_over_time max_over_time min_over_time minute month pi predict_linear present_over_time quantile_over_time rate resets round scalar sgn sin sinh sort sort_desc sort_by_label sort_by_label_desc sqrt stddev_over_time stdvar_over_time sum_over_time tan tanh time timestamp vector year avg_over_time ts_of_last_over_time ts_of_max_over_time ts_of_min_over_time";
const KEYWORDS: &str = "sum avg count min max group stddev stdvar topk bottomk count_values quantile limitk limit_ratio and or unless atan2 by without on ignoring group_left group_right bool offset start end smoothed anchored";

pub(crate) fn known_marker(name: &str) -> bool {
    matches!(
        name,
        "check"
            | "target"
            | "metadata"
            | "headers"
            | "options"
            | "payload"
            | "response"
            | "responce"
    )
}

fn at(text: &str, pos: usize) -> At<'_> {
    let mut at = At::new(text, pos);
    let strings: Vec<_> = scan(text)
        .into_iter()
        .filter(|t| t.kind == "string")
        .collect();
    let lines = &at.lines;
    at.markers.retain(|(line, name)| {
        known_marker(name)
            && !strings
                .iter()
                .any(|token| token.start < lines[*line].0 && lines[*line].0 < token.end)
    });
    at.section = at
        .markers
        .iter()
        .rev()
        .find(|(line, _)| *line < at.line)
        .cloned();
    at.section_end = at
        .markers
        .iter()
        .find(|(line, _)| *line > at.line)
        .map_or(at.lines.len(), |(line, _)| *line);
    at
}

pub fn parse(text: &str) -> Result<Expr, String> {
    parser::parse(text)
}

pub fn prettify(text: &str) -> Result<String, String> {
    parse(text).map(|expr| expr.prettify())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Context {
    Metrics,
    Labels {
        selector: Option<String>,
    },
    Values {
        label: String,
        selector: Option<String>,
    },
    Duration,
    None,
}

#[derive(Clone, Debug)]
pub(crate) struct Lexeme<'a> {
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
    pub kind: &'static str,
    pub closed: bool,
}

pub(crate) fn scan(text: &str) -> Vec<Lexeme<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let start = i;
        let c = text[i..].chars().next().unwrap();
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        if text[i..].starts_with("{{") {
            i = text[i + 2..]
                .find("}}")
                .map_or(text.len(), |end| i + end + 4);
            out.push(Lexeme {
                text: &text[start..i],
                start,
                end: i,
                kind: "",
                closed: true,
            });
            continue;
        }
        let kind;
        let mut closed = true;
        if c == '#' {
            i = text[i..].find('\n').map_or(text.len(), |n| i + n);
            kind = "comment";
        } else if matches!(c, '"' | '\'' | '`') {
            closed = false;
            i += c.len_utf8();
            while i < text.len() {
                let next = text[i..].chars().next().unwrap();
                i += next.len_utf8();
                if next == '\\' && c != '`' && i < text.len() {
                    i += text[i..].chars().next().unwrap().len_utf8();
                } else if next == c {
                    closed = true;
                    break;
                }
            }
            kind = "string";
        } else if c.is_alphabetic() || matches!(c, '_' | ':') {
            i += c.len_utf8();
            while i < text.len() {
                let next = text[i..].chars().next().unwrap();
                if !(next.is_alphanumeric() || matches!(next, '_' | ':')) {
                    break;
                }
                i += next.len_utf8();
            }
            let word = &text[start..i];
            kind = if FUNCTIONS.split_whitespace().any(|f| f == word) {
                "def"
            } else if KEYWORDS.split_whitespace().any(|f| f == word) {
                "keyword"
            } else {
                "variable"
            };
        } else if c.is_ascii_digit() {
            i += 1;
            while i < text.len() && text.as_bytes()[i].is_ascii_alphanumeric() {
                i += 1;
            }
            kind = "number";
        } else {
            i += c.len_utf8();
            if matches!(c, '=' | '!' | '<' | '>') && text[i..].starts_with(['=', '~']) {
                i += 1;
            }
            kind = "operator";
        }
        out.push(Lexeme {
            text: &text[start..i],
            start,
            end: i,
            kind,
            closed,
        });
    }
    out
}

fn selector(metric: Option<&str>, matchers: &[Lexeme<'_>]) -> Option<String> {
    let mut parts = Vec::new();
    for matcher in matchers.split(|t| t.text == ",") {
        if matcher.len() == 3
            && matches!(matcher[0].kind, "variable" | "keyword" | "def")
            && matches!(matcher[1].text, "=" | "!=" | "=~" | "!~")
            && matcher[2].kind == "string"
            && matcher[2].closed
        {
            parts.push(matcher.iter().map(|t| t.text).collect::<String>());
        }
    }
    let metric = metric.unwrap_or("");
    if metric.is_empty() && parts.is_empty() {
        return None;
    }
    let result = format!("{metric}{{{}}}", parts.join(","));
    // The AST validates a scoped selector without requiring the unfinished expression to parse.
    matches!(parse(&result), Ok(Expr::VectorSelector(_))).then_some(result)
}

pub fn context(check: &Check, pos: usize) -> Context {
    let at = at(&check.text, pos);
    if at.name() != Some("payload") || at.in_placeholder() || at.in_response() {
        return Context::None;
    }
    let tokens = scan(at.body());
    if tokens
        .last()
        .is_some_and(|t| t.kind == "comment" && !at.body()[t.end..].contains('\n'))
    {
        return Context::None;
    }
    let mut opens = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if token.text == "{" {
            opens.push(i);
        }
        if token.text == "}" {
            opens.pop();
        }
    }
    if let Some(&open) = opens.last() {
        let metric = open
            .checked_sub(1)
            .and_then(|i| tokens.get(i))
            .filter(|t| matches!(t.kind, "variable" | "keyword" | "def"))
            .map(|t| t.text);
        let inside = &tokens[open + 1..];
        let part = inside.rsplit(|t| t.text == ",").next().unwrap_or(&[]);
        let scope = selector(metric, inside);
        if part.len() >= 2 && matches!(part[1].text, "=" | "!=" | "=~" | "!~") {
            if part.len() == 2 || part.last().is_some_and(|t| t.kind == "string" && !t.closed) {
                return Context::Values {
                    label: part[0].text.to_owned(),
                    selector: selector(metric, &inside[..inside.len() - part.len()]),
                };
            }
            if part.last().is_some_and(|t| t.kind == "string") {
                return Context::None;
            }
        }
        return Context::Labels { selector: scope };
    }
    if let Some(open) = tokens.iter().rposition(|t| t.text == "[")
        && !tokens[open..].iter().any(|t| t.text == "]")
    {
        return Context::Duration;
    }
    if let Some(open) = tokens.iter().rposition(|t| t.text == "(")
        && !tokens[open..].iter().any(|t| t.text == ")")
        && open > 0
        && matches!(
            tokens[open - 1].text,
            "by" | "without" | "on" | "ignoring" | "group_left" | "group_right"
        )
    {
        return Context::Labels { selector: None };
    }
    if let Some(offset) = tokens.iter().rposition(|t| t.text == "offset")
        && tokens[offset + 1..].iter().all(|t| t.kind == "number")
    {
        return Context::Duration;
    }
    if tokens.last().is_some_and(|t| t.kind == "string") {
        return Context::None;
    }
    Context::Metrics
}

pub fn complete(check: &Check, pos: usize, discovery: &Discovery) -> Completions {
    let at = at(&check.text, pos);
    if at.in_placeholder() || at.in_response() {
        return at.none();
    }
    if let Some(markers) = at.markers(SECTIONS) {
        return markers;
    }
    // A blank line can start the next section, except inside a multi-line PromQL query.
    if at.name() != Some("payload")
        && let Some(next) = at.next_section(SECTIONS)
    {
        return next;
    }
    let mut from = at.line_start(at.line) + lang::token_start(at.before);
    let mut query = &at.text[from..at.pos];
    let mut items = Vec::new();
    match at.name() {
        Some("metadata" | "options") => {
            let options = at.name() == Some("options");
            if let Some((name, _)) = at.before.split_once(':') {
                let choices: &[&str] = match name.trim().to_ascii_lowercase().as_str() {
                    "format" if options => &["table", "json", "chart"],
                    "query-mode" if !options => &["instant", "range"],
                    _ => &[],
                };
                items.extend(choices.iter().map(|s| lang::item(*s, "Value", "keyword")));
            } else {
                let used = key_value::used(at.other_lines());
                let typed = at.before.trim_start();
                from = at.line_start(at.line) + at.before.len() - typed.len();
                query = typed;
                items = if options {
                    // The console groups the values of one option into `Format: ` that asks
                    // for the value next.
                    key_value::items(
                        &["Format: table", "Format: json", "Format: chart"],
                        &[
                            ("Columns-Type", "Column types"),
                            ("Columns-Size", "Column widths"),
                            ("Chart-Width", "Chart width"),
                            ("Chart-Height", "Chart height"),
                            ("Chart-Min", "Chart y-axis minimum"),
                            ("Chart-Max", "Chart y-axis maximum"),
                        ],
                        &used,
                    )
                } else {
                    // The same for metadata, which the console leaves to the module.
                    key_value::items(
                        &[],
                        &[
                            ("Query-Mode", "Metadata"),
                            ("Accept", "Header"),
                            ("Authorization", "Header"),
                            ("Query-Time", "Evaluation time"),
                            ("Query-Start", "Range start"),
                            ("Query-End", "Range end"),
                            ("Query-Step", "Range step"),
                            ("Query-Timeout", "Query timeout"),
                        ],
                        &used,
                    )
                    .into_iter()
                    .map(|item| {
                        if item.insert == "Query-Mode: " {
                            services_sdk::Item {
                                label: "Query-Mode: instant | range".into(),
                                next: true,
                                ..item
                            }
                        } else {
                            item
                        }
                    })
                    .collect()
                };
            }
        }
        Some("payload") => {
            let context = context(check, at.pos);
            match &context {
                Context::None => return at.none(),
                Context::Duration => {
                    items.extend(
                        ["1m", "5m", "15m", "1h", "6h", "1d", "1w", "30s"]
                            .map(|s| lang::item(s, "Duration", "number")),
                    );
                }
                Context::Metrics => {
                    from = at.line_start(at.line)
                        + at.before
                            .char_indices()
                            .rev()
                            .find(|(_, c)| !c.is_alphanumeric() && !matches!(c, '_' | ':'))
                            .map_or(0, |(i, c)| i + c.len_utf8());
                    query = &at.text[from..at.pos];
                    let body_start = at.line_start(at.body_start());
                    let tokens: Vec<_> = scan(at.body())
                        .into_iter()
                        .filter(|t| body_start + t.end <= from && t.kind != "comment")
                        .collect();
                    let metrics: Vec<_> = discovery
                        .schema_of(check)
                        .map(|schema| {
                            schema["metrics"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|v| v.as_str())
                                .map(|name| {
                                    (name.to_owned(), sequence::kind(discovery, check, name))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    items.extend(sequence::items(&sequence::slot(&tokens), &metrics, query));
                }
                Context::Labels { .. } | Context::Values { .. } => {
                    if let Context::Values { .. } = &context
                        && let Some(token) = scan(at.body()).last().filter(|t| t.kind == "string")
                    {
                        from = at.line_start(at.body_start()) + token.start + 1;
                        query = &at.text[from..at.pos];
                    }
                    let values = discovery.candidates(check, &context);
                    items.extend(
                        values
                            .into_iter()
                            .filter(|value| {
                                from.checked_sub(1).map(|i| at.text.as_bytes()[i]) != Some(b'`')
                                    || !value.contains('`')
                            })
                            .map(|value| {
                                let insert = if matches!(context, Context::Values { .. }) {
                                    let escaped = serde_json::to_string(&value).unwrap();
                                    match from.checked_sub(1).map(|i| at.text.as_bytes()[i]) {
                                        Some(b'"') => escaped[1..escaped.len() - 1].to_owned(),
                                        Some(b'\'') => escaped[1..escaped.len() - 1]
                                            .replace("\\\"", "\"")
                                            .replace('\'', "\\'"),
                                        Some(b'`') => value,
                                        _ => escaped,
                                    }
                                } else {
                                    value
                                };
                                lang::item(insert, "Prometheus label", "property")
                            }),
                    );
                }
            }
        }
        _ => {}
    }
    lang::finish(from, at.pos, query, items)
}

pub fn highlight(state: &[u8], lines: &[String], columns: &[LineColumns]) -> Vec<Line> {
    let split = state.iter().position(|b| *b == 0).unwrap_or(state.len());
    let mut section = String::from_utf8_lossy(&state[..split]).into_owned();
    let mut raw = section.ends_with(":raw");
    if raw {
        section.truncate(section.len() - 4);
    }
    let mut rendered = table::State::decode(state.get(split + 1..).unwrap_or_default());
    let mut out = Vec::new();
    for (index, text) in lines.iter().enumerate() {
        let tokens = if let Some((name, _)) =
            lang::marker(text).filter(|(name, _)| !raw && known_marker(name))
        {
            section = name;
            if let Some(cols) = columns.iter().find(|c| c.line as usize == index) {
                rendered = table::State::default();
                rendered.report(&cols.columns);
            }
            vec![Token {
                start: 0,
                len: text.len() as u32,
                kind: "comment".into(),
            }]
        } else {
            match section.as_str() {
                "target" => endpoint::tokens(text, true),
                "metadata" | "headers" | "options" => key_value::tokens(text),
                "response" | "responce" => table::tokens(text, &mut rendered),
                _ => prom_tokens(text, &mut raw),
            }
        };
        let mut state = section.as_bytes().to_vec();
        if raw {
            state.extend_from_slice(b":raw");
        }
        state.push(0);
        rendered.encode(&mut state);
        // The App overlays placeholders, including those nested inside quoted strings.
        let mut visible = Vec::new();
        for token in tokens {
            let start = token.start as usize;
            let end = start + token.len as usize;
            let mut cursor = start;
            while cursor < end {
                let Some(open) = text[cursor..end].find("{{").map(|i| cursor + i) else {
                    visible.push(Token {
                        start: cursor as u32,
                        len: (end - cursor) as u32,
                        kind: token.kind.clone(),
                    });
                    break;
                };
                if open > cursor {
                    visible.push(Token {
                        start: cursor as u32,
                        len: (open - cursor) as u32,
                        kind: token.kind.clone(),
                    });
                }
                cursor = text[open + 2..end].find("}}").map_or(end, |i| open + i + 4);
            }
        }
        out.push(Line {
            tokens: visible,
            state,
        });
    }
    out
}

fn prom_tokens(text: &str, raw: &mut bool) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut offset = 0;
    if *raw {
        let end = text.find('`').map_or(text.len(), |i| i + 1);
        *raw = !text[..end].ends_with('`');
        if end > 0 {
            tokens.push(Token {
                start: 0,
                len: end as u32,
                kind: "string".into(),
            });
        }
        if *raw {
            return tokens;
        }
        offset = end;
    }
    for token in scan(&text[offset..]) {
        if token.kind == "string" && token.text.starts_with('`') && !token.closed {
            *raw = true;
        }
        if !token.kind.is_empty() {
            tokens.push(Token {
                start: (offset + token.start) as u32,
                len: (token.end - token.start) as u32,
                kind: token.kind.into(),
            });
        }
    }
    tokens
}

pub fn fold(check: &Check) -> Vec<Fold> {
    let mut folds = table::folds(&check.text, &check.columns);
    let mut stack = Vec::new();
    for token in scan(&check.text) {
        let line = check.text[..token.start]
            .bytes()
            .filter(|b| *b == b'\n')
            .count() as u32;
        if matches!(token.text, "(" | "{" | "[") {
            stack.push((token.text, line));
        }
        if matches!(token.text, ")" | "}" | "]")
            && let Some((open, start)) = stack.pop()
            && matches!((open, token.text), ("(", ")") | ("{", "}") | ("[", "]"))
            && line > start
        {
            folds.push(Fold { start, end: line });
        }
    }
    folds
}
