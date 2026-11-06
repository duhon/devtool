//! A JS object on the editor's elements, for the find bar and browser tests.
use super::get;
use super::handle::{Handle, Shared, now};
use crate::document::{Position, TextChange};
use crate::editor::layout::{self, Rect};
use crate::editor::session::{Command, Mark, Metrics, Msg};
use crate::editor::text::{split_lines, utf16_len};
use crate::editor::{Change, Selection, Transaction};
use js_sys::{Array, Object};
use std::rc::Weak;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
const METHODS = [
  "getValue", "setValue", "getLine", "lastLine", "firstLine", "lineCount", "getRange",
  "replaceRange", "replaceSelection", "getCursor", "setCursor", "setSelection", "setSelections",
  "listSelections", "getSelection", "somethingSelected", "undo", "redo", "historySize",
  "clearHistory", "changeGeneration", "isClean", "focus", "hasFocus", "getWrapperElement",
  "getScrollerElement", "getInputField", "getScrollInfo", "scrollTo", "scrollIntoView",
  "charCoords", "cursorCoords", "coordsChar", "heightAtLine", "lineAtHeight",
  "defaultTextHeight", "defaultCharWidth", "getViewport", "getLineTokens", "getTokenAt",
  "getTokenTypeAt", "getOption", "setOption", "on", "off", "startOperation", "endOperation",
  "closeHint", "execCommand", "lineInfo", "setSize", "refresh", "getMode", "foldCode",
  "indexFromPos", "posFromIndex", "foldRange",
];
export function create(dispatch) {
  const editor = {};
  for (const name of METHODS) editor[name] = (...args) => dispatch(name, args);
  // A mark stays the same object while it exists.
  const marks = new Map();
  const mark = (info) => {
    let found = marks.get(info.id);
    if (!found) {
      found = {
        clear: () => { marks.delete(info.id); dispatch("clearMark", [info.id]); },
        find: () => dispatch("findMark", [info.id]),
      };
      marks.set(info.id, found);
    }
    return Object.assign(found, info);
  };
  editor.markText = (...args) => mark(dispatch("markText", args));
  editor.getAllMarks = () => {
    const list = dispatch("getAllMarks", []);
    const ids = new Set(list.map((info) => info.id));
    for (const id of marks.keys()) if (!ids.has(id)) marks.delete(id);
    return list.map(mark);
  };
  editor.operation = (f) => {
    dispatch("startOperation", []);
    try { return f(); } finally { dispatch("endOperation", []); }
  };
  // Line tokens cross from wasm as the text, token ends and styles: objects per token
  // built across the boundary cost several times the tokenizing.
  editor.getLineTokens = (line) => {
    const [text, ends, styles] = dispatch("getLineTokens", [line]);
    const types = styles.split("\n");
    const tokens = [];
    let start = 0;
    for (let i = 0; i < ends.length; i++) {
      const end = ends[i];
      tokens.push({ start, end, string: text.slice(start, end), type: types[i] || null });
      start = end;
    }
    return tokens;
  };
  editor.getDoc = () => editor;
  Object.defineProperty(editor, "state", {
    get: () => {
      const state = dispatch("state", []);
      if (state.completionActive) state.completionActive.pick = (_data, index) => dispatch("pickHint", [index]);
      return state;
    },
  });
  Object.defineProperty(editor, "__servicesCompletion", { get: () => dispatch("servicesCompletion", []) });
  return editor;
}
"#)]
extern "C" {
    fn create(dispatch: &js_sys::Function) -> JsValue;
}

type Dispatch = Closure<dyn FnMut(String, Array) -> Result<JsValue, JsValue>>;

pub(super) struct Facade {
    pub object: JsValue,
    _dispatch: Dispatch,
}

impl Facade {
    pub fn new(shared: Weak<Shared>) -> Self {
        let dispatch: Dispatch = Closure::new(move |name: String, args: Array| {
            let handle = Handle(shared.upgrade().ok_or("The editor was closed")?);
            call(&handle, &name, &args)
        });
        let object = create(dispatch.as_ref().unchecked_ref());
        Self {
            object,
            _dispatch: dispatch,
        }
    }
}

fn set(target: &Object, key: &str, value: impl Into<JsValue>) {
    super::set(target, key, &value.into());
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let target = Object::new();
    for (key, value) in entries {
        set(&target, key, value.clone());
    }
    target.into()
}

fn position(at: Position) -> JsValue {
    object(&[("line", at.line.into()), ("ch", at.ch.into())])
}

fn number(value: &JsValue) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}

fn read_position(value: &JsValue) -> Option<Position> {
    let line = number(&get(value, "line"))?;
    let ch = number(&get(value, "ch")).unwrap_or(0.0);
    Some(Position {
        line: line.max(0.0) as usize,
        ch: ch.max(0.0) as usize,
    })
}

fn lines(lines: &[String]) -> JsValue {
    lines.iter().map(JsValue::from).collect::<Array>().into()
}

/// A change as `changes` listeners receive it.
pub(super) fn change(change: &TextChange) -> JsValue {
    object(&[
        ("from", position(change.from)),
        ("to", position(change.to)),
        ("text", lines(&change.text)),
        ("removed", lines(&change.removed)),
        (
            "origin",
            change
                .origin
                .as_deref()
                .map_or(JsValue::UNDEFINED, JsValue::from),
        ),
    ])
}

fn rect(rect: Rect, (x, y): (f64, f64)) -> JsValue {
    object(&[
        ("left", (rect.left + x).into()),
        ("right", (rect.right + x).into()),
        ("top", (rect.top + y).into()),
        ("bottom", (rect.bottom + y).into()),
    ])
}

/// The position after inserting `text` at `from`.
fn end_of(from: Position, text: &str) -> Position {
    let lines = split_lines(text);
    let last = utf16_len(lines.last().map_or("", String::as_str));
    if lines.len() > 1 {
        Position {
            line: from.line + lines.len() - 1,
            ch: last,
        }
    } else {
        Position {
            line: from.line,
            ch: from.ch + last,
        }
    }
}

fn metrics(handle: &Handle) -> Metrics {
    super::render::metrics(handle.metrics())
}

/// What to add to text-area coordinates for the coordinate `mode`.
fn origin(handle: &Handle, mode: &JsValue) -> (f64, f64) {
    let mode = mode.as_string().unwrap_or_else(|| "page".into());
    let Some(dom) = handle.dom().filter(|_| mode == "window" || mode == "page") else {
        return (0.0, 0.0);
    };
    let metrics = metrics(handle);
    let bounds = dom.scroller.get_bounding_client_rect();
    let mut x = bounds.left() + metrics.gutter_width - metrics.scroll_left;
    let mut y = bounds.top() - metrics.scroll_top;
    if mode == "page" {
        let window = super::window();
        x += window.page_x_offset().unwrap_or_default();
        y += window.page_y_offset().unwrap_or_default();
    }
    (x, y)
}

fn caret(handle: &Handle, at: Position) -> Rect {
    let metrics = metrics(handle);
    handle.read(|session| {
        let state = session.state();
        let at = state.clip(at);
        layout::caret(state, &metrics, at).unwrap_or_else(|| {
            let line = state.line_of_row(state.row_of_line(at.line));
            layout::caret(state, &metrics, Position { line, ch: 0 }).unwrap_or(Rect {
                left: layout::LINE_PADDING,
                top: layout::PADDING_TOP,
                right: layout::LINE_PADDING,
                bottom: layout::PADDING_TOP + metrics.line_height,
            })
        })
    })
}

fn cursor(handle: &Handle, side: &JsValue) -> Position {
    let selection = handle.read(|session| session.state().selection());
    match side.as_string().as_deref() {
        Some("anchor") => selection.anchor,
        Some("from" | "start") => selection.from(),
        Some("to" | "end") => selection.to(),
        _ => selection.head,
    }
}

fn select(handle: &Handle, selection: Selection, options: &JsValue) {
    let scroll = get(options, "scroll") != JsValue::FALSE;
    handle.with(|session| session.select(selection, scroll));
}

fn replace(handle: &Handle, from: Position, to: Position, text: String, origin: &JsValue, selection: Option<Selection>) {
    let origin = origin.as_string().unwrap_or_default();
    handle.with(|session| {
        let mut transaction = Transaction::replace(from, to, text, &origin);
        transaction.selection = selection;
        session.apply(transaction, now())
    });
}

fn tokens(handle: &Handle, line: usize) -> Vec<(usize, usize, String, Option<&'static str>)> {
    handle.with(|session| {
        if line >= session.state().line_count() {
            return Vec::new();
        }
        let (state, highlighter) = session.parts();
        let spans = highlighter.spans(state.lines(), line);
        let text = state.line(line);
        let mut start = 0;
        let mut tokens = Vec::new();
        for span in spans.iter() {
            if span.end <= start {
                continue;
            }
            let range = state.range(Position { line, ch: start }, Position { line, ch: span.end });
            tokens.push((start, span.end, range, span.style));
            start = span.end;
        }
        if text.is_empty() {
            tokens.push((0, 0, String::new(), None));
        }
        tokens
    })
}

fn token(token: &(usize, usize, String, Option<&'static str>)) -> JsValue {
    object(&[
        ("start", token.0.into()),
        ("end", token.1.into()),
        ("string", token.2.as_str().into()),
        ("type", token.3.map_or(JsValue::NULL, JsValue::from)),
    ])
}

fn token_at(handle: &Handle, at: Position) -> (usize, usize, String, Option<&'static str>) {
    if at.ch == 0 {
        return (0, 0, String::new(), None);
    }
    tokens(handle, at.line)
        .into_iter()
        .find(|token| token.1 >= at.ch)
        .unwrap_or((at.ch, at.ch, String::new(), None))
}

fn marks(handle: &Handle) -> JsValue {
    handle.read(|session| {
        let list = Array::new();
        for (id, mark) in session.marks() {
            let class = match mark {
                Mark::Range { class, .. } | Mark::Line { class, .. } => class.as_str(),
            };
            list.push(&object(&[("id", id.to_string().into()), ("className", class.into())]));
        }
        for fold in session.state().folds() {
            list.push(&object(&[
                ("id", format!("fold:{}", fold.from.line).into()),
                ("collapsed", true.into()),
                ("__isFold", true.into()),
            ]));
        }
        list.into()
    })
}

fn fold_line(id: &str) -> Option<usize> {
    id.strip_prefix("fold:")?.parse().ok()
}

fn options(handle: &Handle, name: &str) -> JsValue {
    handle.read(|session| match name {
        "readOnly" => session.setup().readonly.into(),
        "servicesResponses" => super::js(&serde_json::to_value(session.regions()).unwrap_or_default()),
        "servicesChecks" => session.check_lines().iter().map(|line| JsValue::from(*line)).collect::<Array>().into(),
        "mode" => object(&[("name", format!("services-{}", session.setup().mode).into())]),
        "lineNumbers" => true.into(),
        "lineWrapping" => false.into(),
        "indentUnit" => 2.into(),
        "selectionStyle" => "text".into(),
        "scrollbarStyle" => "native".into(),
        "foldGutter" => session.setup().console.into(),
        _ => JsValue::UNDEFINED,
    })
}

fn command(name: &str) -> Option<Command> {
    use crate::editor::Unit;
    Some(match name {
        "undo" => Command::Undo,
        "redo" => Command::Redo,
        "selectAll" => Command::SelectAll,
        "deleteLine" => Command::DeleteLine,
        "newlineAndIndent" => Command::Newline,
        "indentMore" => Command::Indent(crate::editor::Indent::Add),
        "indentLess" => Command::Indent(crate::editor::Indent::Subtract),
        "goDocStart" => Command::DocStart(false),
        "goDocEnd" => Command::DocEnd(false),
        "goLineStartSmart" => Command::Home(false),
        "goLineEnd" => Command::End(false),
        "goCharLeft" => Command::MoveH(-1, Unit::Char, false),
        "goCharRight" => Command::MoveH(1, Unit::Char, false),
        "goLineUp" => Command::MoveV(-1, false),
        "goLineDown" => Command::MoveV(1, false),
        "delCharBefore" => Command::Backspace,
        "delCharAfter" => Command::Delete,
        "autocomplete" => Command::Complete,
        _ => return None,
    })
}

fn completion(handle: &Handle) -> JsValue {
    handle.read(|session| {
        let Some(popup) = session.popup() else {
            return object(&[("completionActive", JsValue::NULL)]);
        };
        let list: Array = popup
            .items
            .iter()
            .map(|item| {
                object(&[
                    ("text", item.text.as_str().into()),
                    ("displayText", item.display.as_str().into()),
                    ("literal", item.literal.into()),
                ])
            })
            .collect();
        let active = object(&[
            ("data", object(&[("list", list.into()), ("from", position(popup.from)), ("to", position(popup.to))])),
            ("widget", object(&[("selectedHint", popup.selected.into())])),
        ]);
        object(&[("completionActive", active)])
    })
}

fn call(handle: &Handle, name: &str, args: &Array) -> Result<JsValue, JsValue> {
    let arg = |index: u32| args.get(index);
    let at = |index: u32| read_position(&arg(index));
    let missing = || JsValue::from_str(&format!("{name}: missing position"));
    let state = |read: &dyn Fn(&crate::editor::State) -> JsValue| handle.read(|session| read(session.state()));
    Ok(match name {
        "getValue" => handle.text().as_ref().into(),
        "setValue" => {
            let text = arg(0).as_string().unwrap_or_default();
            handle.with(|session| {
                let last = session.state().last_line();
                let end = Position {
                    line: last,
                    ch: utf16_len(session.state().line(last)),
                };
                session.apply(
                    Transaction {
                        changes: vec![Change {
                            from: Position::default(),
                            to: end,
                            text,
                        }],
                        origin: "setValue".into(),
                        selection: Some(Selection::default()),
                        join: false,
                    },
                    now(),
                )
            });
            handle.scroll_to(Some(0.0), Some(0.0));
            JsValue::UNDEFINED
        }
        "getLine" => {
            let line = number(&arg(0)).unwrap_or(-1.0);
            state(&|state| {
                if line >= 0.0 && (line as usize) < state.line_count() {
                    state.line(line as usize).into()
                } else {
                    JsValue::UNDEFINED
                }
            })
        }
        "lastLine" => state(&|state| state.last_line().into()),
        "firstLine" => 0.into(),
        "lineCount" => state(&|state| state.line_count().into()),
        "getRange" => {
            let (from, to) = (at(0).ok_or_else(missing)?, at(1).ok_or_else(missing)?);
            state(&|state| {
                let (from, to) = (state.clip(from), state.clip(to));
                state
                    .range(crate::editor::text::min(from, to), crate::editor::text::max(from, to))
                    .into()
            })
        }
        "replaceRange" => {
            let from = at(1).ok_or_else(missing)?;
            let to = at(2).unwrap_or(from);
            replace(handle, from, to, arg(0).as_string().unwrap_or_default(), &arg(3), None);
            JsValue::UNDEFINED
        }
        "replaceSelection" => {
            let text = arg(0).as_string().unwrap_or_default();
            let selection = handle.read(|session| session.state().selection());
            let (from, to) = (selection.from(), selection.to());
            let end = end_of(from, &text);
            let after = match arg(1).as_string().as_deref() {
                Some("around") => Selection { anchor: from, head: end },
                Some("start") => Selection::cursor(from),
                _ => Selection::cursor(end),
            };
            replace(handle, from, to, text, &arg(2), Some(after));
            JsValue::UNDEFINED
        }
        "getCursor" => position(cursor(handle, &arg(0))),
        "setCursor" => {
            let at = match number(&arg(0)) {
                Some(line) => Position {
                    line: line.max(0.0) as usize,
                    ch: number(&arg(1)).unwrap_or(0.0).max(0.0) as usize,
                },
                None => at(0).ok_or_else(missing)?,
            };
            let options = if number(&arg(0)).is_some() { arg(2) } else { arg(1) };
            select(handle, Selection::cursor(at), &options);
            JsValue::UNDEFINED
        }
        "setSelection" => {
            let anchor = at(0).ok_or_else(missing)?;
            let head = at(1).unwrap_or(anchor);
            select(handle, Selection { anchor, head }, &arg(2));
            JsValue::UNDEFINED
        }
        "setSelections" => {
            let ranges: Array = arg(0).dyn_into().map_err(|_| missing())?;
            let primary = number(&arg(1)).map_or(ranges.length().saturating_sub(1), |index| index as u32);
            let range = ranges.get(primary);
            let anchor = read_position(&get(&range, "anchor")).ok_or_else(missing)?;
            let head = read_position(&get(&range, "head")).unwrap_or(anchor);
            select(handle, Selection { anchor, head }, &arg(2));
            JsValue::UNDEFINED
        }
        "listSelections" => {
            let selection = handle.read(|session| session.state().selection());
            let range = object(&[("anchor", position(selection.anchor)), ("head", position(selection.head))]);
            Array::of1(&range).into()
        }
        "getSelection" => state(&|state| state.selected_text().into()),
        "somethingSelected" => state(&|state| (!state.selection().empty()).into()),
        "undo" => {
            handle.with(|session| session.undo());
            JsValue::UNDEFINED
        }
        "redo" => {
            handle.with(|session| session.redo());
            JsValue::UNDEFINED
        }
        "historySize" => {
            let (undo, redo) = state(&|state| {
                let (undo, redo) = state.history_size();
                Array::of2(&undo.into(), &redo.into()).into()
            })
            .dyn_into::<Array>()
            .map(|sizes| (sizes.get(0), sizes.get(1)))?;
            object(&[("undo", undo), ("redo", redo)])
        }
        "clearHistory" => {
            handle.with(|session| session.clear_history());
            JsValue::UNDEFINED
        }
        "changeGeneration" => {
            if arg(0).is_truthy() {
                handle.with(|session| session.close_history()).into()
            } else {
                state(&|state| state.generation().into())
            }
        }
        "isClean" => {
            let generation = number(&arg(0)).unwrap_or(-1.0);
            state(&|state| (state.generation() as f64 == generation).into())
        }
        "focus" => {
            if let Some(dom) = handle.dom() {
                let _ = dom.input.focus();
            }
            JsValue::UNDEFINED
        }
        "hasFocus" => handle
            .dom()
            .and_then(|dom| {
                let active = super::window().document()?.active_element()?;
                Some(JsValue::from(&active == AsRef::<web_sys::Element>::as_ref(&dom.input)))
            })
            .unwrap_or(JsValue::FALSE),
        "getWrapperElement" => handle.0.wrapper.borrow().clone().map_or(JsValue::NULL, Into::into),
        "getScrollerElement" => handle.dom().map_or(JsValue::NULL, |dom| dom.scroller.into()),
        "getInputField" => handle.dom().map_or(JsValue::NULL, |dom| dom.input.into()),
        "getScrollInfo" => {
            let metrics = metrics(handle);
            let (width, height) = handle
                .dom()
                .map_or((0, 0), |dom| (dom.scroller.scroll_width(), dom.scroller.scroll_height()));
            object(&[
                ("left", metrics.scroll_left.into()),
                ("top", metrics.scroll_top.into()),
                ("width", width.into()),
                ("height", height.into()),
                ("clientWidth", metrics.client_width.into()),
                ("clientHeight", metrics.client_height.into()),
            ])
        }
        "scrollTo" => {
            handle.scroll_to(number(&arg(1)), number(&arg(0)));
            JsValue::UNDEFINED
        }
        "scrollIntoView" => {
            let target = arg(0);
            let margin = number(&arg(1)).unwrap_or(0.0);
            let mut area = if target.is_null() || target.is_undefined() {
                caret(handle, cursor(handle, &JsValue::UNDEFINED))
            } else if let Some(at) = read_position(&target) {
                caret(handle, at)
            } else if let (Some(from), Some(to)) = (
                read_position(&get(&target, "from")),
                read_position(&get(&target, "to")),
            ) {
                let (from, to) = (caret(handle, from), caret(handle, to));
                Rect {
                    left: from.left.min(to.left),
                    top: from.top.min(to.top),
                    right: from.right.max(to.right),
                    bottom: from.bottom.max(to.bottom),
                }
            } else {
                Rect {
                    left: number(&get(&target, "left")).unwrap_or(0.0),
                    top: number(&get(&target, "top")).unwrap_or(0.0),
                    right: number(&get(&target, "right")).unwrap_or(0.0),
                    bottom: number(&get(&target, "bottom")).unwrap_or(0.0),
                }
            };
            area.top -= margin;
            area.bottom += margin;
            let metrics = metrics(handle);
            let height = handle.read(|session| {
                layout::document_height(session.state(), &metrics, session.setup().console)
            });
            let (top, left) = layout::scroll_into_view(&metrics, height, metrics.gutter_width, area);
            handle.scroll_to(top, left);
            JsValue::UNDEFINED
        }
        "charCoords" | "cursorCoords" => {
            let at = at(0).unwrap_or_else(|| cursor(handle, &arg(0)));
            let mut area = caret(handle, at);
            if name == "charCoords" {
                area.right += metrics(handle).char_width;
            }
            rect(area, origin(handle, &arg(1)))
        }
        "coordsChar" => {
            let (x, y) = origin(handle, &arg(1));
            let left = number(&get(&arg(0), "left")).unwrap_or(0.0) - x;
            let top = number(&get(&arg(0), "top")).unwrap_or(0.0) - y;
            let metrics = metrics(handle);
            position(handle.read(|session| layout::position_at(session.state(), &metrics, left, top)))
        }
        "heightAtLine" => {
            let line = number(&arg(0)).unwrap_or(0.0).max(0.0) as usize;
            let metrics = metrics(handle);
            let top = handle.read(|session| {
                let state = session.state();
                if line >= state.line_count() {
                    layout::row_top(state.row_count(), metrics.line_height)
                } else {
                    layout::row_top(state.row_of_line(line), metrics.line_height)
                }
            });
            (top + origin(handle, &arg(1)).1).into()
        }
        "lineAtHeight" => {
            let y = number(&arg(0)).unwrap_or(0.0) - origin(handle, &arg(1)).1;
            let metrics = metrics(handle);
            handle
                .read(|session| {
                    let state = session.state();
                    let row = ((y - layout::PADDING_TOP) / metrics.line_height).floor().max(0.0) as usize;
                    if row >= state.row_count() {
                        state.last_line()
                    } else {
                        state.line_of_row(row)
                    }
                })
                .into()
        }
        "defaultTextHeight" => metrics(handle).line_height.into(),
        "defaultCharWidth" => metrics(handle).char_width.into(),
        "getViewport" => {
            let (from, to) = handle.read(|session| {
                let rows = layout::rendered_rows(session.state(), session.metrics());
                layout::viewport(session.state(), &rows)
            });
            object(&[("from", from.into()), ("to", to.into())])
        }
        "getLineTokens" => {
            let line = number(&arg(0)).unwrap_or(0.0).max(0.0) as usize;
            let (text, ends, styles) = handle.with(|session| {
                let mut ends = Vec::new();
                let mut styles = Vec::new();
                if line >= session.state().line_count() {
                    return (String::new(), ends, String::new());
                }
                let (state, highlighter) = session.parts();
                for span in highlighter.spans(state.lines(), line).iter() {
                    if span.end as u32 > ends.last().copied().unwrap_or(0) {
                        ends.push(span.end as u32);
                        styles.push(span.style.unwrap_or_default());
                    }
                }
                (state.line(line).to_owned(), ends, styles.join("\n"))
            });
            Array::of3(&text.into(), &js_sys::Uint32Array::from(&ends[..]), &styles.into()).into()
        }
        "getTokenAt" => token(&token_at(handle, at(0).ok_or_else(missing)?)),
        "getTokenTypeAt" => token_at(handle, at(0).ok_or_else(missing)?)
            .3
            .map_or(JsValue::NULL, JsValue::from),
        "markText" => {
            let (from, to) = (at(0).ok_or_else(missing)?, at(1).ok_or_else(missing)?);
            let options = arg(2);
            if get(&options, "collapsed").is_truthy() {
                handle.with(|session| session.fold(from, to));
                object(&[
                    ("id", format!("fold:{}", from.line).into()),
                    ("collapsed", true.into()),
                    ("__isFold", true.into()),
                ])
            } else {
                let class = get(&options, "className").as_string().unwrap_or_default();
                let id = handle.with(|session| {
                    session.mark(Mark::Range {
                        from,
                        to,
                        class: class.clone(),
                    })
                });
                object(&[("id", id.to_string().into()), ("className", class.into())])
            }
        }
        "getAllMarks" => marks(handle),
        "clearMark" => {
            let id = arg(0).as_string().unwrap_or_default();
            if let Some(line) = fold_line(&id) {
                handle.with(|session| session.unfold(line));
            } else if let Ok(id) = id.parse() {
                handle.with(|session| session.clear_mark(id));
            }
            JsValue::UNDEFINED
        }
        "findMark" => {
            let id = arg(0).as_string().unwrap_or_default();
            let range = if let Some(line) = fold_line(&id) {
                handle.read(|session| {
                    session
                        .state()
                        .fold_starting(line)
                        .map(|fold| (fold.from, fold.to))
                })
            } else {
                id.parse()
                    .ok()
                    .and_then(|id| handle.read(|session| session.find_mark(id)))
            };
            range.map_or(JsValue::UNDEFINED, |(from, to)| {
                object(&[("from", position(from)), ("to", position(to))])
            })
        }
        "getOption" => options(handle, &arg(0).as_string().unwrap_or_default()),
        "setOption" => {
            match arg(0).as_string().as_deref() {
                Some("readOnly") => {
                    let readonly = arg(1).is_truthy();
                    handle.with(|session| session.set_readonly(readonly));
                }
                Some("servicesResponses") => {
                    let regions: Vec<crate::document::ResponseRegion> =
                        serde_json::from_value(crate::browser::json(arg(1))).unwrap_or_default();
                    handle.with(|session| session.set_responses(&regions));
                }
                _ => {}
            }
            JsValue::UNDEFINED
        }
        "on" | "off" => {
            let event = arg(0).as_string().unwrap_or_default();
            let handler: js_sys::Function = arg(1).dyn_into().map_err(|_| missing())?;
            let mut listeners = handle.0.listeners.borrow_mut();
            if name == "on" {
                listeners.push((event, handler));
            } else if let Some(index) = listeners
                .iter()
                .position(|(name, listener)| *name == event && *listener == handler)
            {
                listeners.remove(index);
            }
            JsValue::UNDEFINED
        }
        "startOperation" => {
            handle.0.batch.set(handle.0.batch.get() + 1);
            handle.0.session.borrow_mut().start_operation();
            JsValue::UNDEFINED
        }
        "endOperation" => {
            let depth = handle.0.batch.get().saturating_sub(1);
            handle.0.batch.set(depth);
            handle.0.session.borrow_mut().end_operation();
            if depth == 0 {
                handle.after();
            }
            JsValue::UNDEFINED
        }
        "pickHint" => {
            let index = number(&arg(0)).unwrap_or(0.0).max(0.0) as usize;
            handle.with(|session| session.dispatch(Msg::PickHint(index), now()));
            JsValue::UNDEFINED
        }
        "closeHint" => {
            if handle.read(|session| session.popup().is_some()) {
                handle.with(|session| session.dispatch(Msg::Command(Command::HintClose), now()));
            }
            JsValue::UNDEFINED
        }
        "execCommand" => {
            let name = arg(0).as_string().unwrap_or_default();
            let command = command(&name).ok_or_else(|| JsValue::from_str(&format!("Unknown command {name}")))?;
            // Commands run synchronously; callers read the result right away.
            handle.with(|session| session.dispatch(Msg::Command(command), now()));
            JsValue::UNDEFINED
        }
        "lineInfo" => {
            let line = number(&arg(0)).unwrap_or(0.0).max(0.0) as usize;
            let (text, marker) = handle.with(|session| {
                if line >= session.state().line_count() {
                    return (None, None);
                }
                let text = session.state().line(line).to_string();
                let marker = session.setup().console.then(|| session.fold_marker(line)).flatten();
                (Some(text), marker)
            });
            let Some(text) = text else {
                return Ok(JsValue::NULL);
            };
            let markers = marker.map_or(JsValue::NULL, |folded| {
                let class = if folded { "console-fold-folded" } else { "console-fold-open" };
                // Callers get the marker element itself; off-screen lines have none drawn.
                let drawn = handle.dom().and_then(|dom| {
                    let selector = format!(".console-fold-gutter[data-line=\"{line}\"] > .{class}");
                    dom.root.query_selector(&selector).ok().flatten()
                });
                let marker = drawn.map_or_else(|| object(&[("className", class.into())]), JsValue::from);
                object(&[("console-fold-gutter", marker)])
            });
            object(&[("line", line.into()), ("text", text.into()), ("gutterMarkers", markers)])
        }
        "setSize" => {
            if let Some(dom) = handle.dom() {
                let style = dom.root.style();
                let size = |value: JsValue| {
                    number(&value)
                        .map(|number| format!("{number}px"))
                        .or_else(|| value.as_string())
                };
                if let Some(width) = size(arg(0)) {
                    let _ = style.set_property("width", &width);
                }
                if let Some(height) = size(arg(1)) {
                    let _ = style.set_property("height", &height);
                }
            }
            JsValue::UNDEFINED
        }
        "refresh" => {
            handle.after();
            JsValue::UNDEFINED
        }
        "getMode" => options(handle, "mode"),
        "foldCode" => {
            let line = at(0)
                .map(|at| at.line)
                .or_else(|| number(&arg(0)).map(|line| line.max(0.0) as usize))
                .unwrap_or_else(|| cursor(handle, &JsValue::UNDEFINED).line);
            let force = arg(2).as_string();
            handle.with(|session| {
                let folded = session.state().fold_starting(line).is_some();
                let toggle = match force.as_deref() {
                    Some("fold") => !folded,
                    Some("unfold") => folded,
                    _ => true,
                };
                if toggle {
                    session.toggle_fold(line);
                }
            });
            JsValue::UNDEFINED
        }
        "foldRange" => {
            let line = at(0).ok_or_else(missing)?.line;
            match handle.with(|session| session.fold_range(line)) {
                Some(fold) => object(&[("from", position(fold.from)), ("to", position(fold.to))]),
                None => JsValue::UNDEFINED,
            }
        }
        "indexFromPos" => {
            let at = at(0).ok_or_else(missing)?;
            state(&|state| state.index(state.clip(at)).into())
        }
        "posFromIndex" => {
            let index = number(&arg(0)).unwrap_or(0.0).max(0.0) as usize;
            state(&|state| position(state.position(index)))
        }
        "state" => completion(handle),
        "servicesCompletion" => handle.read(|session| {
            let setup = session.setup();
            crate::browser::js(&serde_json::json!({
                "service": setup.service,
                "field": setup.field,
                "discovery": session.context().discovery,
            }))
        }),
        _ => return Err(JsValue::from_str(&format!("Unsupported editor method {name}"))),
    })
}
