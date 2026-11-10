//! Text comparison for modules, on the `similar` crate (it compiles to wasm).
//!
//! [`compare`] lines two texts up into [`Row`]s: a line of the left text against a line of
//! the right one, or against nothing. Consecutive rows that differ form a [`Chunk`]. In
//! [`Mode::Words`] and [`Mode::Chars`] the paired lines of a chunk also carry the byte
//! ranges that differ inside them. An empty text has no lines.
//!
//! [`Diff::aligned`] gives a side's text with a [`GHOST`] line wherever the other side has a
//! line of its own, so both sides show the same rows; [`strip`] takes those lines out of
//! what the user edited, and [`Diff::take`] makes one side's chunk replace the other's.

use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffTag, capture_diff_slices};
use std::ops::Range;

/// The only content of a line that stands for no line of the text: it shows nothing.
pub const GHOST: char = '\u{200b}';

/// What the compared pieces of a changed line are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    /// Whole lines.
    #[default]
    Lines,
    /// Words and the punctuation between them, inside the changed lines.
    Words,
    /// Characters, inside the changed lines.
    Chars,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub mode: Mode,
    /// Lines are compared with their whitespace runs collapsed and trimmed; inside a line
    /// whitespace never differs.
    pub ignore_whitespace: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Same,
    /// Only the left text has the line.
    Removed,
    /// Only the right text has the line.
    Added,
    /// Both have a line there, and they differ.
    Changed,
}

/// A line of the left text against a line of the right one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The 0-based lines of the two texts; `None` where only the other side has one.
    pub left: Option<usize>,
    pub right: Option<usize>,
    pub change: Change,
    /// The chunk the row belongs to.
    pub chunk: Option<usize>,
    /// The byte ranges of the left and right line that differ (words, characters).
    pub left_marks: Vec<Range<usize>>,
    pub right_marks: Vec<Range<usize>>,
}

/// Consecutive rows that differ.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub rows: Range<usize>,
    /// The lines of each text the chunk covers; empty where the text has none there, the
    /// range then standing where they would go.
    pub left: Range<usize>,
    pub right: Range<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub rows: Vec<Row>,
    pub chunks: Vec<Chunk>,
}

fn lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').collect()
    }
}

fn collapse(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Compares `left` with `right`.
pub fn compare(left: &str, right: &str, options: Options) -> Diff {
    let (a, b) = (lines(left), lines(right));
    let key = |line: &&str| -> String {
        if options.ignore_whitespace {
            collapse(line)
        } else {
            (*line).to_owned()
        }
    };
    let (keys_a, keys_b): (Vec<String>, Vec<String>) =
        (a.iter().map(key).collect(), b.iter().map(key).collect());
    let mut rows = Vec::new();
    let row = |left: Option<usize>, right: Option<usize>, change| Row {
        left,
        right,
        change,
        chunk: None,
        left_marks: Vec::new(),
        right_marks: Vec::new(),
    };
    for op in capture_diff_slices(Algorithm::Myers, &keys_a, &keys_b) {
        let (tag, old, new) = op.as_tag_tuple();
        match tag {
            DiffTag::Equal => rows.extend(
                old.zip(new)
                    .map(|(l, r)| row(Some(l), Some(r), Change::Same)),
            ),
            DiffTag::Delete => rows.extend(old.map(|l| row(Some(l), None, Change::Removed))),
            DiffTag::Insert => rows.extend(new.map(|r| row(None, Some(r), Change::Added))),
            DiffTag::Replace => {
                let paired = old.len().min(new.len());
                for offset in 0..paired {
                    let (l, r) = (old.start + offset, new.start + offset);
                    let mut changed = row(Some(l), Some(r), Change::Changed);
                    if options.mode != Mode::Lines {
                        (changed.left_marks, changed.right_marks) = inline(a[l], b[r], options);
                    }
                    rows.push(changed);
                }
                rows.extend(
                    (old.start + paired..old.end).map(|l| row(Some(l), None, Change::Removed)),
                );
                rows.extend(
                    (new.start + paired..new.end).map(|r| row(None, Some(r), Change::Added)),
                );
            }
        }
    }
    let mut chunks: Vec<Chunk> = Vec::new();
    let (mut next_left, mut next_right) = (0, 0);
    for (index, row) in rows.iter_mut().enumerate() {
        if row.change != Change::Same {
            match chunks.last_mut().filter(|chunk| chunk.rows.end == index) {
                Some(chunk) => chunk.rows.end += 1,
                None => chunks.push(Chunk {
                    rows: index..index + 1,
                    left: next_left..next_left,
                    right: next_right..next_right,
                }),
            }
            row.chunk = Some(chunks.len() - 1);
        }
        if let Some(left) = row.left {
            next_left = left + 1;
        }
        if let Some(right) = row.right {
            next_right = right + 1;
        }
        if let Some(chunk) = row.chunk.map(|chunk| &mut chunks[chunk]) {
            chunk.left.end = next_left;
            chunk.right.end = next_right;
        }
    }
    Diff { rows, chunks }
}

/// The pieces of a line the mode compares, as byte ranges.
fn pieces(line: &str, options: Options) -> Vec<Range<usize>> {
    let kind = |c: char| match c {
        c if c.is_whitespace() => 0,
        c if c.is_alphanumeric() || c == '_' => 1,
        _ => 2,
    };
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut last = None;
    for (at, c) in line.char_indices() {
        let class = kind(c);
        if class == 0 && options.ignore_whitespace {
            last = None;
            continue;
        }
        let end = at + c.len_utf8();
        let joins = options.mode == Mode::Words && class != 2 && last == Some(class);
        match out.last_mut() {
            Some(piece) if joins => piece.end = end,
            _ => out.push(at..end),
        }
        last = Some(class);
    }
    out
}

/// The ranges of two paired lines that differ, by their words or characters.
fn inline(left: &str, right: &str, options: Options) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let (a, b) = (pieces(left, options), pieces(right, options));
    let words = |line: &str, pieces: &[Range<usize>]| -> Vec<String> {
        pieces
            .iter()
            .map(|piece| line[piece.clone()].to_owned())
            .collect()
    };
    let (mut gone, mut new) = (Vec::new(), Vec::new());
    for op in capture_diff_slices(Algorithm::Myers, &words(left, &a), &words(right, &b)) {
        let (tag, old, added) = op.as_tag_tuple();
        if matches!(tag, DiffTag::Delete | DiffTag::Replace) {
            gone.push(a[old.start].start..a[old.end - 1].end);
        }
        if matches!(tag, DiffTag::Insert | DiffTag::Replace) {
            new.push(b[added.start].start..b[added.end - 1].end);
        }
    }
    (merge(left, gone, options), merge(right, new, options))
}

/// Joins ranges with nothing (or only ignored whitespace) between them.
fn merge(line: &str, ranges: Vec<Range<usize>>, options: Options) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        match out.last_mut() {
            Some(last)
                if last.end == range.start
                    || options.ignore_whitespace
                        && line[last.end..range.start].trim().is_empty() =>
            {
                last.end = range.end;
            }
            _ => out.push(range),
        }
    }
    out
}

impl Diff {
    pub fn is_same(&self) -> bool {
        self.chunks.is_empty()
    }

    /// The text of `side` with a [`GHOST`] line wherever only the other side has a line, so
    /// that line `n` of both texts is row `n`.
    pub fn aligned(&self, side: Side, text: &str) -> String {
        let lines = lines(text);
        self.rows
            .iter()
            .map(|row| match side {
                Side::Left => row.left,
                Side::Right => row.right,
            })
            .map(|line| line.map_or_else(|| GHOST.to_string(), |line| lines[line].to_owned()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The text of the side opposite `from` once chunk `chunk` of `from` replaces the other
    /// side's piece.
    pub fn take(&self, chunk: usize, from: Side, left: &str, right: &str) -> String {
        let (source, target) = match from {
            Side::Left => (left, right),
            Side::Right => (right, left),
        };
        let Some(chunk) = self.chunks.get(chunk) else {
            return target.to_owned();
        };
        let (from_range, to_range) = match from {
            Side::Left => (chunk.left.clone(), chunk.right.clone()),
            Side::Right => (chunk.right.clone(), chunk.left.clone()),
        };
        let (source, target) = (lines(source), lines(target));
        let mut out: Vec<&str> = target[..to_range.start].to_vec();
        out.extend(&source[from_range]);
        out.extend(&target[to_range.end..]);
        out.join("\n")
    }
}

/// The text without what [`Diff::aligned`] added: lines that are only a [`GHOST`] go, and a
/// ghost the user typed next to is dropped.
pub fn strip(text: &str) -> String {
    if !text.contains(GHOST) {
        return text.to_owned();
    }
    text.split('\n')
        .filter(|line| *line != GHOST.to_string())
        .map(|line| line.replace(GHOST, ""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The UTF-16 column of byte `at` of `line`, the unit the editors count in.
pub fn utf16(line: &str, at: usize) -> usize {
    line[..at.min(line.len())].encode_utf16().count()
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    fn lines_mode() -> Options {
        Options::default()
    }

    #[test]
    fn identical_texts_have_no_chunks() {
        let diff = compare("a\nb", "a\nb", lines_mode());
        assert!(diff.is_same());
        assert_eq!(diff.rows.len(), 2);
        assert!(compare("", "", lines_mode()).is_same());
    }

    #[test]
    fn lines_pair_up_and_the_rest_stands_alone() {
        let diff = compare("a\nb\nc", "a\nx\ny\nc", lines_mode());
        let shape: Vec<_> = diff
            .rows
            .iter()
            .map(|row| (row.left, row.right, row.change))
            .collect();
        assert_eq!(
            shape,
            [
                (Some(0), Some(0), Change::Same),
                (Some(1), Some(1), Change::Changed),
                (None, Some(2), Change::Added),
                (Some(2), Some(3), Change::Same),
            ]
        );
        assert_eq!(diff.chunks.len(), 1);
        assert_eq!(diff.chunks[0].rows, 1..3);
        assert_eq!(diff.chunks[0].left, 1..2);
        assert_eq!(diff.chunks[0].right, 1..3);
        assert!(diff.rows[1].left_marks.is_empty());
    }

    #[test]
    fn a_pure_insertion_has_an_empty_range_on_the_other_side() {
        let diff = compare("a\nc", "a\nb\nc", lines_mode());
        assert_eq!(diff.chunks[0].left, 1..1);
        assert_eq!(diff.chunks[0].right, 1..2);
        assert_eq!(diff.rows[1].change, Change::Added);
    }

    #[test]
    fn words_mark_what_differs_inside_a_line() {
        let options = Options {
            mode: Mode::Words,
            ..Options::default()
        };
        let diff = compare("the quick fox", "the slow fox", options);
        let row = &diff.rows[0];
        assert_eq!(row.left_marks, [4..9]);
        assert_eq!(row.right_marks, [4..8]);
    }

    #[test]
    fn characters_mark_single_characters() {
        let options = Options {
            mode: Mode::Chars,
            ..Options::default()
        };
        let diff = compare("color", "colour", options);
        assert!(diff.rows[0].left_marks.is_empty());
        assert_eq!(diff.rows[0].right_marks, [4..5]);
        let diff = compare("abc", "axc", options);
        assert_eq!(diff.rows[0].left_marks, [1..2]);
    }

    #[test]
    fn ignoring_whitespace_equates_lines_and_pieces() {
        let ignore = Options {
            ignore_whitespace: true,
            ..Options::default()
        };
        assert!(compare("a  b \n c", "a b\nc", ignore).is_same());
        assert!(!compare("a  b", "a b", lines_mode()).is_same());
        let words = Options {
            mode: Mode::Words,
            ignore_whitespace: true,
        };
        let diff = compare("one  two three", "one two four", words);
        assert_eq!(diff.rows[0].left_marks, [9..14]);
        assert_eq!(diff.rows[0].right_marks, [8..12]);
    }

    #[test]
    fn aligned_texts_have_the_same_rows_and_strip_back() {
        let (left, right) = ("a\nb\nc", "a\nx\ny\nc");
        let diff = compare(left, right, lines_mode());
        let (aligned_left, aligned_right) = (
            diff.aligned(Side::Left, left),
            diff.aligned(Side::Right, right),
        );
        assert_eq!(aligned_left, format!("a\nb\n{GHOST}\nc"));
        assert_eq!(aligned_right, "a\nx\ny\nc");
        assert_eq!(strip(&aligned_left), left);
        assert_eq!(strip(&format!("{GHOST}a{GHOST}")), "a");
    }

    #[test]
    fn taking_a_chunk_replaces_the_other_sides_piece() {
        let (left, right) = ("a\nb\nc", "a\nx\ny\nc");
        let diff = compare(left, right, lines_mode());
        assert_eq!(diff.take(0, Side::Left, left, right), "a\nb\nc");
        assert_eq!(diff.take(0, Side::Right, left, right), "a\nx\ny\nc");
        let diff = compare("a\nc", "a\nb\nc", lines_mode());
        assert_eq!(diff.take(0, Side::Left, "a\nc", "a\nb\nc"), "a\nc");
        assert_eq!(diff.take(0, Side::Right, "a\nc", "a\nb\nc"), "a\nb\nc");
        assert_eq!(diff.take(9, Side::Right, "a\nc", "a\nb\nc"), "a\nc");
    }

    #[test]
    fn utf16_columns_count_surrogates() {
        assert_eq!(utf16("a😀b", 5), 3);
    }
}
