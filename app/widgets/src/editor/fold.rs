//! Fold ranges: what the editor's extension folds, else JSON brackets.
use super::highlight::Highlighter;
use super::state::Fold;
use super::text::Position;

/// The range a fold starting at `line` covers.
pub fn range(highlighter: &mut Highlighter, lines: &[String], line: usize) -> Option<Fold> {
    lines.get(line)?;
    if let Some(fold) = highlighter.extension().fold(lines, line) {
        return fold;
    }
    brackets(lines, line)
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

    fn lines(text: &str) -> Vec<String> {
        text.split('\n').map(str::to_owned).collect()
    }

    fn at(line: usize, ch: usize) -> Position {
        Position { line, ch }
    }

    #[test]
    fn brackets_fold_past_quoted_ones() {
        let doc = lines("{\n  \"x\": \"}\"\n}\nx");
        let mut json = Highlighter::new("json");
        let fold =
            |h: &mut Highlighter, line| range(h, &doc, line).map(|fold| (fold.from, fold.to));
        assert_eq!(fold(&mut json, 0), Some((at(0, 1), at(2, 0))));
        assert_eq!(fold(&mut json, 3), None);
    }
}
