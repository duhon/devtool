//! Editing and motion commands. Each command is one undo step.
use super::state::{Change, Selection, State, Transaction};
use super::text::{
    CharClass, INDENT_UNIT, byte_at, byte_at_column, ch_at, class, column, indentation,
    leading_whitespace, utf16_len,
};
use crate::editor::text::{Position, TextChange};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Char,
    Group,
    /// To the start or end of the visual line.
    Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Indent {
    /// Like the previous line.
    Prev,
    Add,
    Subtract,
}

impl State {
    /// Replaces the selection with `text` and puts the cursor after it.
    pub fn insert(&mut self, text: &str, origin: &str, time: f64) -> Vec<TextChange> {
        let selection = self.selection();
        self.apply(
            Transaction::replace(selection.from(), selection.to(), text, origin),
            time,
        )
    }

    pub fn newline_and_indent(&mut self, time: f64) -> Vec<TextChange> {
        let selection = self.selection();
        let (from, to) = (selection.from(), selection.to());
        let first = self.line(from.line);
        let before = &first[..byte_at(first, from.ch)];
        let last = self.line(to.line);
        let rest = &last[byte_at(last, to.ch)..];
        let current = &rest[..leading_whitespace(rest)];
        let indent = " ".repeat(indentation(before));
        let line = from.line + 1;
        let mut changes = vec![Change {
            from,
            to,
            text: "\n".into(),
        }];
        if indent != current {
            changes.push(Change {
                from: Position { line, ch: 0 },
                to: Position {
                    line,
                    ch: utf16_len(current),
                },
                text: indent.clone(),
            });
        }
        self.apply(
            Transaction {
                changes,
                origin: "+input".into(),
                selection: Some(Selection::cursor(Position {
                    line,
                    ch: utf16_len(&indent),
                })),
                join: false,
            },
            time,
        )
    }

    /// Tab: indents the selected lines, or inserts two spaces.
    pub fn tab(&mut self, time: f64) -> Vec<TextChange> {
        if self.selection().empty() {
            self.insert(&" ".repeat(INDENT_UNIT), "+input", time)
        } else {
            self.indent_selection(Indent::Add, time)
        }
    }

    /// Re-indents the lines the selection touches (a selection ending at column 0 leaves that
    /// line out); with a selection, blank lines become empty.
    pub fn indent_selection(&mut self, how: Indent, time: f64) -> Vec<TextChange> {
        let selection = self.selection();
        let (from, to) = (selection.from(), selection.to());
        let last = if to.line > from.line && to.ch == 0 {
            to.line - 1
        } else {
            to.line
        };
        let mut changes = Vec::new();
        // `Prev` follows the previous line as re-indented here, so a selection lines up.
        let mut previous = from
            .line
            .checked_sub(1)
            .map_or(0, |prev| indentation(self.line(prev)));
        for line in from.line..=last {
            let text = self.line(line);
            let current = &text[..leading_whitespace(text)];
            let blank = current.len() == text.len();
            let width = if !selection.empty() && blank {
                0
            } else {
                match how {
                    Indent::Prev => previous,
                    Indent::Add => indentation(text) + INDENT_UNIT,
                    Indent::Subtract => indentation(text).saturating_sub(INDENT_UNIT),
                }
            };
            if !blank {
                previous = width;
            }
            let indent = " ".repeat(width);
            if indent != current {
                changes.push(Change {
                    from: Position { line, ch: 0 },
                    to: Position {
                        line,
                        ch: utf16_len(current),
                    },
                    text: indent,
                });
            }
        }
        let indented = |text: &str| ch_at(text, leading_whitespace(text));
        let before = indented(self.line(from.line));
        let transaction = Transaction {
            changes,
            origin: "+input".into(),
            ..Transaction::default()
        };
        let changes = self.apply(transaction, time);
        // Changes at column 0 push positions there past the new indentation.
        let mapped = self.selection();
        let start = Position {
            line: from.line,
            ch: 0,
        };
        let fixed = match (selection.empty(), from.ch) {
            (true, ch) if ch <= before => Selection::cursor(Position {
                line: from.line,
                ch: indented(self.line(from.line)),
            }),
            (false, 0) if selection.anchor == from => Selection {
                anchor: start,
                ..mapped
            },
            (false, 0) => Selection {
                head: start,
                ..mapped
            },
            _ => mapped,
        };
        self.set_selection(fixed);
        changes
    }

    /// Deletes the selection, or from the cursor by `unit` in direction `dir`.
    pub fn delete(&mut self, dir: i8, unit: Unit, time: f64) -> Vec<TextChange> {
        let selection = self.selection();
        let (from, to) = if selection.empty() {
            let head = selection.head;
            let target = match unit {
                Unit::Line if dir < 0 => self.visual_line_start(head.line),
                Unit::Line => self.visual_line_end(head.line),
                _ => self.find_h(head, dir, unit),
            };
            if let Some(fold) = self.fold_at(target).copied()
                && fold.from != target
                && fold.to != target
            {
                self.unfold(fold.from.line);
            }
            (
                super::text::min(head, target),
                super::text::max(head, target),
            )
        } else {
            (selection.from(), selection.to())
        };
        self.apply(
            Transaction {
                changes: vec![Change {
                    from,
                    to,
                    text: String::new(),
                }],
                origin: "+delete".into(),
                selection: Some(Selection::cursor(from)),
                join: false,
            },
            time,
        )
    }

    /// Deletes the lines the selection touches.
    pub fn delete_lines(&mut self, time: f64) -> Vec<TextChange> {
        let selection = self.selection();
        let from = Position {
            line: selection.from().line,
            ch: 0,
        };
        let to = self.clip(Position {
            line: selection.to().line + 1,
            ch: 0,
        });
        let to = if to.line == selection.to().line {
            Position {
                line: to.line,
                ch: utf16_len(self.line(to.line)),
            }
        } else {
            to
        };
        self.apply(
            Transaction {
                changes: vec![Change {
                    from,
                    to,
                    text: String::new(),
                }],
                origin: "+delete".into(),
                selection: Some(Selection::cursor(from)),
                join: false,
            },
            time,
        )
    }

    /// Arrow Left/Right (with Alt/Ctrl for groups, Cmd for line ends on macOS).
    pub fn move_h(&mut self, dir: i8, unit: Unit, extend: bool) -> bool {
        let selection = self.selection();
        let head = if !extend && !selection.empty() && unit == Unit::Char {
            if dir < 0 {
                selection.from()
            } else {
                selection.to()
            }
        } else {
            match unit {
                Unit::Line if dir < 0 => Position {
                    line: self.visual_line_start(selection.head.line).line,
                    ch: 0,
                },
                Unit::Line => self.visual_line_end(selection.head.line),
                _ => self.find_h(selection.head, dir, unit),
            }
        };
        self.move_to(head, extend)
    }

    /// Arrow Up/Down and PageUp/PageDown: `rows` visual rows, keeping the goal column.
    pub fn move_v(&mut self, dir: i8, rows: usize, extend: bool) -> bool {
        let head = self.selection().head;
        let goal = self.goal.unwrap_or_else(|| self.display_column(head));
        let row = self.row_of_line(head.line);
        let last = self.row_count() - 1;
        let target = if dir < 0 && row == 0 {
            Position { line: 0, ch: 0 }
        } else if dir > 0 && row == last {
            let line = self.last_line();
            Position {
                line,
                ch: utf16_len(self.line(line)),
            }
        } else {
            let row = if dir < 0 {
                row.saturating_sub(rows)
            } else {
                (row + rows).min(last)
            };
            let line = self.line_of_row(row);
            let text = self.line(line);
            let at = Position {
                line,
                ch: ch_at(text, byte_at_column(text, goal)),
            };
            match self.fold_starting(line) {
                Some(fold) if super::text::before(fold.from, at) => fold.to,
                _ => at,
            }
        };
        let moved = self.move_to(target, extend);
        self.goal = Some(goal);
        moved
    }

    /// Home: the first non-space character, or the line start when already there.
    pub fn home(&mut self, extend: bool) -> bool {
        let head = self.selection().head;
        let start = self.visual_line_start(head.line);
        let text = self.line(start.line);
        let first = text
            .char_indices()
            .find(|(_, c)| !c.is_whitespace())
            .map_or(0, |(index, _)| ch_at(text, index));
        let in_space = head.line == start.line && head.ch <= first && head.ch > 0;
        self.move_to(
            Position {
                line: start.line,
                ch: if in_space { 0 } else { first },
            },
            extend,
        )
    }

    pub fn end(&mut self, extend: bool) -> bool {
        let end = self.visual_line_end(self.selection().head.line);
        self.move_to(end, extend)
    }

    pub fn doc_start(&mut self, extend: bool) -> bool {
        self.move_to(Position { line: 0, ch: 0 }, extend)
    }

    pub fn doc_end(&mut self, extend: bool) -> bool {
        let line = self.last_line();
        let end = Position {
            line,
            ch: utf16_len(self.line(line)),
        };
        self.move_to(end, extend)
    }

    pub fn select_all(&mut self) -> bool {
        let line = self.last_line();
        let end = Position {
            line,
            ch: utf16_len(self.line(line)),
        };
        self.set_selection(Selection {
            anchor: Position { line: 0, ch: 0 },
            head: end,
        })
    }

    /// The word (or run of punctuation or space) around `at`, for double click.
    pub fn word_at(&self, at: Position) -> Selection {
        let at = self.clip(at);
        let text = self.line(at.line);
        let byte = byte_at(text, at.ch);
        let word = |c: char| class(c) == CharClass::Word;
        let pivot = match (
            text[..byte].chars().next_back(),
            text[byte..].chars().next(),
        ) {
            (_, Some(c)) if word(c) => byte,
            (Some(c), _) if word(c) => byte - c.len_utf8(),
            (_, Some(_)) => byte,
            (Some(c), None) => byte - c.len_utf8(),
            (None, None) => return Selection::cursor(at),
        };
        let kind = text[pivot..].chars().next().map(class);
        let same = |c: char| Some(class(c)) == kind;
        let start = text[..pivot]
            .char_indices()
            .rev()
            .take_while(|&(_, c)| same(c))
            .last()
            .map_or(pivot, |(index, _)| index);
        let end = text[pivot..]
            .find(|c| !same(c))
            .map_or(text.len(), |n| pivot + n);
        let position = |byte| Position {
            line: at.line,
            ch: ch_at(text, byte),
        };
        Selection {
            anchor: position(start),
            head: position(end),
        }
    }

    fn move_to(&mut self, head: Position, extend: bool) -> bool {
        let anchor = if extend {
            self.selection().anchor
        } else {
            head
        };
        self.set_selection(Selection { anchor, head })
    }

    fn display_column(&self, at: Position) -> usize {
        let text = self.line(at.line);
        column(text, byte_at(text, at.ch))
    }

    /// The start of the row `line` is on; a fold can put several lines on one row.
    fn visual_line_start(&self, mut line: usize) -> Position {
        while let Some(fold) = self
            .outer_folds()
            .iter()
            .find(|fold| fold.to.line == line && fold.from.line < line)
        {
            line = fold.from.line;
        }
        Position { line, ch: 0 }
    }

    fn visual_line_end(&self, mut line: usize) -> Position {
        while let Some(fold) = self.fold_starting(line).filter(|fold| fold.to.line > line) {
            line = fold.to.line;
        }
        Position {
            line,
            ch: utf16_len(self.line(line)),
        }
    }

    /// The position one "char" or "group" step from `at` in direction `dir`. A group is the
    /// spaces before the next run of word or punctuation characters on the line; a line end
    /// is a step of its own.
    fn find_h(&self, at: Position, dir: i8, unit: Unit) -> Position {
        /// Where a group ends, from `(offset past the char, char)` in the direction of travel.
        fn group(chars: impl Iterator<Item = (usize, char)>, edge: usize) -> usize {
            let mut chars = chars.skip_while(|&(_, c)| class(c) == CharClass::Space);
            let Some((mut end, first)) = chars.next() else {
                return edge;
            };
            for (past, _) in chars.take_while(|&(_, c)| class(c) == class(first)) {
                end = past;
            }
            end
        }
        let text = self.line(at.line);
        let byte = byte_at(text, at.ch);
        if dir < 0 && byte == 0 {
            return match at.line.checked_sub(1) {
                Some(line) => Position {
                    line,
                    ch: utf16_len(self.line(line)),
                },
                None => at,
            };
        }
        if dir > 0 && byte == text.len() {
            return if at.line < self.last_line() {
                Position {
                    line: at.line + 1,
                    ch: 0,
                }
            } else {
                at
            };
        }
        let (before, after) = (&text[..byte], &text[byte..]);
        let end = match unit {
            Unit::Char if dir < 0 => byte - before.chars().next_back().map_or(0, char::len_utf8),
            Unit::Char => byte + after.chars().next().map_or(0, char::len_utf8),
            Unit::Group if dir < 0 => group(before.char_indices().rev(), 0),
            Unit::Group => group(
                after
                    .char_indices()
                    .map(|(index, c)| (byte + index + c.len_utf8(), c)),
                text.len(),
            ),
            Unit::Line if dir < 0 => 0,
            Unit::Line => text.len(),
        };
        Position {
            line: at.line,
            ch: ch_at(text, end),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: usize, ch: usize) -> Position {
        Position { line, ch }
    }

    fn state(text: &str, cursor: Position) -> State {
        let mut state = State::new(text);
        state.set_selection(Selection::cursor(cursor));
        state
    }

    #[test]
    fn newline_keeps_the_previous_indentation_in_one_undo_step() {
        let mut state = state("  {\"a\": 1}", at(0, 3));
        state.newline_and_indent(0.0);
        assert_eq!(state.text(), "  {\n  \"a\": 1}");
        assert_eq!(state.cursor(), at(1, 2));
        state.insert("x", "+input", 10.0);
        state.undo();
        assert_eq!(state.text(), "  {\"a\": 1}");
    }

    #[test]
    fn tab_inserts_spaces_or_indents_selected_lines() {
        let mut state = state("a\n\nb\nc", at(0, 1));
        state.tab(0.0);
        assert_eq!(state.text(), "a  \n\nb\nc");
        state.set_selection(Selection {
            anchor: at(0, 0),
            head: at(3, 0),
        });
        state.tab(0.0);
        assert_eq!(state.text(), "  a  \n\n  b\nc");
        assert_eq!(
            state.selection(),
            Selection {
                anchor: at(0, 0),
                head: at(3, 0)
            }
        );
        state.indent_selection(Indent::Subtract, 0.0);
        assert_eq!(state.text(), "a  \n\nb\nc");
    }

    #[test]
    fn vertical_moves_keep_the_column_and_skip_folds() {
        let mut state = state("abcdef\nx\n#response\n1\n2\nabcdef", at(0, 5));
        state.move_v(1, 1, false);
        assert_eq!(state.cursor(), at(1, 1));
        state.move_v(1, 1, false);
        assert_eq!(state.cursor(), at(2, 5));
        state.fold(at(2, 9), at(4, 1));
        state.move_v(1, 1, false);
        assert_eq!(state.cursor(), at(5, 5));
        state.move_v(1, 1, true);
        assert_eq!(
            state.selection(),
            Selection {
                anchor: at(5, 5),
                head: at(5, 6)
            }
        );
        state.move_v(-1, 1, false);
        assert_eq!(state.cursor(), at(2, 5));
    }

    #[test]
    fn home_toggles_between_indentation_and_line_start() {
        let mut state = state("    abc", at(0, 6));
        state.home(false);
        assert_eq!(state.cursor(), at(0, 4));
        state.move_h(-1, Unit::Char, false);
        state.home(false);
        assert_eq!(state.cursor(), at(0, 0));
        state.end(false);
        assert_eq!(state.cursor(), at(0, 7));
    }

    #[test]
    fn double_click_selects_a_word() {
        let state = state("GET /users/{{id}}", at(0, 0));
        assert_eq!(
            state.word_at(at(0, 7)),
            Selection {
                anchor: at(0, 5),
                head: at(0, 10)
            }
        );
        assert_eq!(
            state.word_at(at(0, 10)),
            Selection {
                anchor: at(0, 5),
                head: at(0, 10)
            }
        );
    }

    #[test]
    fn double_click_selects_a_run_of_spaces_or_punctuation() {
        let state = state("a  b..\n", at(0, 0));
        assert_eq!(
            state.word_at(at(0, 1)),
            Selection {
                anchor: at(0, 0),
                head: at(0, 1)
            }
        );
        assert_eq!(
            state.word_at(at(0, 2)),
            Selection {
                anchor: at(0, 1),
                head: at(0, 3)
            }
        );
        assert_eq!(
            state.word_at(at(0, 6)),
            Selection {
                anchor: at(0, 4),
                head: at(0, 6)
            }
        );
        assert_eq!(state.word_at(at(1, 0)), Selection::cursor(at(1, 0)));
    }

    #[test]
    fn group_motion_skips_spaces_then_one_run() {
        let mut doc = state("foo.bar  baz\n  x", at(0, 0));
        let mut stops = |dir, n| {
            (0..n)
                .map(|_| {
                    doc.move_h(dir, Unit::Group, false);
                    doc.cursor()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            stops(1, 6),
            [at(0, 3), at(0, 4), at(0, 7), at(0, 12), at(1, 0), at(1, 3)]
        );
        assert_eq!(
            stops(-1, 7),
            [
                at(1, 2),
                at(1, 0),
                at(0, 12),
                at(0, 9),
                at(0, 4),
                at(0, 3),
                at(0, 0)
            ]
        );
        assert_eq!(stops(-1, 1), [at(0, 0)]);
        let mut doc = state("a\u{1F600}b  \nc", at(0, 1));
        doc.move_h(1, Unit::Char, false);
        assert_eq!(doc.cursor(), at(0, 3));
        doc.move_h(1, Unit::Group, false);
        doc.move_h(1, Unit::Group, false);
        assert_eq!(doc.cursor(), at(0, 6));
    }

    #[test]
    fn group_delete_removes_a_run_and_crosses_lines() {
        let mut doc = state("foo.bar  baz", at(0, 12));
        doc.delete(-1, Unit::Group, 0.0);
        assert_eq!(doc.text(), "foo.bar  ");
        doc.delete(-1, Unit::Char, 0.0);
        doc.delete(-1, Unit::Group, 0.0);
        assert_eq!((doc.text(), doc.cursor()), ("foo.".to_owned(), at(0, 4)));
        let mut doc = state("ab\n  cd", at(1, 0));
        doc.delete(-1, Unit::Group, 0.0);
        doc.delete(1, Unit::Group, 0.0);
        assert_eq!((doc.text(), doc.cursor()), ("ab".to_owned(), at(0, 2)));
    }

    #[test]
    fn indenting_lines_moves_a_cursor_out_of_the_indentation_in_one_undo_step() {
        let mut doc = state("  a\n b\n ", at(1, 0));
        doc.indent_selection(Indent::Prev, 0.0);
        assert_eq!(
            (doc.text(), doc.cursor()),
            ("  a\n  b\n ".to_owned(), at(1, 2))
        );
        doc.indent_selection(Indent::Subtract, 0.0);
        assert_eq!(
            (doc.text(), doc.cursor()),
            ("  a\nb\n ".to_owned(), at(1, 0))
        );
        doc.set_selection(Selection::cursor(at(2, 1)));
        doc.indent_selection(Indent::Add, 0.0);
        assert_eq!(
            (doc.text(), doc.cursor()),
            ("  a\nb\n   ".to_owned(), at(2, 3))
        );
        doc.set_selection(Selection {
            anchor: at(2, 2),
            head: at(0, 1),
        });
        doc.indent_selection(Indent::Add, 10_000.0);
        assert_eq!(doc.text(), "    a\n  b\n");
        assert_eq!(
            doc.selection(),
            Selection {
                anchor: at(2, 0),
                head: at(0, 4)
            }
        );
        doc.undo();
        assert_eq!(doc.text(), "  a\nb\n   ");
    }
}
