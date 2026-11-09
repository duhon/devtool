//! Console checks: a check typed or pasted right under text gets a blank line before it,
//! and checks alternate between two bands.
use super::is_check;
use super::{Position, ResponseRegion};

fn leading_whitespace(text: &str) -> usize {
    text.char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map_or(text.len(), |(index, _)| index)
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The line an edit starts on, as it was before the edit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Previous {
    pub line: usize,
    pub check: bool,
    /// The check is a response region's own check line.
    pub output: bool,
}

impl Previous {
    /// `text` is the line before the edit.
    pub fn capture(text: Option<&str>, line: usize, regions: &[ResponseRegion]) -> Self {
        let check = text.is_some_and(is_check);
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

/// The lines that get a blank line before them (the last first): checks an insertion of
/// `inserted` lines at `from` produced. `lines` is the text after the insertion,
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
) -> Option<Vec<usize>> {
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
    added.reverse();
    Some(added)
}

/// The band of `line`: `Some(true)` under the 1st, 3rd, … check, `Some(false)` under the
/// others, `None` above the first; `checks` are the sorted check lines.
pub fn band(checks: &[usize], line: usize) -> Option<bool> {
    match checks.partition_point(|check| *check <= line) {
        0 => None,
        count => Some(count % 2 == 1),
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
        let previous = Previous::capture(Some(&doc[2]), 2, &[]);
        assert!(separate(&doc, at(2, 5), &["k".into()], "+input", &[0], previous, &[]).is_none());
        let doc = lines("#check a\nx\n#check b");
        assert!(separate(&doc, at(2, 7), &["b".into()], "+input", &[0], previous, &[]).is_none());
        assert!(separate(&doc, at(2, 6), &[" ".into()], "+input", &[0], previous, &[]).is_some());
        assert!(separate(&doc, at(2, 6), &[" ".into()], "undo", &[0], previous, &[]).is_none());
    }

    #[test]
    fn bands_alternate_from_the_first_check() {
        let checks = [2, 5];
        assert_eq!(band(&checks, 0), None);
        assert_eq!(band(&checks, 2), Some(true));
        assert_eq!(band(&checks, 4), Some(true));
        assert_eq!(band(&checks, 5), Some(false));
    }
}
