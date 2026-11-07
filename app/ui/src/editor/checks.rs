//! Console checks: a check typed or pasted right under text gets a blank line before it,
//! and checks get alternating band classes.
use super::state::{Change, Transaction};
use super::text::{leading_whitespace, utf16_len};
use crate::console::is_check;
use crate::document::{Position, ResponseRegion};

/// The line an edit starts on, as it was before the edit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Previous {
    pub line: usize,
    pub check: bool,
    /// The check is a response region's own check line.
    pub output: bool,
}

impl Previous {
    pub fn capture(lines: &[String], line: usize, regions: &[ResponseRegion]) -> Self {
        let check = lines.get(line).is_some_and(|text| is_check(text));
        Self {
            line,
            check,
            output: check
                && regions
                    .iter()
                    .any(|region| line >= region.header && line < region.end),
        }
    }
}

/// The UTF-16 length of a check line's `#check`/`#request` marker, with its indentation.
fn marker(text: &str) -> &str {
    let start = leading_whitespace(text) + 1;
    let word = if text[start..].to_ascii_lowercase().starts_with("check") {
        5
    } else {
        7
    };
    &text[..(start + word).min(text.len())]
}

/// Blank lines before the checks an insertion of `inserted` lines at `from` produced, as a
/// transaction joining the insertion's undo step. `lines` is the text after the insertion,
/// `checks` the check lines before it, `pasted` the response regions of pasted text
/// (line numbers relative to it), which keep their layout.
pub fn separate(
    lines: &[String],
    from: Position,
    inserted: &[String],
    origin: &str,
    checks: &[usize],
    previous: Previous,
    pasted: &[ResponseRegion],
) -> Option<Transaction> {
    if !matches!(origin, "+input" | "paste" | "complete") {
        return None;
    }
    let mut protected: Vec<_> = pasted
        .iter()
        .map(|region| (region.header, region.end))
        .collect();
    protected.sort_unstable();
    let mut next = 0;
    let mut seen = None;
    let mut added = Vec::new();
    for i in 0..inserted.len() {
        let number = from.line + i;
        let Some(text) = lines
            .get(number)
            .filter(|text| !text.is_empty() && is_check(text))
        else {
            continue;
        };
        while next < protected.len() && protected[next].1 <= i {
            next += 1;
        }
        if next < protected.len() && protected[next].0 <= i {
            continue;
        }
        if i == 0
            && previous.line == number
            && previous.check
            && previous.output
            && origin != "paste"
        {
            continue;
        }
        let seen = seen.get_or_insert_with(|| checks.iter().any(|line| *line < from.line));
        let marker = marker(text);
        let single = origin == "+input" && inserted.len() == 1;
        let title = i == 0 && single && from.ch > utf16_len(marker);
        let incomplete = single
            && text.trim().to_lowercase() == marker.trim().to_lowercase()
            && !inserted[0].contains(char::is_whitespace);
        if *seen && !title && !incomplete && number > 0 && !lines[number - 1].trim().is_empty() {
            added.push(number);
        }
        *seen = true;
    }
    if added.is_empty() {
        return None;
    }
    Some(Transaction {
        changes: added
            .into_iter()
            .rev()
            .map(|line| Change {
                from: Position { line, ch: 0 },
                to: Position { line, ch: 0 },
                text: "\n".into(),
            })
            .collect(),
        origin: origin.into(),
        selection: None,
        join: true,
    })
}

/// The band class of `line`; `checks` are the sorted check lines.
pub fn band(checks: &[usize], line: usize) -> Option<&'static str> {
    match checks.partition_point(|check| *check <= line) {
        0 => None,
        count if count % 2 == 1 => Some("console-check-band-even"),
        _ => Some("console-check-band-odd"),
    }
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
    fn checks_after_text_get_a_blank_line() {
        let doc = lines("#check a\nx\n#check b\ny\n#check c");
        let inserted = lines("#check b\ny\n#check c");
        let separate = |checks: &[usize], pasted: &[ResponseRegion]| {
            separate(
                &doc,
                at(2, 0),
                &inserted,
                "paste",
                checks,
                Previous::default(),
                pasted,
            )
            .map(|transaction| {
                transaction
                    .changes
                    .iter()
                    .map(|change| change.from.line)
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(separate(&[0], &[]), Some(vec![4, 2]));
        // Without an earlier check the first one stays put.
        assert_eq!(separate(&[], &[]), Some(vec![4]));
        let pasted = ResponseRegion {
            request: 0,
            header: 2,
            end: 3,
            columns: Vec::new(),
        };
        assert_eq!(separate(&[0], &[pasted]), Some(vec![2]));
    }

    #[test]
    fn typing_a_marker_or_title_does_not_separate() {
        let doc = lines("#check a\nx\n#check");
        let previous = Previous::capture(&doc, 2, &[]);
        assert_eq!(
            separate(&doc, at(2, 5), &["k".into()], "+input", &[0], previous, &[]).map(|t| t.join),
            None
        );
        let doc = lines("#check a\nx\n#check b");
        assert_eq!(
            separate(&doc, at(2, 7), &["b".into()], "+input", &[0], previous, &[]).map(|t| t.join),
            None
        );
        assert_eq!(
            separate(&doc, at(2, 6), &[" ".into()], "+input", &[0], previous, &[]).map(|t| t.join),
            Some(true)
        );
        assert_eq!(
            separate(&doc, at(2, 6), &[" ".into()], "undo", &[0], previous, &[]).map(|t| t.join),
            None
        );
    }

    #[test]
    fn bands_alternate_from_the_first_check() {
        let checks = [2, 5];
        assert_eq!(band(&checks, 0), None);
        assert_eq!(band(&checks, 2), Some("console-check-band-even"));
        assert_eq!(band(&checks, 4), Some("console-check-band-even"));
        assert_eq!(band(&checks, 5), Some("console-check-band-odd"));
    }
}
