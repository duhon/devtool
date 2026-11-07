//! Fold ranges: response regions, checks and sections, module folds and JSON brackets.
use super::highlight::Highlighter;
use super::state::Fold;
use super::text::utf16_len;
use crate::document::Position;

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
    brackets(lines, line)
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
            let mut check_text = crate::kernel::check(service, &text, &serde_json::Value::Null);
            check_text.columns = highlighter
                .regions()
                .iter()
                .filter(|region| region.request == check)
                .flat_map(|region| &region.columns)
                .map(|(name, kind)| services_sdk::ColumnType::new(name, kind))
                .collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::ResponseRegion;

    fn lines(text: &str) -> Vec<String> {
        text.split('\n').map(str::to_owned).collect()
    }

    fn at(line: usize, ch: usize) -> Position {
        Position { line, ch }
    }

    #[test]
    fn checks_responses_and_brackets_fold() {
        let mut highlighter = Highlighter::new("console-none");
        let doc = lines("#check a\n{\n  \"x\": \"}\"\n}\n#response 200\n{}\n#check b\nx");
        highlighter.set_regions(&[ResponseRegion {
            request: 0,
            header: 4,
            end: 6,
            columns: Vec::new(),
        }]);
        let fold =
            |h: &mut Highlighter, line| range(h, &doc, line).map(|fold| (fold.from, fold.to));
        assert_eq!(fold(&mut highlighter, 0), Some((at(0, 8), at(5, 2))));
        assert_eq!(fold(&mut highlighter, 4), Some((at(4, 13), at(5, 2))));
        // Inside a check only its module folds.
        assert_eq!(fold(&mut highlighter, 1), None);
        assert_eq!(fold(&mut highlighter, 7), None);
        assert_eq!(fold(&mut highlighter, 6), Some((at(6, 8), at(7, 1))));
        let mut json = Highlighter::new("json");
        assert_eq!(fold(&mut json, 1), Some((at(1, 1), at(3, 0))));
    }
}
