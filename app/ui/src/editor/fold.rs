//! Fold ranges: response regions, checks and sections, GraphQL operations, JSON brackets
//! and the JSON values of vertical table rows.
use super::highlight::Highlighter;
use super::state::Fold;
use super::text::utf16_len;
use crate::document::{Position, ResponseRegion};
use services_contracts::columns::Kind;

/// Caches vertical JSON field lookups; cleared on every edit.
#[derive(Debug, Default)]
pub struct JsonFolds {
    /// The region header the fields belong to.
    region: Option<usize>,
    fields: Vec<Field>,
    scopes: Vec<Scope>,
    folds: Vec<(usize, Fold)>,
}

#[derive(Debug)]
struct Field {
    prefix: String,
    json: bool,
}

#[derive(Debug)]
struct Scope {
    from: usize,
    to: usize,
    field: usize,
}

impl JsonFolds {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

fn end_of(lines: &[String], line: usize) -> Position {
    Position {
        line,
        ch: utf16_len(&lines[line]),
    }
}

/// The range a fold starting at `line` covers.
pub fn range(highlighter: &mut Highlighter, lines: &[String], line: usize) -> Option<Fold> {
    let count = lines.len();
    let first = lines.get(line)?;
    let regions = highlighter.regions().to_vec();
    if let Some(region) = regions.iter().find(|region| region.header == line) {
        let end = region.end.saturating_sub(1).min(count - 1);
        return Some(Fold {
            from: end_of(lines, line),
            to: end_of(lines, end),
        });
    }
    let output = |line: usize| {
        regions
            .iter()
            .any(|region| line >= region.header && line < region.end)
    };
    if !output(line) && first.trim_start().starts_with('#') {
        let check = crate::console::is_check(first);
        let main = first.trim_start().starts_with("#respon");
        for end in line + 1..=count {
            let next = lines.get(end).map_or("", String::as_str);
            let boundary = if check {
                !output(end) && crate::console::is_check(next)
            } else if output(end) {
                regions.iter().any(|region| region.header == end)
            } else if main {
                crate::console::is_check(next) || next.trim_start().starts_with("#respon")
            } else {
                next.trim_start().starts_with('#')
            };
            if end == count || boundary {
                return (end > line + 1).then(|| Fold {
                    from: end_of(lines, line),
                    to: end_of(lines, end - 1),
                });
            }
        }
    }
    if let Some(service) = highlighter.config().kernel().map(str::to_owned) {
        return module_fold(highlighter, &service, lines, line);
    }
    let config = highlighter.config().clone();
    if config.grid() {
        if let Some(region) = regions
            .iter()
            .find(|region| line >= region.header && line < region.end)
        {
            let json = region.table.as_ref().is_some_and(|table| {
                table.json || table.types.values().any(|kind| *kind == Kind::Json)
            });
            if first.starts_with('|') || !first.contains(['{', '[']) || !json {
                return None;
            }
            return vertical_json(highlighter, lines, region, line);
        }
        if highlighter.state_before(lines, line + 1).response() {
            return None;
        }
    }
    let keyword = first
        .trim_start()
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .next()
        .unwrap_or("");
    if !output(line)
        && config.language("", false) == "graphql"
        && matches!(keyword, "query" | "mutation" | "subscription")
    {
        let state = highlighter.state_before(lines, line);
        if state.lex().graphql_depth == 0 && !state.lex().block_string {
            return operation(&config, lines, line, &output);
        }
    }
    brackets(lines, line)
}

fn operation(
    config: &super::highlight::Config,
    lines: &[String],
    line: usize,
    output: &dyn Fn(usize) -> bool,
) -> Option<Fold> {
    let mut end = line + 1;
    while end < lines.len() {
        let text = &lines[end];
        let marker = text
            .trim_start()
            .strip_prefix('#')
            .and_then(|text| text.split_whitespace().next());
        if output(end)
            || (config.console()
                && (crate::console::is_check(text)
                    || marker.is_some_and(|marker| {
                        marker.starts_with("respon") || config.section(marker)
                    })))
        {
            break;
        }
        end += 1;
    }
    let source = lines[line..end].join("\n");
    let (from, to) = crate::graphql_completion::operation_fold(&source)?;
    let position = |offset: usize| {
        let before = &source[..offset];
        let start = before.rfind('\n').map_or(0, |index| index + 1);
        Position {
            line: line + before.matches('\n').count(),
            ch: utf16_len(&before[start..]),
        }
    };
    Some(Fold {
        from: position(from),
        to: position(to),
    })
}

/// A fold the service's module reports for the check `line` is in: from after the first
/// bracket of `line` to the indentation of its last line.
fn module_fold(
    highlighter: &mut Highlighter,
    service: &str,
    lines: &[String],
    line: usize,
) -> Option<Fold> {
    let check = (0..line)
        .rev()
        .find(|&at| crate::console::is_check(&lines[at]))?;
    let folds = match &highlighter.module_folds {
        Some((at, folds)) if *at == check => folds.clone(),
        _ => {
            let end = (check + 1..lines.len())
                .find(|&at| crate::console::is_check(&lines[at]))
                .unwrap_or(lines.len());
            let text = lines[check + 1..end].join("\n");
            let check_text = crate::kernel::check(service, &text, &serde_json::Value::Null);
            let folds: std::rc::Rc<[(usize, usize)]> =
                crate::kernel::with(|modules| modules.fold(service, check_text))
                    .unwrap_or_default()
                    .into_iter()
                    .map(|fold| {
                        (
                            check + 1 + fold.start as usize,
                            check + 1 + fold.end as usize,
                        )
                    })
                    .collect();
            highlighter.module_folds = Some((check, folds.clone()));
            folds
        }
    };
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
    Some(Fold {
        from: Position {
            line,
            ch: from.unwrap_or_else(|| utf16_len(first)),
        },
        to: Position {
            line: end,
            ch: utf16_len(&last[..indent]),
        },
    })
}

/// From after the first `{`/`[` on `line` to its partner; quotes are skipped.
fn brackets(lines: &[String], line: usize) -> Option<Fold> {
    let mut depth = 0i32;
    let mut start = None;
    let mut quote = None;
    let mut escape = false;
    for (i, text) in lines.iter().enumerate().skip(line) {
        let mut utf16 = 0;
        for c in text.chars() {
            let at = utf16;
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
                    start.get_or_insert(Position {
                        line: i,
                        ch: at + 1,
                    });
                    depth += 1;
                }
                '}' | ']' => {
                    depth -= 1;
                    if depth == 0
                        && let Some(from) = start
                    {
                        return Some(Fold {
                            from,
                            to: Position { line: i, ch: at },
                        });
                    }
                }
                _ => {}
            }
        }
        if i == line && start.is_none() {
            return None;
        }
    }
    None
}

/// The JSON value of a vertical row field (`  name: {...}`) that `line` opens.
fn vertical_json(
    highlighter: &mut Highlighter,
    lines: &[String],
    region: &ResponseRegion,
    line: usize,
) -> Option<Fold> {
    let table = region.table.as_ref()?;
    if line <= region.header {
        return None;
    }
    let cache = &mut highlighter.json_folds;
    if cache.region != Some(region.header) {
        let width = table
            .columns
            .iter()
            .map(|name| name.chars().count())
            .max()
            .unwrap_or(0);
        *cache = JsonFolds {
            region: Some(region.header),
            fields: table
                .columns
                .iter()
                .map(|name| Field {
                    prefix: format!("{}{name}: ", " ".repeat(width - name.chars().count())),
                    json: table.types.get(name) == Some(&Kind::Json),
                })
                .collect(),
            ..JsonFolds::default()
        };
    }
    if let Some((_, fold)) = cache.folds.iter().find(|(at, _)| *at == line) {
        return Some(*fold);
    }
    let mut found = None;
    for at in (region.header + 1..=line).rev() {
        if let Some(scope) = cache
            .scopes
            .iter_mut()
            .find(|scope| at >= scope.from && at <= scope.to)
        {
            scope.to = scope.to.max(line);
            found = Some((scope.from, scope.field));
            break;
        }
        let text = &lines[at];
        if text.starts_with("*****") || text.starts_with('|') || text.starts_with('+') {
            return None;
        }
        if let Some(field) = cache
            .fields
            .iter()
            .position(|field| text.starts_with(&field.prefix))
        {
            cache.scopes.push(Scope {
                from: at,
                to: line,
                field,
            });
            if cache.scopes.len() > 32 {
                cache.scopes.remove(0);
            }
            found = Some((at, field));
            break;
        }
    }
    let (field_line, field) = found?;
    let field = &cache.fields[field];
    if !field.json {
        return None;
    }
    let value = lines[field_line][field.prefix.len()..].trim_start();
    if !value.starts_with(['{', '[']) {
        return None;
    }
    let prefix = field.prefix.len();
    let mut stack = Vec::new();
    let mut from = None;
    let (mut quoted, mut escaped) = (false, false);
    for (at, text) in lines.iter().enumerate().take(region.end).skip(line) {
        let skip = if at == field_line { prefix } else { 0 };
        let mut ch = utf16_len(&text[..skip]);
        for c in text[skip..].chars() {
            if quoted {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    quoted = false;
                }
            } else if c == '"' {
                quoted = true;
            } else if c == '{' || c == '[' {
                from.get_or_insert(Position {
                    line: at,
                    ch: ch + 1,
                });
                stack.push(if c == '{' { '}' } else { ']' });
            } else if c == '}' || c == ']' {
                if stack.pop() != Some(c) {
                    return None;
                }
                if stack.is_empty()
                    && let Some(from) = from
                {
                    let fold = Fold {
                        from,
                        to: Position { line: at, ch },
                    };
                    cache.folds.push((line, fold));
                    if cache.folds.len() > 128 {
                        cache.folds.remove(0);
                    }
                    return Some(fold);
                }
            }
            ch += c.len_utf16();
        }
        from?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::highlight::Config;
    use crate::tables::Metadata;
    use services_contracts::manifest::View;

    fn lines(text: &str) -> Vec<String> {
        text.split('\n').map(str::to_owned).collect()
    }

    fn at(line: usize, ch: usize) -> Position {
        Position { line, ch }
    }

    fn console(payload: &str, response: View) -> Highlighter {
        Highlighter::with_config(Config::console_for_tests(
            &[("sql", "sql")],
            payload,
            response,
        ))
    }

    #[test]
    fn checks_responses_and_brackets_fold() {
        let mut highlighter = console("json", View::BodyHeaders);
        let doc = lines("#check a\n{\n  \"x\": \"}\"\n}\n#response 200\n{}\n#check b\nx");
        highlighter.set_regions(&[ResponseRegion {
            request: 0,
            header: 4,
            end: 6,
            table: None,
        }]);
        let fold =
            |h: &mut Highlighter, line| range(h, &doc, line).map(|fold| (fold.from, fold.to));
        assert_eq!(fold(&mut highlighter, 0), Some((at(0, 8), at(5, 2))));
        assert_eq!(fold(&mut highlighter, 4), Some((at(4, 13), at(5, 2))));
        assert_eq!(fold(&mut highlighter, 1), Some((at(1, 1), at(3, 0))));
        assert_eq!(fold(&mut highlighter, 7), None);
        assert_eq!(fold(&mut highlighter, 6), Some((at(6, 8), at(7, 1))));
    }

    #[test]
    fn graphql_operations_fold_to_their_selection_set() {
        let mut highlighter = console("graphql", View::BodyHeaders);
        let doc = lines("#check\nquery Q {\n  a\n}\n#check\nquery {");
        let fold = range(&mut highlighter, &doc, 1).unwrap();
        assert_eq!((fold.from, fold.to), (at(1, 9), at(3, 0)));
    }

    #[test]
    fn vertical_json_fields_fold_their_value() {
        let mut highlighter = console("sql", View::Grid);
        let doc = lines(
            "#check\nSELECT 1\n#response\n***** 1. row *****\n  id: 1\ndata: {\"a\": [\n  1]}\n  id: 2",
        );
        highlighter.set_regions(&[ResponseRegion {
            request: 0,
            header: 2,
            end: 8,
            table: Some(Metadata {
                columns: vec!["id".into(), "data".into()],
                types: [("data".to_owned(), Kind::Json)].into(),
                header_lines: 1,
                json: false,
            }),
        }]);
        let fold = range(&mut highlighter, &doc, 5).unwrap();
        assert_eq!((fold.from, fold.to), (at(5, 7), at(6, 4)));
        assert_eq!(range(&mut highlighter, &doc, 4), None);
        highlighter.edited(6);
        assert_eq!(range(&mut highlighter, &doc, 5), Some(fold));
    }
}
