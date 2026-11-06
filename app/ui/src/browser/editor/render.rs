//! The editor's DOM for one render of a session: text rows, gutter, selection, cursor and
//! hints, with the class names the theme, the console and tests style.
use crate::document::Position;
use crate::editor::highlight::Span;
use crate::editor::layout::{self, Piece, Row, WIDGET_COLUMNS};
use crate::editor::session::{Mark, Metrics, Session};
use crate::editor::text::{TAB_SIZE, Utf16Cursor};
use crate::editor::{State, checks};
use std::collections::BTreeMap;
use yew::prelude::*;

pub(super) const DEFAULT_LINE_HEIGHT: f64 = 15.0;
pub(super) const DEFAULT_CHAR_WIDTH: f64 = 7.8;

/// The geometry of one render.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Frame {
    pub metrics: Metrics,
    pub first: usize,
    pub end: usize,
    pub gutter: f64,
    pub numbers: f64,
    pub console: bool,
    pub height: f64,
}

/// The metrics to lay out with, defaulted until the view is measured.
pub(super) fn metrics(metrics: Option<Metrics>) -> Metrics {
    let mut metrics = metrics.unwrap_or_default();
    if metrics.line_height <= 0.0 {
        metrics.line_height = DEFAULT_LINE_HEIGHT;
    }
    if metrics.char_width <= 0.0 {
        metrics.char_width = DEFAULT_CHAR_WIDTH;
    }
    metrics
}

pub(super) fn frame(session: &Session) -> Frame {
    let state = session.state();
    let console = session.setup().console;
    let metrics = metrics(session.metrics());
    let rows = layout::rendered_rows(state, session.metrics());
    let numbers = layout::number_width(state.line_count(), metrics.char_width);
    Frame {
        metrics,
        first: rows.start,
        end: rows.end,
        gutter: numbers + if console { layout::FOLD_GUTTER } else { 0.0 },
        numbers,
        console,
        height: layout::document_height(state, &metrics, console),
    }
}

/// The widest line in columns; folds joining lines are not counted.
pub(super) fn columns(state: &State) -> usize {
    let mut widest = 0;
    for line in state.lines() {
        if line.len() * TAB_SIZE <= widest {
            continue;
        }
        let columns = if line.contains('\t') {
            layout::advance(0, line)
        } else if line.is_ascii() {
            line.len()
        } else {
            line.chars().count()
        };
        widest = widest.max(columns);
    }
    widest
}

/// A class on a run of a line's text, in UTF-16 columns.
#[derive(Clone, Debug, PartialEq)]
struct Decoration {
    from: usize,
    to: usize,
    class: AttrValue,
}

/// What the rendered lines show besides their tokens.
#[derive(Default)]
struct Decorations {
    text: BTreeMap<usize, Vec<Decoration>>,
    lines: BTreeMap<usize, Vec<AttrValue>>,
}

fn decorations(session: &Session, lines: std::ops::RangeInclusive<usize>) -> Decorations {
    let state = session.state();
    let mut out = Decorations::default();
    let mut add = |line: usize, from: usize, to: usize, class: AttrValue| {
        if lines.contains(&line) && from < to {
            out.text
                .entry(line)
                .or_default()
                .push(Decoration { from, to, class });
        }
    };
    for (_, mark) in session.marks() {
        match mark {
            Mark::Range { from, to, class } => {
                for line in from.line.max(*lines.start())..=to.line.min(*lines.end()) {
                    let start = if line == from.line { from.ch } else { 0 };
                    let end = if line == to.line { to.ch } else { usize::MAX };
                    add(line, start, end, AttrValue::from(class.clone()));
                }
            }
            Mark::Line { anchor, class } => {
                if let Some(line) = state.anchor_line(*anchor)
                    && lines.contains(&line)
                {
                    out.lines
                        .entry(line)
                        .or_default()
                        .push(AttrValue::from(class.clone()));
                }
            }
        }
    }
    for (at, matched) in session.brackets() {
        let class = if *matched {
            "editor-matchingbracket"
        } else {
            "editor-nonmatchingbracket"
        };
        add(at.line, at.ch, at.ch + 1, AttrValue::Static(class));
    }
    for (line, from, to) in session.objects() {
        add(
            *line,
            *from,
            *to,
            AttrValue::Static("tok-graphql-object-field"),
        );
    }
    out
}

/// Appends `text` with tabs expanded to the next tab stop, advancing `column`.
fn expand(text: &str, column: &mut usize, out: &mut String) {
    for c in text.chars() {
        if c == '\t' {
            let width = TAB_SIZE - *column % TAB_SIZE;
            out.extend(std::iter::repeat_n(' ', width));
            *column += width;
        } else {
            out.push(c);
            *column += 1;
        }
    }
}

/// The token spans of one piece of a row, merged where their classes match.
fn piece(
    text: &str,
    piece: &Piece,
    spans: &[Span],
    decorations: &[Decoration],
    out: &mut Vec<Html>,
) {
    let mut cuts = vec![piece.from, piece.to];
    let inside = |cut: usize| cut > piece.from && cut < piece.to;
    cuts.extend(spans.iter().map(|span| span.end).filter(|end| inside(*end)));
    for decoration in decorations {
        cuts.extend(
            [decoration.from, decoration.to]
                .into_iter()
                .filter(|cut| inside(*cut)),
        );
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut column = piece.column;
    let mut bytes = Utf16Cursor::new(text);
    let mut span = 0;
    let mut run: Option<(String, String)> = None;
    let flush = |run: Option<(String, String)>, out: &mut Vec<Html>| {
        if let Some((class, text)) = run {
            out.push(if class.is_empty() {
                html! {{text}}
            } else {
                html! {<span class={class}>{text}</span>}
            });
        }
    };
    for pair in cuts.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        while span < spans.len() && spans[span].end <= from {
            span += 1;
        }
        let mut class = String::new();
        if let Some(style) = spans.get(span).and_then(|span| span.style) {
            for part in style.split_whitespace() {
                if !class.is_empty() {
                    class.push(' ');
                }
                class.push_str("tok-");
                class.push_str(part);
            }
        }
        for decoration in decorations {
            if decoration.from <= from && to <= decoration.to {
                if !class.is_empty() {
                    class.push(' ');
                }
                class.push_str(&decoration.class);
            }
        }
        let slice = &text[bytes.byte_at(from)..bytes.byte_at(to)];
        match &mut run {
            Some((current, body)) if *current == class => expand(slice, &mut column, body),
            _ => {
                flush(run.take(), out);
                let mut body = String::new();
                expand(slice, &mut column, &mut body);
                run = Some((class, body));
            }
        }
    }
    flush(run, out);
}

/// The text of one line on a row, with what decorates it.
#[derive(Clone, Debug, PartialEq)]
struct LinePiece {
    text: String,
    piece: Piece,
    spans: std::rc::Rc<[Span]>,
    decorations: Vec<Decoration>,
    /// The line of the fold whose marker follows the piece.
    fold: Option<usize>,
}

/// A rendered row; it is rebuilt only when what it shows changes, so a cursor or
/// selection change does not rebuild every token of the viewport.
#[derive(Properties, Clone, Debug, PartialEq)]
struct LineProps {
    pieces: Vec<LinePiece>,
    blank: bool,
    band: Option<&'static str>,
    backgrounds: Vec<AttrValue>,
    widget_width: f64,
}

#[function_component]
fn Line(props: &LineProps) -> Html {
    let mut children = Vec::new();
    for part in &props.pieces {
        piece(
            &part.text,
            &part.piece,
            &part.spans,
            &part.decorations,
            &mut children,
        );
        if let Some(line) = part.fold {
            children.push(html! {
                <span class="editor-widget" role="presentation">
                    <span class="editor-foldmarker" data-line={line.to_string()}
                        style={format!("width:{}px", props.widget_width)}>{"\u{2194}"}</span>
                </span>
            });
        }
    }
    if props.blank {
        children.push(html! {{"\u{200b}"}});
    }
    html! {
        <div role="presentation">
            <>{ for props.backgrounds.iter().map(|class| html! {
                <div class={classes!("editor-linebackground", class.clone())}/>
            }) }</>
            <pre class={classes!("editor-line", props.band)} role="presentation">
                <span role="presentation">{ for children }</span>
            </pre>
        </div>
    }
}

/// The text rows from `frame.first` to `frame.end`.
pub(super) fn rows(session: &mut Session, frame: &Frame) -> Html {
    let (first_line, last_line) = {
        let state = session.state();
        if frame.first >= frame.end {
            return html! {<div class="editor-code" role="presentation"/>};
        }
        (
            state.line_of_row(frame.first),
            Row::new(state, state.line_of_row(frame.end - 1)).last_line(),
        )
    };
    // Tokens need the highlighter mutably, decorations borrow the session; take tokens first.
    let mut tokens: BTreeMap<usize, std::rc::Rc<[Span]>> = BTreeMap::new();
    let layouts: Vec<Row> = {
        let (state, highlighter) = session.parts();
        (frame.first..frame.end)
            .map(|row| {
                let layout = Row::new(state, state.line_of_row(row));
                for piece in &layout.pieces {
                    tokens
                        .entry(piece.line)
                        .or_insert_with(|| highlighter.spans(state.lines(), piece.line));
                }
                layout
            })
            .collect()
    };
    let session = &*session;
    let state = session.state();
    let mut decorations = decorations(session, first_line..=last_line);
    let widget_width = frame.metrics.char_width * WIDGET_COLUMNS as f64;
    let rows = layouts.into_iter().map(|layout| {
        let line = layout.pieces[0].line;
        let key = state
            .anchor(line)
            .map_or(line as u64, |anchor| anchor.key());
        let pieces = layout
            .pieces
            .iter()
            .enumerate()
            .map(|(index, part)| LinePiece {
                text: state.line(part.line).to_owned(),
                piece: part.clone(),
                spans: tokens
                    .get(&part.line)
                    .cloned()
                    .unwrap_or_else(|| std::rc::Rc::from([])),
                decorations: decorations
                    .text
                    .get(&part.line)
                    .cloned()
                    .unwrap_or_default(),
                fold: layout.widgets.get(index).map(|(_, fold)| fold.from.line),
            })
            .collect();
        let props = LineProps {
            pieces,
            blank: layout.columns == 0,
            band: frame
                .console
                .then(|| checks::band(session.check_lines(), line))
                .flatten(),
            backgrounds: decorations.lines.remove(&line).unwrap_or_default(),
            widget_width,
        };
        html! { <Line key={key} ..props/> }
    });
    let top = layout::row_top(frame.first, frame.metrics.line_height);
    html! {
        <div class="editor-code" role="presentation"
            style={format!("left:{}px;top:{top}px;--line-height:{}px", frame.gutter, frame.metrics.line_height)}>
            { for rows }
        </div>
    }
}

/// The line numbers and, in the console, the fold markers of the rendered rows.
pub(super) fn gutter(session: &mut Session, frame: &Frame) -> Html {
    let lines: Vec<usize> = (frame.first..frame.end)
        .map(|row| session.state().line_of_row(row))
        .collect();
    let rows = lines.into_iter().map(|line| {
        let key = session.state().anchor(line).map_or(line as u64, |anchor| anchor.key());
        let marker = if frame.console {
            session.fold_marker(line)
        } else {
            None
        };
        html! {
            <div key={key} class="editor-gutter-wrapper">
                <div class="editor-linenumber editor-gutter-elt"
                    style={format!("width:{}px", frame.numbers)}>{line + 1}</div>
                if frame.console {
                    <div class="editor-gutter-elt console-fold-gutter" data-line={line.to_string()}>
                        if let Some(folded) = marker {
                            <div class={if folded { "console-fold-folded" } else { "console-fold-open" }}/>
                        }
                    </div>
                }
            </div>
        }
    });
    let top = layout::row_top(frame.first, frame.metrics.line_height);
    html! {
        <div class="editor-gutters"
            style={format!("width:{}px;height:{}px", frame.gutter, frame.height)}>
            <div class="editor-gutter-rows"
                style={format!("top:{top}px;--line-height:{}px", frame.metrics.line_height)}>
                { for rows }
            </div>
        </div>
    }
}

/// The selection: rectangles under the text and their outline.
pub(super) fn selection(session: &Session, frame: &Frame) -> Html {
    let state = session.state();
    let rects = layout::selection_rects(
        state,
        &frame.metrics,
        state.selection(),
        frame.first..frame.end,
    );
    let outline = (!rects.is_empty()).then(|| layout::outline(&rects));
    html! {
        <div class="editor-selection-layer" style={format!("left:{}px", frame.gutter)}>
            <>{ for rects.iter().map(|rect| html! {
                <div class="editor-selected" style={format!(
                    "left:{}px;top:{}px;width:{}px;height:{}px",
                    rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top
                )}/>
            }) }</>
            if let Some(path) = outline {
                <svg class="editor-selection-outline">
                    <path d={path} fill="none" stroke="currentColor" stroke-width="1"/>
                </svg>
            }
        </div>
    }
}

/// The cursor of a focused editor, restarted on every change so it shows while the user types.
pub(super) fn cursor(session: &Session, frame: &Frame) -> Html {
    let state = session.state();
    let caret = (session.focused() && state.selection().empty())
        .then(|| layout::caret(state, &frame.metrics, state.cursor()))
        .flatten();
    html! {
        <div class="editor-cursors" style={format!("left:{}px", frame.gutter)}>
            if let Some(caret) = caret {
                <div key={session.revision().to_string()} class="editor-cursor" style={format!(
                    "left:{}px;top:{}px;height:{}px",
                    caret.left, caret.top, frame.metrics.line_height
                )}/>
            }
        </div>
    }
}

/// Where the text-area point `(x, y)` is in the window, given the scroller's bounds.
pub(super) fn to_window(frame: &Frame, bounds: (f64, f64), x: f64, y: f64) -> (f64, f64) {
    (
        bounds.0 + frame.gutter + x - frame.metrics.scroll_left,
        bounds.1 + y - frame.metrics.scroll_top,
    )
}

/// The caret of the hints' start in window coordinates: `(left, top, bottom)`, when visible.
pub(super) fn hint_anchor(
    session: &Session,
    frame: &Frame,
    bounds: (f64, f64),
) -> Option<(f64, f64, f64)> {
    let popup = session.popup()?;
    let state = session.state();
    let caret = layout::caret(state, &frame.metrics, state.clip(popup.from))?;
    let visible = caret.bottom > frame.metrics.scroll_top
        && caret.top
            < frame.metrics.scroll_top + frame.metrics.client_height.max(frame.metrics.line_height);
    if !visible {
        return None;
    }
    let (left, top) = to_window(frame, bounds, caret.left, caret.top);
    Some((left, top, top + frame.metrics.line_height))
}

/// The open hints list under the caret; the view moves it above when it does not fit.
pub(super) fn hints(
    session: &Session,
    anchor: (f64, f64, f64),
    node: &NodeRef,
    onclick: Callback<MouseEvent>,
) -> Html {
    let Some(popup) = session.popup() else {
        return Html::default();
    };
    let (left, _, bottom) = anchor;
    html! {
        <ul ref={node.clone()} class="editor-hints dracula" role="listbox"
            style={format!("left:{left}px;top:{bottom}px")}
            onmousedown={|event: MouseEvent| event.prevent_default()} {onclick}>
            { for popup.items.iter().enumerate().map(|(index, item)| html! {
                <li class={classes!("editor-hint", (index == popup.selected).then_some("editor-hint-active"))}
                    role="option" data-index={index.to_string()}>{&item.display}</li>
            }) }
        </ul>
    }
}

/// The position whose character is under the text-area point, for hover.
pub(super) fn char_at(state: &State, metrics: &Metrics, x: f64, y: f64) -> Option<Position> {
    let row = ((y - layout::PADDING_TOP) / metrics.line_height).floor();
    if row < 0.0 || row >= state.row_count() as f64 {
        return None;
    }
    let line = state.line_of_row(row as usize);
    let column = ((x - layout::LINE_PADDING) / metrics.char_width.max(1.0)).floor();
    if column < 0.0 {
        return None;
    }
    Some(Row::new(state, line).position(state, column))
}
