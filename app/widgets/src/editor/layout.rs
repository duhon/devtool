//! Where text is on screen: fixed-height rows, monospace columns, and folds shown inline
//! as a widget joining the lines around them; lines never wrap.
use super::model::Metrics;
use super::state::{Fold, Selection, State};
use super::text::{TAB_SIZE, before, byte_at, max, min, utf16_len};
use crate::editor::text::Position;
use std::ops::Range;

/// Space above the first line and below the last one.
pub const PADDING_TOP: f64 = 4.0;
pub const PADDING_BOTTOM: f64 = 4.0;
/// Space left and right of a line's text.
pub const LINE_PADDING: f64 = 4.0;
/// Rows rendered beyond the visible ones, so scrolling rarely shows blank space.
pub const MARGIN: usize = 10;
/// Rows rendered before the scroller is measured.
const UNMEASURED_ROWS: usize = 50;
/// Columns the widget of a fold takes.
pub const WIDGET_COLUMNS: usize = 1;
const NUMBER_PADDING: f64 = 8.0;
const NUMBER_MIN_WIDTH: f64 = 20.0;
pub const FOLD_GUTTER: f64 = 20.0;

/// A run of one line's text on a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub line: usize,
    pub from: usize,
    pub to: usize,
    /// The display column the run starts at.
    pub column: usize,
}

/// A row on screen: a line and the lines its folds join to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub pieces: Vec<Piece>,
    /// The display column of each fold widget and its fold.
    pub widgets: Vec<(usize, Fold)>,
    pub columns: usize,
}

/// The display column after `text` when it starts at `column`; tabs stop every [`TAB_SIZE`].
pub fn advance(mut column: usize, text: &str) -> usize {
    for c in text.chars() {
        column += if c == '\t' {
            TAB_SIZE - column % TAB_SIZE
        } else {
            1
        };
    }
    column
}

fn slice(text: &str, from: usize, to: usize) -> &str {
    &text[byte_at(text, from)..byte_at(text, to)]
}

impl Row {
    pub fn new(state: &State, line: usize) -> Self {
        let mut pieces = Vec::new();
        let mut widgets = Vec::new();
        let mut at = Position { line, ch: 0 };
        let mut column = 0;
        loop {
            let text = state.line(at.line);
            let fold = state
                .outer_folds()
                .iter()
                .find(|fold| fold.from.line == at.line && fold.from.ch >= at.ch);
            let end = fold.map_or_else(|| utf16_len(text), |fold| fold.from.ch);
            let start = column;
            column = advance(column, slice(text, at.ch, end));
            pieces.push(Piece {
                line: at.line,
                from: at.ch,
                to: end,
                column: start,
            });
            let Some(fold) = fold else {
                break;
            };
            widgets.push((column, *fold));
            column += WIDGET_COLUMNS;
            at = fold.to;
        }
        Self {
            pieces,
            widgets,
            columns: column,
        }
    }

    pub fn last_line(&self) -> usize {
        self.pieces.last().map_or(0, |piece| piece.line)
    }

    /// The display column of `at`, when this row shows it.
    pub fn column(&self, state: &State, at: Position) -> Option<usize> {
        let piece = self
            .pieces
            .iter()
            .find(|piece| piece.line == at.line && piece.from <= at.ch && at.ch <= piece.to)?;
        Some(advance(
            piece.column,
            slice(state.line(at.line), piece.from, at.ch),
        ))
    }

    /// The position nearest to display column `column`.
    pub fn position(&self, state: &State, column: f64) -> Position {
        for (index, piece) in self.pieces.iter().enumerate() {
            let text = state.line(piece.line);
            let mut at = piece.column;
            let mut ch = piece.from;
            for c in slice(text, piece.from, piece.to).chars() {
                let width = if c == '\t' {
                    TAB_SIZE - at % TAB_SIZE
                } else {
                    1
                };
                if column < at as f64 + width as f64 / 2.0 {
                    return Position {
                        line: piece.line,
                        ch,
                    };
                }
                at += width;
                ch += c.len_utf16();
            }
            let Some((widget, fold)) = self.widgets.get(index) else {
                break;
            };
            if column < *widget as f64 + WIDGET_COLUMNS as f64 / 2.0 {
                return fold.from;
            }
            if column < (*widget + WIDGET_COLUMNS) as f64 {
                return fold.to;
            }
        }
        let last = self.pieces.last().expect("a row has a piece");
        Position {
            line: last.line,
            ch: last.to,
        }
    }
}

/// A rectangle in the coordinates of the text area: x from the left of the text area,
/// y from the top of the document.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

pub fn row_top(row: usize, line_height: f64) -> f64 {
    PADDING_TOP + row as f64 * line_height
}

pub fn x(column: usize, char_width: f64) -> f64 {
    LINE_PADDING + column as f64 * char_width
}

pub fn number_width(line_count: usize, char_width: f64) -> f64 {
    let digits = line_count.max(1).to_string().len();
    (digits as f64 * char_width).max(NUMBER_MIN_WIDTH) + NUMBER_PADDING
}

pub fn gutter_width(line_count: usize, char_width: f64, document: bool) -> f64 {
    number_width(line_count, char_width) + if document { FOLD_GUTTER } else { 0.0 }
}

/// The rows to render: the visible ones and a margin around them.
pub fn rendered_rows(state: &State, metrics: Option<Metrics>) -> Range<usize> {
    let rows = state.row_count();
    let Some(metrics) =
        metrics.filter(|metrics| metrics.line_height > 0.0 && metrics.client_height > 0.0)
    else {
        return 0..rows.min(UNMEASURED_ROWS);
    };
    let first = ((metrics.scroll_top - PADDING_TOP).max(0.0) / metrics.line_height) as usize;
    let last = ((metrics.scroll_top + metrics.client_height - PADDING_TOP) / metrics.line_height)
        .ceil() as usize;
    first.saturating_sub(MARGIN).min(rows)..(last + MARGIN).min(rows)
}

/// The lines `rows` show, as the facade's `getViewport` reports them.
pub fn viewport(state: &State, rows: &Range<usize>) -> (usize, usize) {
    if rows.is_empty() {
        let line = state.line_of_row(rows.start);
        return (line, line);
    }
    (
        state.line_of_row(rows.start),
        Row::new(state, state.line_of_row(rows.end - 1)).last_line() + 1,
    )
}

pub fn document_height(state: &State, metrics: &Metrics, document: bool) -> f64 {
    let bottom = if document {
        (metrics.client_height - metrics.line_height).max(0.0)
    } else {
        PADDING_BOTTOM
    };
    PADDING_TOP + state.row_count() as f64 * metrics.line_height + bottom
}

/// The position under a point of the text area; past the end is the end of the document.
pub fn position_at(state: &State, metrics: &Metrics, x: f64, y: f64) -> Position {
    let row = ((y - PADDING_TOP) / metrics.line_height).floor();
    if row >= state.row_count() as f64 {
        let line = state.last_line();
        return Position {
            line,
            ch: utf16_len(state.line(line)),
        };
    }
    let line = state.line_of_row(row.max(0.0) as usize);
    let column = (x - LINE_PADDING) / metrics.char_width.max(1.0);
    Row::new(state, line).position(state, column)
}

/// Where `at` is, a caret-wide rectangle, when no fold hides it.
pub fn caret(state: &State, metrics: &Metrics, at: Position) -> Option<Rect> {
    if state.hidden(at.line) {
        let fold = state.fold_at(at)?;
        if fold.to != at {
            return None;
        }
    }
    let row = state.row_of_line(at.line);
    let line = state.line_of_row(row);
    let column = Row::new(state, line).column(state, at)?;
    let top = row_top(row, metrics.line_height);
    let left = x(column, metrics.char_width);
    Some(Rect {
        left,
        top,
        right: left,
        bottom: top + metrics.line_height,
    })
}

/// The selected text on `rows`: one
/// rectangle per selected run of text, joined on a row.
pub fn selection_rects(
    state: &State,
    metrics: &Metrics,
    selection: Selection,
    rows: Range<usize>,
) -> Vec<Rect> {
    let (from, to) = (selection.from(), selection.to());
    let mut rects = Vec::new();
    if from == to {
        return rects;
    }
    for row in rows {
        let line = state.line_of_row(row);
        if line > to.line {
            break;
        }
        let layout = Row::new(state, line);
        if layout.last_line() < from.line {
            continue;
        }
        let top = row_top(row, metrics.line_height).round();
        let bottom = row_top(row + 1, metrics.line_height).round();
        let mut row_rects: Vec<Rect> = Vec::new();
        let mut add = |left: f64, right: f64| {
            if right <= left {
                return;
            }
            match row_rects.last_mut() {
                Some(last) if left <= last.right => last.right = last.right.max(right),
                _ => row_rects.push(Rect {
                    left,
                    top,
                    right,
                    bottom,
                }),
            }
        };
        for (index, piece) in layout.pieces.iter().enumerate() {
            let start = max(
                from,
                Position {
                    line: piece.line,
                    ch: piece.from,
                },
            );
            let end = min(
                to,
                Position {
                    line: piece.line,
                    ch: piece.to,
                },
            );
            if before(start, end) && start.line == piece.line && end.line == piece.line {
                let text = state.line(piece.line);
                let left = advance(piece.column, slice(text, piece.from, start.ch));
                let right = advance(left, slice(text, start.ch, end.ch));
                add(x(left, metrics.char_width), x(right, metrics.char_width));
            }
            if let Some((widget, fold)) = layout.widgets.get(index)
                && !before(fold.from, from)
                && !before(to, fold.to)
            {
                add(
                    x(*widget, metrics.char_width),
                    x(widget + WIDGET_COLUMNS, metrics.char_width),
                );
            }
        }
        rects.extend(row_rects);
    }
    rects
}

/// The outer contour of `rects` as an SVG path, without the edges rectangles share.
pub fn outline(rects: &[Rect]) -> String {
    type Groups = Vec<(f64, Vec<(f64, i32)>)>;
    fn edge(groups: &mut Groups, coordinate: f64, from: f64, to: f64, sign: i32) {
        let index = match groups.iter().position(|(at, _)| *at == coordinate) {
            Some(index) => index,
            None => {
                groups.push((coordinate, Vec::new()));
                groups.len() - 1
            }
        };
        groups[index].1.extend([(from, sign), (to, -sign)]);
    }
    let mut horizontal = Groups::new();
    let mut vertical = Groups::new();
    for rect in rects {
        edge(&mut horizontal, rect.top, rect.left, rect.right, 1);
        edge(&mut horizontal, rect.bottom, rect.left, rect.right, -1);
        edge(&mut vertical, rect.left, rect.top, rect.bottom, 1);
        edge(&mut vertical, rect.right, rect.top, rect.bottom, -1);
    }
    let mut path = String::new();
    for (groups, horizontal) in [(horizontal, true), (vertical, false)] {
        for (coordinate, mut events) in groups {
            events.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut count = 0;
            let mut previous = events[0].0;
            let mut index = 0;
            while index < events.len() {
                let position = events[index].0;
                if count != 0 && position > previous {
                    let inset = coordinate + if count > 0 { 0.5 } else { -0.5 };
                    path.push_str(&if horizontal {
                        format!("M{previous} {inset}H{position}")
                    } else {
                        format!("M{inset} {previous}V{position}")
                    });
                }
                while index < events.len() && events[index].0 == position {
                    count += events[index].1;
                    index += 1;
                }
                previous = position;
            }
        }
    }
    path
}

/// The scroll offsets that bring `rect` into view.
pub fn scroll_into_view(
    metrics: &Metrics,
    document_height: f64,
    gutter: f64,
    mut rect: Rect,
) -> (Option<f64>, Option<f64>) {
    let snap = metrics.line_height;
    rect.top = rect.top.max(0.0);
    let screen = metrics.client_height;
    if rect.bottom - rect.top > screen {
        rect.bottom = rect.top + screen;
    }
    let at_top = rect.top < snap;
    let at_bottom = rect.bottom > document_height - snap;
    let mut top = None;
    if rect.top < metrics.scroll_top {
        top = Some(if at_top { 0.0 } else { rect.top });
    } else if rect.bottom > metrics.scroll_top + screen {
        let next = rect.top.min(
            if at_bottom {
                document_height
            } else {
                rect.bottom
            } - screen,
        );
        if next != metrics.scroll_top {
            top = Some(next);
        }
    }
    let width = metrics.client_width - gutter;
    let too_wide = rect.right - rect.left > width;
    if too_wide {
        rect.right = rect.left + width;
    }
    let margin = if too_wide { 0.0 } else { 10.0 };
    let left = if rect.left < 10.0 {
        Some(0.0)
    } else if rect.left < metrics.scroll_left {
        Some((rect.left - margin).max(0.0))
    } else if rect.right > width + metrics.scroll_left - 3.0 {
        Some(rect.right + margin - width)
    } else {
        None
    };
    (top, left)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: usize, ch: usize) -> Position {
        Position { line, ch }
    }

    fn metrics() -> Metrics {
        Metrics {
            scroll_top: 0.0,
            scroll_left: 0.0,
            client_height: 100.0,
            client_width: 200.0,
            line_height: 10.0,
            char_width: 5.0,
            gutter_width: 30.0,
        }
    }

    #[test]
    fn folds_join_lines_on_one_row() {
        let mut state = State::new("ab{\n  x\n}cd\ne");
        state.fold(at(0, 3), at(2, 0));
        let row = Row::new(&state, 0);
        assert_eq!(row.columns, 3 + WIDGET_COLUMNS + 3);
        assert_eq!(row.column(&state, at(2, 1)), Some(3 + WIDGET_COLUMNS + 1));
        assert_eq!(row.position(&state, 3.2), at(0, 3));
        assert_eq!(row.position(&state, 3.7), at(2, 0));
        assert_eq!(row.position(&state, 5.6), at(2, 2));
        assert_eq!(state.row_count(), 2);
        assert_eq!(position_at(&state, &metrics(), 6.0, 15.0), at(3, 0));
        assert_eq!(position_at(&state, &metrics(), 0.0, 90.0), at(3, 1));
    }

    #[test]
    fn tabs_stop_at_tab_size() {
        let state = State::new("a\tb");
        let row = Row::new(&state, 0);
        assert_eq!(row.column(&state, at(0, 2)), Some(TAB_SIZE));
        assert_eq!(row.position(&state, 3.0), at(0, 2));
        assert_eq!(row.position(&state, 1.9), at(0, 1));
    }

    #[test]
    fn selection_paints_text_only() {
        let state = State::new("abc\n\nde");
        let selection = Selection {
            anchor: at(0, 1),
            head: at(2, 1),
        };
        let rects = selection_rects(&state, &metrics(), selection, 0..3);
        assert_eq!(
            rects,
            vec![
                Rect {
                    left: 9.0,
                    top: 4.0,
                    right: 19.0,
                    bottom: 14.0
                },
                Rect {
                    left: 4.0,
                    top: 24.0,
                    right: 9.0,
                    bottom: 34.0
                },
            ]
        );
        assert_eq!(
            outline(&rects[..1]),
            "M9 4.5H19M9 13.5H19M9.5 4V14M18.5 4V14"
        );
    }

    #[test]
    fn rendered_rows_add_a_margin() {
        let state = State::new(&"x\n".repeat(200));
        let mut metrics = metrics();
        metrics.scroll_top = 504.0;
        assert_eq!(rendered_rows(&state, Some(metrics)), 40..70);
        assert_eq!(rendered_rows(&state, None), 0..50);
        assert_eq!(viewport(&state, &(40..70)), (40, 70));
    }

    #[test]
    fn scrolls_a_caret_below_into_view() {
        let metrics = metrics();
        let rect = Rect {
            left: 4.0,
            top: 204.0,
            right: 4.0,
            bottom: 214.0,
        };
        assert_eq!(
            scroll_into_view(&metrics, 1000.0, 30.0, rect),
            (Some(114.0), Some(0.0))
        );
        let wide = Rect {
            left: 400.0,
            right: 400.0,
            ..rect
        };
        assert_eq!(
            scroll_into_view(&metrics, 1000.0, 30.0, wide).1,
            Some(240.0)
        );
    }
}
