//! The editor's language: what a service's module and the console add to the text of an
//! editor — highlighting, folds, completion and hints — in document terms (lines, byte
//! tokens, UTF-16 columns). The Shell lends it the running modules ([`Services`]) and turns
//! the results into the editor widget's props.

pub mod completion;
pub mod functions;
pub mod references;
pub mod templates;

pub use completion::{kernel_check, saved_hints};
pub use templates::{Expansion, expand_call, expand_eager};

use crate::document::{self, Position, ResponseRegion, is_check};
use serde_json::Value;
use services_contracts::templates::Library;
use services_sdk::refs::Root;
use services_sdk::{Check, ColumnType, Completions, Fold, Line, LineColumns, Token};

/// Lines past this many UTF-16 units stay unstyled.
pub const MAX_HIGHLIGHT: usize = 10_000;

/// What the language reads from the running modules. Every method answers for one service.
pub trait Services {
    /// Whether `service` is a module service, so its checks highlight and hint through it.
    fn is_service(&self, service: &str) -> bool;
    /// The `#marker` sections of a service's checks: its module's, then `options`.
    fn markers(&self, service: &str) -> Vec<String>;
    /// The module's highlighting of `lines`, starting in `state`; `None` when it has none.
    fn highlight(
        &self,
        service: &str,
        state: Vec<u8>,
        lines: Vec<String>,
        columns: Vec<LineColumns>,
    ) -> Option<Vec<Line>>;
    /// The module's folds of one check.
    fn fold(&self, service: &str, check: Check) -> Vec<Fold>;
    /// The module's marks of one check: tokens laid over its highlighting, per line.
    fn marks(&self, service: &str, check: Check) -> Vec<Line>;
    /// The module's completion of one check at UTF-8 offset `pos`.
    fn complete(&self, service: &str, check: Check, pos: u32) -> Option<Completions>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub text: String,
    pub label: Option<String>,
    pub detail: String,
    /// A letter in front of the item that tells where it comes from.
    pub badge: Option<char>,
    pub continue_hint: bool,
    pub literal: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Completion {
    pub from: usize,
    pub to: usize,
    pub candidates: Vec<Candidate>,
    pub comma: Option<(usize, usize)>,
    pub automatic: bool,
}

/// A highlighted line: its tokens as `(byte end, kind)` and the state after it.
pub type Highlighted = (Vec<(usize, Option<String>)>, Vec<u8>);

/// The module's state inside a template section: after its `#section` line.
pub fn section_state(services: &dyn Services, service: &str, section: Option<&str>) -> Vec<u8> {
    let Some(section) = section else {
        return Vec::new();
    };
    services
        .highlight(service, Vec::new(), vec![format!("#{section}")], Vec::new())
        .and_then(|mut lines| lines.pop())
        .map_or_else(Vec::new, |line| line.state)
}

/// Whether `text` is a template definition line: `@name(…) description`.
pub fn is_definition(text: &str) -> bool {
    text.starts_with('@')
}

/// The tokens of a template definition line `@name(params) description`, as `(byte end,
/// kind)`: the name, the parameter names, operators, strings and the description; `problems`
/// (byte ranges of references that offer no options) are marked over the references.
pub fn definition_tokens(text: &str, problems: &[(usize, usize)]) -> Vec<(usize, Option<String>)> {
    definition(text, problems).0
}

/// The parameter names of a template definition line.
pub fn definition_params(text: &str) -> Vec<String> {
    definition(text, &[]).1
}

/// The marks of the parameter uses `{{name}}` of `params` in a template body line.
pub fn parameter_marks(text: &str, params: &[String]) -> Vec<Token> {
    let mut marks = Vec::new();
    let mut from = 0;
    while let Some(offset) = text[from..].find("{{") {
        let start = from + offset;
        let inner = &text[start + 2..];
        let Some(close) = inner.find("}}") else {
            break;
        };
        if !services_sdk::refs::escaped(text, start)
            && params.iter().any(|param| param == inner[..close].trim())
        {
            let len = (close + 4) as u32;
            marks.push(Token {
                start: start as u32,
                len,
                kind: "tpl-arg".into(),
            });
            from = start + close + 4;
        } else {
            from = start + 2;
        }
    }
    marks
}

/// The style of `@`, brackets and commas in a definition: that of a reference's separators.
const MARK: &str = "gw-placeholder-colon";

fn definition(
    text: &str,
    problems: &[(usize, usize)],
) -> (Vec<(usize, Option<String>)>, Vec<String>) {
    let mut tokens: Vec<(usize, Option<String>)> = Vec::new();
    let mut names = Vec::new();
    let mut push = |end: usize, kind: Option<&str>| {
        if tokens.last().is_some_and(|(at, _)| *at >= end) {
            return;
        }
        match tokens.last_mut() {
            Some((at, last)) if last.as_deref() == kind => *at = end,
            _ => tokens.push((end, kind.map(str::to_owned))),
        }
    };
    let word = |at: usize, name: fn(char) -> bool| {
        text[at..]
            .find(|c: char| !name(c))
            .map_or(text.len(), |n| at + n)
    };
    let template_name = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.');
    let param_name = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-');
    let mut at = word(1, template_name);
    push(1, Some(MARK));
    push(at, Some("def"));
    if text[at..].starts_with('(') {
        let spans = services_sdk::refs::scan(text);
        push(at + 1, Some(MARK));
        at += 1;
        let mut expect_name = true;
        while let Some(c) = text[at..].chars().next() {
            if let Some(span) = spans.iter().find(|span| span.start == at) {
                let problem = problems.contains(&(span.start, span.end));
                push(span.end, problem.then_some("tpl-problem"));
                at = span.end;
                continue;
            }
            let next = at + c.len_utf8();
            match c {
                '"' => {
                    let mut escaped = false;
                    let end = text[next..]
                        .char_indices()
                        .find(|&(_, ch)| {
                            let close = ch == '"' && !escaped;
                            escaped = ch == '\\' && !escaped;
                            close
                        })
                        .map_or(text.len(), |(index, _)| next + index + 1);
                    push(end, Some("string"));
                    at = end;
                }
                ')' => {
                    push(next, Some(MARK));
                    break;
                }
                ',' => {
                    push(next, Some(MARK));
                    expect_name = true;
                    at = next;
                }
                ':' | '=' | '|' => {
                    push(next, Some("operator"));
                    expect_name = false;
                    at = next;
                }
                c if expect_name && param_name(c) => {
                    let end = word(at, param_name);
                    names.push(text[at..end].to_owned());
                    push(end, Some("tpl-arg"));
                    expect_name = false;
                    at = end;
                }
                _ => {
                    push(next, None);
                    at = next;
                }
            }
        }
    }
    push(text.len(), Some("comment"));
    (tokens, names)
}

/// One line of a service document from `state`: `#check` and template `@` lines are the
/// console's, the rest the module's, with the module's `marks` of the line laid over.
/// `region` is the response the line is in.
pub fn highlight(
    services: &dyn Services,
    service: &str,
    section: Option<&str>,
    region: Option<&ResponseRegion>,
    (index, text): (usize, &str),
    state: &[u8],
    marks: &[Token],
) -> Highlighted {
    // The response's header line carries the column types its module reported.
    let columns = region
        .filter(|region| region.header == index && !region.columns.is_empty())
        .map(|region| {
            vec![LineColumns {
                line: 0,
                columns: column_types(&region.columns),
            }]
        })
        .unwrap_or_default();
    let whole = |kind: Option<&str>| vec![(text.len(), kind.map(str::to_owned))];
    if section.is_some() && is_definition(text) {
        return (
            definition_tokens(text, &[]),
            section_state(services, service, section),
        );
    }
    if is_check(text) {
        return (whole(Some("comment")), Vec::new());
    }
    if text.encode_utf16().count() > MAX_HIGHLIGHT {
        return (whole(None), state.to_vec());
    }
    let highlighted = services.highlight(service, state.to_vec(), vec![text.to_owned()], columns);
    let Some(line) = highlighted.and_then(|mut lines| lines.pop()) else {
        let marker = text.trim_start().starts_with('#');
        return (whole(marker.then_some("comment")), state.to_vec());
    };
    let mut tokens = Vec::with_capacity(line.tokens.len() * 2);
    let mut at = 0;
    for token in overlay(line.tokens, marks) {
        let start = (token.start as usize).max(at);
        let end = (token.start as usize + token.len as usize).min(text.len());
        if start >= end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        if start > at {
            tokens.push((start, None));
        }
        tokens.push((end, (!token.kind.is_empty()).then_some(token.kind)));
        at = end;
    }
    if at < text.len() {
        tokens.push((text.len(), None));
    }
    (tokens, line.state)
}

/// `tokens` with `marks` laid over them: a token keeps its kind outside the marks.
fn overlay(tokens: Vec<Token>, marks: &[Token]) -> Vec<Token> {
    if marks.is_empty() {
        return tokens;
    }
    let mut out = marks.to_vec();
    for token in tokens {
        let mut pieces = vec![(token.start, token.start + token.len)];
        for mark in marks {
            let (from, to) = (mark.start, mark.start + mark.len);
            pieces = pieces
                .into_iter()
                .flat_map(|(start, end)| {
                    [(start, end.min(from)), (start.max(to), end)]
                        .into_iter()
                        .filter(|(start, end)| start < end)
                })
                .collect();
        }
        out.extend(pieces.into_iter().map(|(start, end)| Token {
            start,
            len: end - start,
            kind: token.kind.clone(),
        }));
    }
    out.sort_by_key(|token| token.start);
    out
}

fn column_types(columns: &[(String, String)]) -> Vec<ColumnType> {
    columns
        .iter()
        .map(|(name, kind)| ColumnType::new(name, kind))
        .collect()
}

fn end_of(lines: &[String], line: usize) -> Position {
    Position {
        line,
        ch: lines[line].encode_utf16().count(),
    }
}

/// The console's fold starting at `line`: a response from its header, a check to the next
/// one, a `#section` to the next marker. `None` when the line starts none of them.
pub fn fold(
    lines: &[String],
    line: usize,
    regions: &[ResponseRegion],
) -> Option<Option<(Position, Position)>> {
    let count = lines.len();
    let first = lines.get(line)?;
    if let Some(region) = regions.iter().find(|region| region.header == line) {
        let end = region.end.saturating_sub(1).min(count - 1);
        return Some(Some((end_of(lines, line), end_of(lines, end))));
    }
    let output = |line: usize| {
        regions
            .iter()
            .any(|region| line >= region.header && line < region.end)
    };
    if output(line) || !first.trim_start().starts_with('#') {
        return None;
    }
    let check = is_check(first);
    let main = first.trim_start().starts_with("#respon");
    for end in line + 1..=count {
        let next = lines.get(end).map_or("", String::as_str);
        let boundary = if check {
            !output(end) && is_check(next)
        } else if output(end) {
            regions.iter().any(|region| region.header == end)
        } else if main {
            is_check(next) || next.trim_start().starts_with("#respon")
        } else {
            next.trim_start().starts_with('#')
        };
        if end == count || boundary {
            return Some((end > line + 1).then(|| (end_of(lines, line), end_of(lines, end - 1))));
        }
    }
    None
}

/// The check `line` is in: its `#check` line, the line after its last and what its module
/// reads; `None` above the first check.
fn module_check(
    service: &str,
    lines: &[String],
    line: usize,
    regions: &[ResponseRegion],
) -> Option<(usize, usize, Check)> {
    let check = (0..line).rev().find(|&at| is_check(&lines[at]))?;
    let end = (check + 1..lines.len())
        .find(|&at| is_check(&lines[at]))
        .unwrap_or(lines.len());
    let text = lines[check + 1..end].join("\n");
    let columns: Vec<(String, String)> = regions
        .iter()
        .filter(|region| region.request == check)
        .flat_map(|region| region.columns.iter().cloned())
        .collect();
    let mut module_check = document::check(service, &text, &Value::Null);
    module_check.columns = column_types(&columns);
    Some((check, end, module_check))
}

/// The module's folds of the check `line` is in, as `(start, end)` document lines, and that
/// check's line; `None` above the first check.
pub fn module_folds(
    services: &dyn Services,
    service: &str,
    lines: &[String],
    line: usize,
    regions: &[ResponseRegion],
) -> Option<(usize, Vec<(usize, usize)>)> {
    let (check, _, module_check) = module_check(service, lines, line, regions)?;
    let folds = services
        .fold(service, module_check)
        .into_iter()
        .map(|fold| {
            (
                check + 1 + fold.start as usize,
                check + 1 + fold.end as usize,
            )
        })
        .collect();
    Some((check, folds))
}

/// The module's marks of the check `line` is in: its `#check` line, the line after its
/// last, and the marks of its lines from the one after `#check`; `None` above the first
/// check.
pub fn module_marks(
    services: &dyn Services,
    service: &str,
    lines: &[String],
    line: usize,
    regions: &[ResponseRegion],
) -> Option<(usize, usize, Vec<Line>)> {
    let (check, end, module_check) = module_check(service, lines, line, regions)?;
    Some((check, end, services.marks(service, module_check)))
}

/// A module fold starting at `line` among `folds`: from after the first bracket of `line`
/// to the indentation of its last line.
pub fn module_fold(
    lines: &[String],
    line: usize,
    folds: &[(usize, usize)],
) -> Option<(Position, Position)> {
    let &(_, end) = folds
        .iter()
        .find(|&&(start, end)| start == line && end > line && end < lines.len())?;
    let first = &lines[line];
    let mut quote = None;
    let mut escape = false;
    let mut from = None;
    let mut utf16 = 0;
    for c in first.chars() {
        utf16 += c.len_utf16();
        if let Some(q) = quote {
            if c == q && !escape {
                quote = None;
            }
            escape = c == '\\' && !escape;
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '{' | '[' => {
                from = Some(utf16);
                break;
            }
            _ => {}
        }
    }
    let last = &lines[end];
    let indent = last.len() - last.trim_start().len();
    Some((
        Position {
            line,
            ch: from.unwrap_or_else(|| first.encode_utf16().count()),
        },
        Position {
            line: end,
            ch: last[..indent].encode_utf16().count(),
        },
    ))
}

/// Completion at `line`/`ch` of `source` (response regions masked out). `field` is empty
/// for a console; otherwise the editor holds one section and `field` names it. `scope` is
/// the variable tree and the jq definitions a `{{$name(…)}}` call sees.
#[allow(clippy::too_many_arguments)]
pub fn suggest(
    services: &dyn Services,
    service: &str,
    field: &str,
    source: &str,
    (line, ch): (usize, usize),
    hints: &[(String, String)],
    library: &Library,
    scope: (&Root, &str),
) -> Completion {
    templates::suggest(services, service, field, source, line, ch, library, scope).unwrap_or_else(
        || {
            completion::suggest(
                services, service, field, source, line, ch, hints, library, scope,
            )
        },
    )
}

/// The check and offset hints were asked for in a console of a module service, so its
/// module may fetch what they need; `source` has the response regions masked out.
pub fn hints(
    services: &dyn Services,
    service: &str,
    source: &str,
    line: usize,
    ch: usize,
) -> Option<(Check, u32)> {
    if !services.is_service(service) {
        return None;
    }
    kernel_check(service, source, line, ch)
}

/// The text a completed console marker inserts: its line break, or a space after
/// `#check` for the title; a marker on a marker line, or one with a `$0` cursor, stays.
pub fn insertion(services: &dyn Services, service: &str, candidate: &str, line: &str) -> String {
    if !candidate.starts_with('#') || candidate.contains("$0") {
        return candidate.to_owned();
    }
    let current = line.trim();
    if is_check(current)
        || services
            .markers(service)
            .iter()
            .any(|marker| current.eq_ignore_ascii_case(&format!("#{marker}")))
    {
        return candidate.to_owned();
    }
    marker_insertion(candidate)
}

/// A console marker with what follows it: a space after `#check`, a line break otherwise.
pub fn marker_insertion(marker: &str) -> String {
    format!("{marker}{}", if marker == "#check" { " " } else { "\n" })
}

/// Template library sections of a service with `markers`: `check` (whole checks), then
/// its `#marker` sections.
pub fn template_sections(markers: Vec<String>) -> Vec<String> {
    if markers.is_empty() {
        return Vec::new();
    }
    std::iter::once("check".to_owned())
        .chain(markers.into_iter().filter(|marker| marker != "check"))
        .collect()
}

/// `query`'s characters appear in `candidate` in order, ignoring case.
pub fn fuzzy_match(candidate: &str, query: &str) -> bool {
    let mut chars = candidate.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .all(|q| chars.by_ref().any(|c| c == q))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(start: u32, len: u32, kind: &str) -> Token {
        Token {
            start,
            len,
            kind: kind.into(),
        }
    }

    #[test]
    fn definition_lines_color_their_parts() {
        let line = "@t(a: {{:x}}, b: \"q,)\" | \"s\" = \"q\") Desc";
        let mut start = 0;
        let pieces: Vec<(&str, Option<String>)> = definition_tokens(line, &[(6, 12)])
            .into_iter()
            .map(|(end, kind)| {
                let piece = &line[start..end];
                start = end;
                (piece, kind)
            })
            .collect();
        let expected = [
            ("@", Some("gw-placeholder-colon")),
            ("t", Some("def")),
            ("(", Some("gw-placeholder-colon")),
            ("a", Some("tpl-arg")),
            (":", Some("operator")),
            (" ", None),
            ("{{:x}}", Some("tpl-problem")),
            (",", Some("gw-placeholder-colon")),
            (" ", None),
            ("b", Some("tpl-arg")),
            (":", Some("operator")),
            (" ", None),
            ("\"q,)\"", Some("string")),
            (" ", None),
            ("|", Some("operator")),
            (" ", None),
            ("\"s\"", Some("string")),
            (" ", None),
            ("=", Some("operator")),
            (" ", None),
            ("\"q\"", Some("string")),
            (")", Some("gw-placeholder-colon")),
            (" Desc", Some("comment")),
        ];
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(piece, kind)| (piece, kind.map(str::to_owned)))
            .collect();
        assert_eq!(pieces, expected);
        assert_eq!(definition_params(line), ["a", "b"]);
        assert_eq!(
            definition_tokens("@t", &[]),
            vec![
                (1, Some("gw-placeholder-colon".to_owned())),
                (2, Some("def".to_owned()))
            ]
        );
        let marks = parameter_marks("x{{a}}{{:a}}{{ b }}{{c}}", &["a".into(), "b".into()]);
        let spans: Vec<_> = marks.iter().map(|mark| (mark.start, mark.len)).collect();
        assert_eq!(spans, [(1, 5), (12, 7)]);
    }

    #[test]
    fn marks_replace_the_kind_only_where_they_cover() {
        let tokens = vec![token(0, 5, "keyword"), token(6, 10, "property")];
        let marks = [token(6, 5, "graphql-object-field"), token(13, 1, "x")];
        let kinds: Vec<_> = overlay(tokens, &marks)
            .into_iter()
            .map(|t| (t.start, t.len, t.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                (0, 5, "keyword".into()),
                (6, 5, "graphql-object-field".into()),
                (11, 2, "property".into()),
                (13, 1, "x".into()),
                (14, 2, "property".into()),
            ]
        );
    }
}
