//! The editor model: text, selection, undo history, folds and line anchors.
//! Pure, so native tests cover it; the browser view only renders it.
use super::history::{History, Step};
use super::text::{byte_at, ch_at, max, min, split_lines, utf16_len};
use crate::editor::text::{Position, TextChange};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Position,
    pub head: Position,
}

impl Selection {
    pub fn cursor(at: Position) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }
    pub fn from(&self) -> Position {
        min(self.anchor, self.head)
    }
    pub fn to(&self) -> Position {
        max(self.anchor, self.head)
    }
    pub fn empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// Replace `from..to` with `text`; positions are in the document as it is when the change applies.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub from: Position,
    pub to: Position,
    pub text: String,
}

/// Changes applied in order as one undo step.
#[derive(Clone, Debug, Default)]
pub struct Transaction {
    pub changes: Vec<Change>,
    pub origin: String,
    /// The selection afterwards; by default the old one maps through the changes.
    pub selection: Option<Selection>,
    /// Joins the previous transaction's operation, so both form one undo step.
    pub join: bool,
}

impl Transaction {
    pub fn replace(from: Position, to: Position, text: impl Into<String>, origin: &str) -> Self {
        Self {
            changes: vec![Change {
                from,
                to,
                text: text.into(),
            }],
            origin: origin.into(),
            selection: None,
            join: false,
        }
    }
}

/// A line that keeps its identity while edits move it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Anchor(u64);

impl Anchor {
    /// A number unique to the line, e.g. to key its rendered row.
    pub fn key(self) -> u64 {
        self.0
    }
}

/// A collapsed range; the lines after `from.line` through `to.line` are hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fold {
    pub from: Position,
    pub to: Position,
}

#[derive(Clone, Debug)]
pub struct State {
    lines: Vec<String>,
    ids: Vec<u64>,
    next_id: u64,
    selection: Selection,
    history: History,
    /// Every fold, nested ones included, by start.
    folds: Vec<Fold>,
    /// The folds no other fold contains: what the screen shows.
    outer: Vec<Fold>,
    operation: u64,
    /// While held, every transaction joins the operation the first began.
    held: Option<bool>,
    version: u64,
    /// The display column vertical moves keep.
    pub(super) goal: Option<usize>,
}

impl Default for State {
    fn default() -> Self {
        Self::new("")
    }
}

impl State {
    pub fn new(text: &str) -> Self {
        let lines = split_lines(text);
        let ids = (0..lines.len() as u64).collect();
        Self {
            next_id: lines.len() as u64,
            lines,
            ids,
            selection: Selection::default(),
            history: History::default(),
            folds: Vec::new(),
            outer: Vec::new(),
            operation: 0,
            held: None,
            version: 0,
            goal: None,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
    pub fn line(&self, line: usize) -> &str {
        self.lines.get(line).map_or("", String::as_str)
    }
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
    pub fn last_line(&self) -> usize {
        self.lines.len() - 1
    }
    /// Changes whenever the text, selection or folds change.
    pub fn version(&self) -> u64 {
        self.version
    }
    pub fn selection(&self) -> Selection {
        self.selection
    }
    pub fn cursor(&self) -> Position {
        self.selection.head
    }
    pub fn folds(&self) -> &[Fold] {
        &self.folds
    }
    /// The folds no other fold contains, by start.
    pub fn outer_folds(&self) -> &[Fold] {
        &self.outer
    }

    pub fn clip(&self, at: Position) -> Position {
        let line = at.line.min(self.last_line());
        Position {
            line,
            ch: at.ch.min(utf16_len(&self.lines[line])),
        }
    }

    pub fn range(&self, from: Position, to: Position) -> String {
        self.removed(self.clip(min(from, to)), self.clip(max(from, to)))
            .join("\n")
    }

    pub fn selected_text(&self) -> String {
        self.range(self.selection.from(), self.selection.to())
    }

    /// The UTF-16 offset of `at` in the whole text.
    pub fn index(&self, at: Position) -> usize {
        let at = self.clip(at);
        self.lines[..at.line]
            .iter()
            .map(|line| utf16_len(line) + 1)
            .sum::<usize>()
            + at.ch
    }

    pub fn position(&self, mut index: usize) -> Position {
        for (line, text) in self.lines.iter().enumerate() {
            let length = utf16_len(text);
            if index <= length {
                return Position { line, ch: index };
            }
            index -= length + 1;
        }
        let line = self.last_line();
        Position {
            line,
            ch: utf16_len(&self.lines[line]),
        }
    }

    /// Selects; a position inside a fold opens it.
    pub fn set_selection(&mut self, selection: Selection) -> bool {
        let selection = Selection {
            anchor: self.clip(selection.anchor),
            head: self.clip(selection.head),
        };
        let folds = self.folds.len();
        self.folds
            .retain(|fold| !inside(selection.head, fold) && !inside(selection.anchor, fold));
        self.sync_folds();
        if selection == self.selection && folds == self.folds.len() {
            return false;
        }
        self.selection = selection;
        self.goal = None;
        self.version += 1;
        true
    }

    /// Makes the transactions until `hold(false)` one undo step.
    pub fn hold(&mut self, held: bool) {
        self.held = held.then_some(false);
    }

    pub fn apply(&mut self, transaction: Transaction, time: f64) -> Vec<TextChange> {
        if !transaction.join && self.held != Some(true) {
            self.operation += 1;
        }
        if self.held.is_some() {
            self.held = Some(true);
        }
        let operation = self.operation;
        let before = self.selection;
        let mut changes = Vec::with_capacity(transaction.changes.len());
        for change in transaction.changes {
            let (from, to) = (
                self.clip(min(change.from, change.to)),
                self.clip(max(change.from, change.to)),
            );
            let text = split_lines(&change.text);
            if from == to && text.len() == 1 && text[0].is_empty() {
                continue;
            }
            let (removed, end) = self.replace(from, to, text.clone());
            self.history.add(
                Step {
                    from,
                    to: end,
                    text: removed.clone(),
                },
                to,
                &transaction.origin,
                operation,
                time,
                before,
            );
            changes.push(TextChange {
                from,
                to,
                text,
                removed,
                origin: (!transaction.origin.is_empty()).then(|| transaction.origin.clone()),
            });
        }
        if let Some(selection) = transaction.selection {
            self.selection = Selection {
                anchor: self.clip(selection.anchor),
                head: self.clip(selection.head),
            };
        }
        if !changes.is_empty() || transaction.selection.is_some() {
            self.goal = None;
            self.version += 1;
        }
        changes
    }

    pub fn undo(&mut self) -> Vec<TextChange> {
        self.replay(true)
    }

    pub fn redo(&mut self) -> Vec<TextChange> {
        self.replay(false)
    }

    fn replay(&mut self, undo: bool) -> Vec<TextChange> {
        let Some(event) = self.history.take(undo) else {
            return Vec::new();
        };
        let origin = if undo { "undo" } else { "redo" };
        let current = self.selection;
        let mut inverse = Vec::new();
        let mut changes = Vec::new();
        for step in event.steps.iter().rev() {
            let (from, to) = (self.clip(step.from), self.clip(step.to));
            let (removed, end) = self.replace(from, to, step.text.clone());
            inverse.push(Step {
                from,
                to: end,
                text: removed.clone(),
            });
            changes.push(TextChange {
                from,
                to,
                text: step.text.clone(),
                removed,
                origin: Some(origin.into()),
            });
        }
        self.history.finish(undo, &event, inverse, current);
        self.selection = Selection {
            anchor: self.clip(event.selection.anchor),
            head: self.clip(event.selection.head),
        };
        self.goal = None;
        self.version += 1;
        changes
    }

    pub fn generation(&self) -> u64 {
        self.history.generation()
    }
    /// Ends the current undo step; returns the generation.
    pub fn close_history(&mut self) -> u64 {
        self.history.close()
    }
    /// Every generation undo or redo can return to.
    pub fn generations(&self) -> std::collections::BTreeSet<u64> {
        self.history.generations()
    }
    pub fn history_size(&self) -> (usize, usize) {
        self.history.size()
    }
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    pub fn anchor(&self, line: usize) -> Option<Anchor> {
        self.ids.get(line).copied().map(Anchor)
    }
    /// The line of `anchor`, or `None` once the line was deleted.
    pub fn anchor_line(&self, anchor: Anchor) -> Option<usize> {
        self.ids.iter().position(|id| *id == anchor.0)
    }

    /// Replaces the text with `next` as one minimal change. The common prefix stops at
    /// byte `preserve_after` so text inserted there keeps the following line's anchor.
    pub fn set_text(
        &mut self,
        next: &str,
        preserve_after: Option<usize>,
        origin: &str,
        time: f64,
    ) -> Vec<TextChange> {
        let old = self.text();
        if old == next {
            return Vec::new();
        }
        let prefix = old
            .chars()
            .zip(next.chars())
            .take_while(|(a, b)| a == b)
            .map(|(c, _)| c.len_utf8())
            .sum::<usize>()
            .min(preserve_after.unwrap_or(usize::MAX));
        let suffix = old[prefix..]
            .chars()
            .rev()
            .zip(next[prefix..].chars().rev())
            .take_while(|(a, b)| a == b)
            .map(|(c, _)| c.len_utf8())
            .sum::<usize>();
        let from = self.byte_position(prefix);
        let to = self.byte_position(old.len() - suffix);
        self.apply(
            Transaction::replace(from, to, &next[prefix..next.len() - suffix], origin),
            time,
        )
    }

    fn byte_position(&self, mut offset: usize) -> Position {
        for (line, text) in self.lines.iter().enumerate() {
            if offset <= text.len() {
                return Position {
                    line,
                    ch: ch_at(text, offset),
                };
            }
            offset -= text.len() + 1;
        }
        let line = self.last_line();
        Position {
            line,
            ch: utf16_len(&self.lines[line]),
        }
    }

    /// Collapses `from..to`. Folds inside it stay, to show again when it opens. A
    /// fold can't partly overlap another or start on a folded line.
    pub fn fold(&mut self, from: Position, to: Position) -> bool {
        use super::text::before;
        let (from, to) = (self.clip(from), self.clip(to));
        if !before(from, to)
            || self.folds.iter().any(|fold| {
                let within = !before(from, fold.from) && !before(fold.to, to);
                let crosses =
                    before(fold.from, from) && before(from, fold.to) && before(fold.to, to)
                        || before(from, fold.from) && before(fold.from, to) && before(to, fold.to);
                within || crosses || fold.from.line == from.line
            })
        {
            return false;
        }
        let at = self.folds.partition_point(|fold| before(fold.from, from));
        self.folds.insert(at, Fold { from, to });
        self.sync_folds();
        for end in [&mut self.selection.anchor, &mut self.selection.head] {
            if inside(*end, &Fold { from, to }) {
                *end = to;
            }
        }
        self.version += 1;
        true
    }

    /// Opens the folds that start on `line`.
    pub fn unfold(&mut self, line: usize) -> bool {
        let count = self.folds.len();
        self.folds.retain(|fold| fold.from.line != line);
        self.sync_folds();
        let changed = count != self.folds.len();
        if changed {
            self.version += 1;
        }
        changed
    }

    pub fn fold_starting(&self, line: usize) -> Option<&Fold> {
        self.folds.iter().find(|fold| fold.from.line == line)
    }

    /// The outermost fold around `at`.
    pub fn fold_at(&self, at: Position) -> Option<&Fold> {
        self.outer
            .iter()
            .find(|fold| !super::text::before(at, fold.from) && !super::text::before(fold.to, at))
    }

    /// Lines a fold hides.
    pub fn hidden(&self, line: usize) -> bool {
        self.outer
            .iter()
            .any(|fold| line > fold.from.line && line <= fold.to.line)
    }

    /// Rows on screen: lines minus the ones folds hide.
    pub fn row_count(&self) -> usize {
        self.lines.len() - self.hidden_lines()
    }

    fn hidden_lines(&self) -> usize {
        self.outer
            .iter()
            .map(|fold| fold.to.line - fold.from.line)
            .sum()
    }

    pub fn line_of_row(&self, row: usize) -> usize {
        let mut line = row;
        for fold in &self.outer {
            if fold.from.line < line {
                line += fold.to.line - fold.from.line;
            } else {
                break;
            }
        }
        line.min(self.last_line())
    }

    pub fn row_of_line(&self, line: usize) -> usize {
        let mut row = line;
        for fold in &self.outer {
            if fold.from.line >= line {
                break;
            }
            row -= (fold.to.line.min(line) - fold.from.line).min(row);
        }
        row
    }

    fn removed(&self, from: Position, to: Position) -> Vec<String> {
        let first = &self.lines[from.line];
        let start = byte_at(first, from.ch);
        if from.line == to.line {
            return vec![first[start..byte_at(first, to.ch).max(start)].to_owned()];
        }
        let last = &self.lines[to.line];
        let mut removed = vec![first[start..].to_owned()];
        removed.extend(self.lines[from.line + 1..to.line].iter().cloned());
        removed.push(last[..byte_at(last, to.ch)].to_owned());
        removed
    }

    /// Replaces `from..to`; returns the removed lines and the end of the inserted text.
    fn replace(
        &mut self,
        from: Position,
        to: Position,
        text: Vec<String>,
    ) -> (Vec<String>, Position) {
        let removed = self.removed(from, to);
        let count = text.len();
        let whole = from.ch == 0 && to.ch == 0 && text[count - 1].is_empty();
        let first = &self.lines[from.line];
        let prefix = first[..byte_at(first, from.ch)].to_owned();
        let last = &self.lines[to.line];
        let suffix = last[byte_at(last, to.ch)..].to_owned();
        let end = Position {
            line: from.line + count - 1,
            ch: if count == 1 {
                from.ch + utf16_len(&text[0])
            } else {
                utf16_len(&text[count - 1])
            },
        };
        let mut lines = text;
        lines[0].insert_str(0, &prefix);
        lines[count - 1].push_str(&suffix);
        let mut fresh = |n: usize| {
            let start = self.next_id;
            self.next_id += n as u64;
            start..self.next_id
        };
        let (first_id, last_id) = (self.ids[from.line], self.ids[to.line]);
        // The edited first line keeps its anchor, and a separate last line keeps its own when
        // the result still has a line for it; an edit of whole lines keeps only the last.
        let ids: Vec<u64> = if whole {
            fresh(count - 1).chain([last_id]).collect()
        } else if from.line == to.line {
            std::iter::once(first_id).chain(fresh(count - 1)).collect()
        } else if count == 1 {
            vec![first_id]
        } else {
            std::iter::once(first_id)
                .chain(fresh(count - 2))
                .chain([last_id])
                .collect()
        };
        self.lines.splice(from.line..=to.line, lines);
        self.ids.splice(from.line..=to.line, ids);
        let map = |at: Position| adjust(at, from, to, end);
        self.selection = Selection {
            anchor: map(self.selection.anchor),
            head: map(self.selection.head),
        };
        let mut folds = Vec::with_capacity(self.folds.len());
        for fold in self.folds.drain(..) {
            let moved = if !super::text::before(fold.from, to) {
                Some(Fold {
                    from: map(fold.from),
                    to: map(fold.to),
                })
            } else if !super::text::before(from, fold.to) {
                Some(fold)
            } else if !super::text::before(from, fold.from) && !super::text::before(fold.to, to) {
                Some(Fold {
                    from: fold.from,
                    to: map(fold.to),
                })
            } else {
                None
            };
            folds.extend(moved.filter(|fold| super::text::before(fold.from, fold.to)));
        }
        self.folds = folds;
        self.sync_folds();
        (removed, end)
    }
}

/// Where a position goes after `from..to` became text ending at `end`.
fn adjust(at: Position, from: Position, to: Position, end: Position) -> Position {
    if super::text::before(at, from) {
        at
    } else if !super::text::before(to, at) {
        end
    } else if at.line == to.line {
        Position {
            line: end.line,
            ch: end.ch + at.ch - to.ch,
        }
    } else {
        Position {
            line: at.line + end.line - to.line,
            ch: at.ch,
        }
    }
}

impl State {
    /// Drops folds an edit left crossing another or sharing a start line, and finds the
    /// outer ones.
    fn sync_folds(&mut self) {
        use super::text::before;
        self.folds.sort_by(|a, b| {
            (a.from.line, a.from.ch, b.to.line, b.to.ch).cmp(&(
                b.from.line,
                b.from.ch,
                a.to.line,
                a.to.ch,
            ))
        });
        let mut open: Vec<Fold> = Vec::new();
        self.outer.clear();
        let mut line = None;
        self.folds.retain(|fold| {
            if line == Some(fold.from.line) {
                return false;
            }
            while open.last().is_some_and(|last| !before(fold.from, last.to)) {
                open.pop();
            }
            if open.last().is_some_and(|last| before(last.to, fold.to)) {
                return false;
            }
            line = Some(fold.from.line);
            if open.is_empty() {
                self.outer.push(*fold);
            }
            open.push(*fold);
            true
        });
    }
}

fn inside(at: Position, fold: &Fold) -> bool {
    super::text::before(fold.from, at) && super::text::before(at, fold.to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: usize, ch: usize) -> Position {
        Position { line, ch }
    }

    #[test]
    fn changes_report_removed_text_and_map_the_cursor() {
        let mut state = State::new("one\ntwo\nthree");
        state.set_selection(Selection::cursor(at(1, 3)));
        let changes = state.apply(
            Transaction::replace(at(0, 1), at(1, 1), "X\nY", "+input"),
            0.0,
        );
        assert_eq!(state.text(), "oX\nYwo\nthree");
        assert_eq!(changes[0].removed, ["ne", "t"]);
        assert_eq!(changes[0].text, ["X", "Y"]);
        assert_eq!(state.cursor(), at(1, 3));
    }

    #[test]
    fn generations_follow_undo_and_redo() {
        let mut state = State::new("");
        assert_eq!(state.generation(), 1);
        state.apply(Transaction::replace(at(0, 0), at(0, 0), "a", "+input"), 0.0);
        state.apply(
            Transaction::replace(at(0, 1), at(0, 1), "b", "+input"),
            10.0,
        );
        assert_eq!(state.generation(), 3);
        assert_eq!(state.history_size(), (1, 0));
        state.close_history();
        state.apply(
            Transaction::replace(at(0, 2), at(0, 2), "c", "+input"),
            20.0,
        );
        assert_eq!(state.generation(), 4);
        let undone = state.undo();
        assert_eq!(undone[0].origin.as_deref(), Some("undo"));
        assert_eq!((state.text().as_str(), state.generation()), ("ab", 3));
        state.undo();
        assert_eq!((state.text().as_str(), state.generation()), ("", 1));
        state.redo();
        assert_eq!((state.text().as_str(), state.generation()), ("ab", 3));
        state.redo();
        assert_eq!((state.text().as_str(), state.generation()), ("abc", 4));
        assert_eq!(
            state.generations().into_iter().collect::<Vec<_>>(),
            [1, 3, 4]
        );
    }

    #[test]
    fn typing_after_the_delay_starts_a_new_undo_step() {
        let mut state = State::new("");
        state.apply(Transaction::replace(at(0, 0), at(0, 0), "a", "+input"), 0.0);
        state.apply(
            Transaction::replace(at(0, 1), at(0, 1), "b", "+input"),
            2000.0,
        );
        state.undo();
        assert_eq!(state.text(), "a");
    }

    #[test]
    fn anchors_follow_their_lines() {
        let mut state = State::new("#check A\nbody\n#check B");
        let next = state.anchor(2).unwrap();
        let first = state.anchor(0).unwrap();
        // A response inserted before the next check keeps the check's anchor.
        state.set_text(
            "#check A\nbody\n#response\nok\n#check B",
            Some(14),
            "+services-response",
            0.0,
        );
        assert_eq!(state.anchor_line(next), Some(4));
        assert_eq!(state.anchor_line(first), Some(0));
        // Splitting a line keeps the anchor on its first part.
        state.apply(
            Transaction::replace(at(0, 6), at(0, 6), "\n", "+input"),
            0.0,
        );
        assert_eq!(state.anchor_line(first), Some(0));
        // Deleting a line drops its anchor.
        let body = state.anchor(2).unwrap();
        state.apply(Transaction::replace(at(2, 0), at(3, 0), "", "+delete"), 0.0);
        assert_eq!(state.anchor_line(body), None);
        assert_eq!(state.anchor_line(next), Some(4));
    }

    #[test]
    fn folds_hide_lines_and_follow_edits() {
        let mut state = State::new("#response\na\nb\nc\nnext");
        assert!(state.fold(at(0, 9), at(3, 1)));
        assert_eq!(
            (
                state.row_count(),
                state.line_of_row(1),
                state.row_of_line(4)
            ),
            (2, 4, 1)
        );
        assert!(state.hidden(2) && !state.hidden(4));
        state.apply(
            Transaction::replace(at(0, 0), at(0, 0), "x\n", "+input"),
            0.0,
        );
        assert_eq!(state.folds()[0].from, at(1, 9));
        state.apply(
            Transaction::replace(at(2, 0), at(2, 1), "A\nA", "+services-response"),
            0.0,
        );
        assert_eq!(state.folds()[0].to, at(5, 1));
        state.set_selection(Selection::cursor(at(3, 0)));
        assert!(state.folds().is_empty());
    }

    #[test]
    fn nested_folds_return_when_the_outer_one_opens() {
        let mut state = State::new(
            "#check
#payload
a
b
#response
c
next",
        );
        assert!(state.fold(at(1, 5), at(3, 1)));
        assert!(state.fold(at(0, 6), at(5, 1)));
        assert_eq!(state.folds().len(), 2);
        assert_eq!((state.row_count(), state.line_of_row(1)), (2, 6));
        assert!(
            !state.fold(at(2, 0), at(6, 2)),
            "a fold can't cross another"
        );
        assert!(state.unfold(0));
        assert_eq!(
            state.folds(),
            &[Fold {
                from: at(1, 5),
                to: at(3, 1)
            }]
        );
        assert_eq!(
            (
                state.row_count(),
                state.line_of_row(2),
                state.row_of_line(4)
            ),
            (5, 4, 2)
        );
    }
}
