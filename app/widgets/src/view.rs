//! A module's UI tree (`services_sdk::ui::Node`) as widgets; actions go back to the module as
//! `Ui` messages. Modules have no HTML or CSS of their own: only the widgets and their
//! variants (`primary`, `danger`, `muted`).
use crate::editor;
use crate::editor::extension;
use crate::editor::model::{Event as EditorEvent, Setup};
use crate::indicator::Indicator;
use services_sdk::ui::{self, Node, node::Kind};
use std::rc::Rc;
use wasm_bindgen::{JsCast, closure::Closure};
use web_sys::{Element, HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

pub const CSS: &str = include_str!("view.css");

/// A module message from the tree and the value that goes with it.
pub type Action = (Vec<u8>, Option<String>);

thread_local! {
    /// The editors on screen that complete hints, by id, with their language.
    static HINT_EDITORS: std::cell::RefCell<std::collections::BTreeMap<u64, (editor::Handle, AttrValue)>> =
        const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
    static NEXT_EDITOR: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Typing not yet told to its module (`on_input` waits), by field: module, msg, text and
    /// the timeout that tells it.
    static PENDING: std::cell::RefCell<std::collections::BTreeMap<u64, Pending>> =
        const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
    static NEXT_INPUT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// A field's typing not yet told: its module, msg, text and the timeout that tells it.
type Pending = (AttrValue, Vec<u8>, String, i32);

/// The module whose tree is rendered, for its fields' pending typing.
#[derive(Clone, PartialEq)]
struct ModuleId(AttrValue);

/// Takes the typing of `module`'s fields that `on_input` has not told it yet: the module
/// hears it before any other action of its view and before its panel closes.
pub fn take_pending(module: &str) -> Vec<Action> {
    PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        let slots: Vec<u64> = pending
            .iter()
            .filter(|(_, (owner, ..))| owner.as_str() == module)
            .map(|(slot, _)| *slot)
            .collect();
        slots
            .into_iter()
            .filter_map(|slot| pending.remove(&slot))
            .map(|(_, msg, value, timeout)| {
                crate::dom::window().clear_timeout_with_handle(timeout);
                (msg, Some(value))
            })
            .collect()
    })
}

/// The hints the editors' languages complete changed: the editors on screen follow.
pub fn hints_changed() {
    let editors: Vec<(editor::Handle, AttrValue)> =
        HINT_EDITORS.with(|editors| editors.borrow().values().cloned().collect());
    for (handle, language) in editors {
        set_hints(&handle, &language);
    }
}

/// Gives `handle` the hints of `language`; false when it has none.
fn set_hints(handle: &editor::Handle, language: &str) -> bool {
    let Some(hints) = extension::languages().hints(language) else {
        return false;
    };
    handle.set_context(crate::editor::model::Context {
        hints,
        ..Default::default()
    });
    true
}

/// The estimated height of a row of a virtual list, in CSS pixels.
const ROW: f64 = 26.0;
/// How long typing waits before `on_input`, in milliseconds.
const DEBOUNCE: i32 = 300;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub module: AttrValue,
    pub node: Rc<Node>,
    pub on: Callback<Action>,
}

/// The tree of one module.
#[function_component(ModuleView)]
pub fn module_view(props: &Props) -> Html {
    html! {
        <ContextProvider<ModuleId> context={ModuleId(props.module.clone())}>
            {render(&props.node, &props.on)}
        </ContextProvider<ModuleId>>
    }
}

fn send(on: &Callback<Action>, msg: &Option<Vec<u8>>, value: Option<String>) {
    if let Some(msg) = msg {
        on.emit((msg.clone(), value));
    }
}

fn classes(node: &Node, kind: &'static str) -> Classes {
    classes!(
        kind,
        (!node.variant.is_empty()).then(|| format!("is-{}", node.variant))
    )
}

fn key(node: &Node) -> Option<AttrValue> {
    (!node.key.is_empty()).then(|| AttrValue::from(node.key.clone()))
}

/// Children keyed for Yew only when every one has a distinct key.
fn children(nodes: &[Node], on: &Callback<Action>) -> Html {
    let mut seen = std::collections::BTreeSet::new();
    let keyed = nodes
        .iter()
        .all(|node| !node.key.is_empty() && seen.insert(node.key.as_str()));
    if keyed {
        nodes
            .iter()
            .map(|node| html! {<KeyedNode key={node.key.clone()} node={Rc::new(node.clone())} on={on.clone()}/>})
            .collect()
    } else {
        nodes.iter().map(|node| render(node, on)).collect()
    }
}

#[derive(Properties, PartialEq)]
struct KeyedProps {
    node: Rc<Node>,
    on: Callback<Action>,
}

#[function_component(KeyedNode)]
fn keyed_node(props: &KeyedProps) -> Html {
    render(&props.node, &props.on)
}

pub fn indicator_state(state: &str) -> Indicator {
    match state {
        "busy" => Indicator::Running,
        "ok" => Indicator::Success,
        "warning" => Indicator::Warning,
        "error" => Indicator::Error,
        _ => Indicator::Ready,
    }
}

pub fn render(node: &Node, on: &Callback<Action>) -> Html {
    let Some(kind) = &node.kind else {
        return Html::default();
    };
    let data_key = key(node);
    match kind {
        Kind::PanelFrame(frame) => html! {
            <section class={classes(node, "ui-panel")} data-key={data_key.clone()} data-testid={data_key}>
                <header class="ui-panel-title">
                    <span class="tab-button active">{frame.title.clone()}</span>
                </header>
                <div class="ui-panel-body">{children(&frame.body, on)}</div>
                if !frame.footer.is_empty() {
                    <footer class="ui-panel-footer">{children(&frame.footer, on)}</footer>
                }
            </section>
        },
        Kind::Tabs(tabs) => {
            let active = tabs
                .tabs
                .iter()
                .find(|tab| tab.key == tabs.active)
                .or(tabs.tabs.first());
            html! {
                <div class={classes(node, "ui-tabs")} data-key={data_key.clone()} data-testid={data_key}>
                    <div class="ui-tab-strip" role="tablist">
                        {for tabs.tabs.iter().map(|tab| {
                            let selected = active.is_some_and(|active| active.key == tab.key);
                            html! {<TabButton key={tab.key.clone()} tab={Rc::new(tab.clone())} {selected}
                                strip={node.key.clone()} on_select={tabs.on_select.clone()}
                                on_reorder={tabs.on_reorder.clone()} on={on.clone()}/>}
                        })}
                    </div>
                    if let Some(tab) = active {
                        <div class="ui-tab-body" role="tabpanel" data-key={format!("{}-body", tab.key)} data-testid={format!("{}-body", tab.key)}>
                            {children(&tab.body, on)}
                        </div>
                    }
                </div>
            }
        }
        Kind::List(list) => html! {
            <VirtualList key={node.key.clone()} list={Rc::new(list.clone())} variant={node.variant.clone()}
                data_key={data_key} on={on.clone()}/>
        },
        Kind::Table(table) => html! {
            <table class={classes(node, "ui-table")} data-key={data_key.clone()} data-testid={data_key}>
                if table.columns.iter().any(|column| !column.is_empty()) {
                    <thead><tr>{for table.columns.iter().map(|column| html! {<th>{column.clone()}</th>})}</tr></thead>
                }
                <tbody>
                    {for table.rows.iter().map(|row| html! {
                        <tr key={row.key.clone()} data-key={row.key.clone()} data-testid={row.key.clone()}>
                            {for row.cells.iter().map(|cell| html! {<td>{render(cell, on)}</td>})}
                        </tr>
                    })}
                </tbody>
            </table>
        },
        Kind::Row(row) => html! {
            <div class={classes(node, "ui-row")} data-key={data_key.clone()} data-testid={data_key}>{children(&row.children, on)}</div>
        },
        Kind::Column(column) => html! {
            <div class={classes(node, "ui-column")} data-key={data_key.clone()} data-testid={data_key}>{children(&column.children, on)}</div>
        },
        Kind::Text(text) => html! {
            <span class={classes(node, "ui-text")} data-key={data_key.clone()} data-testid={data_key}>{text.clone()}</span>
        },
        Kind::Markdown(text) => html! {
            <div class={classes(node, "ui-markdown")} data-key={data_key.clone()} data-testid={data_key}>{crate::markdown::render(text)}</div>
        },
        Kind::Tree(bytes) => {
            let value: serde_json::Value = serde_json::from_slice(bytes).unwrap_or_default();
            html! {
                <pre class={classes(node, "ui-tree")} data-key={data_key.clone()} data-testid={data_key}>
                    {serde_json::to_string_pretty(&value).unwrap_or_default()}
                </pre>
            }
        }
        Kind::Input(input) => html! {
            <TextInput input={input.clone()} variant={node.variant.clone()} data_key={data_key} on={on.clone()}/>
        },
        Kind::Select(select) => {
            let (on, msg) = (on.clone(), select.on_change.clone());
            html! {
                <select class={classes(node, "ui-select")} data-key={data_key.clone()} data-testid={data_key}
                    onchange={Callback::from(move |event: Event| {
                        let value = event.target_unchecked_into::<HtmlSelectElement>().value();
                        send(&on, &msg, Some(value));
                    })}>
                    {for select.options.iter().map(|choice| html! {
                        <option value={choice.value.clone()} selected={choice.value == select.value}>
                            {choice.label.clone()}</option>
                    })}
                </select>
            }
        }
        Kind::Checkbox(checkbox) => {
            let (on, msg) = (on.clone(), checkbox.on_change.clone());
            html! {
                <label class={classes(node, "ui-checkbox")} data-key={data_key.clone()} data-testid={data_key}>
                    <input type="checkbox" checked={checkbox.checked}
                        onchange={Callback::from(move |event: Event| {
                            let checked = event.target_unchecked_into::<HtmlInputElement>().checked();
                            send(&on, &msg, Some(checked.to_string()));
                        })}/>
                    {checkbox.label.clone()}
                </label>
            }
        }
        Kind::Slider(slider) => {
            let (on, msg) = (on.clone(), slider.on_change.clone());
            html! {
                <input type="range" class={classes(node, "ui-slider")} data-key={data_key.clone()} data-testid={data_key}
                    min={slider.min.to_string()} max={slider.max.to_string()}
                    step={slider.step.to_string()} value={slider.value.to_string()}
                    onchange={Callback::from(move |event: Event| {
                        let value = event.target_unchecked_into::<HtmlInputElement>().value();
                        send(&on, &msg, Some(value));
                    })}/>
            }
        }
        Kind::Button(button) => {
            let (on, msg) = (on.clone(), button.on_click.clone());
            html! {
                <button type="button" class={classes(node, "ui-button")} data-key={data_key.clone()} data-testid={data_key}
                    disabled={button.disabled}
                    onclick={Callback::from(move |_| send(&on, &msg, None))}>
                    {button.label.clone()}</button>
            }
        }
        Kind::Indicator(indicator) => {
            let state = indicator_state(&indicator.state);
            html! {
                <span class={classes(node, "ui-indicator")} data-key={data_key.clone()} data-testid={data_key} data-state={indicator.state.clone()}>
                    {crate::indicator::indicator(state, indicator.label.clone())}
                    if !indicator.label.is_empty() {
                        <span class="ui-indicator-label">{indicator.label.clone()}</span>
                    }
                </span>
            }
        }
        Kind::Editor(editor) => html! {
            // A new language is a new editor: its setup is fixed at creation.
            <TextEditor key={format!("{}\u{1f}{}", node.key, editor.language)} editor={editor.clone()} data_key={data_key} on={on.clone()}/>
        },
    }
}

thread_local! {
    /// The key of the tab being dragged.
    static DRAGGED: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[derive(Properties, PartialEq)]
struct TabProps {
    tab: Rc<ui::Tab>,
    selected: bool,
    /// The key of the tab strip: the rename field is `<strip>-rename`.
    strip: String,
    on_select: Option<Vec<u8>>,
    on_reorder: Option<Vec<u8>>,
    on: Callback<Action>,
}

/// A tab of a strip: it may be renamed in place, dragged onto another and carry a light.
#[function_component(TabButton)]
fn tab_button(props: &TabProps) -> Html {
    let tab = &props.tab;
    let renaming = use_state_eq(|| None::<String>);
    let field = use_node_ref();
    // Escape cancels; the blur that follows must not commit.
    let cancelled = use_mut_ref(|| false);
    {
        let field = field.clone();
        use_effect_with(renaming.is_some(), move |renaming| {
            if *renaming && let Some(element) = field.cast::<HtmlInputElement>() {
                let _ = element.focus();
                element.select();
            }
        });
    }
    let onclick = {
        let (on, msg, value) = (props.on.clone(), props.on_select.clone(), tab.key.clone());
        Callback::from(move |_| send(&on, &msg, Some(value.clone())))
    };
    let ondblclick = {
        let (renaming, label, cancelled) = (renaming.clone(), tab.label.clone(), cancelled.clone());
        let enabled = tab.on_rename.is_some();
        Callback::from(move |_| {
            if enabled {
                *cancelled.borrow_mut() = false;
                renaming.set(Some(label.clone()));
            }
        })
    };
    let commit = {
        let (on, msg, label, renaming, cancelled) = (
            props.on.clone(),
            tab.on_rename.clone(),
            tab.label.clone(),
            renaming.clone(),
            cancelled.clone(),
        );
        move |value: String| {
            if std::mem::take(&mut *cancelled.borrow_mut()) || renaming.is_none() {
                return;
            }
            renaming.set(None);
            if value != label {
                send(&on, &msg, Some(value));
            }
        }
    };
    let onkeydown = {
        let (commit, renaming, cancelled) = (commit.clone(), renaming.clone(), cancelled.clone());
        Callback::from(move |event: KeyboardEvent| {
            event.stop_propagation();
            match event.key().as_str() {
                "Enter" => {
                    event.prevent_default();
                    commit(event.target_unchecked_into::<HtmlInputElement>().value());
                }
                "Escape" => {
                    event.prevent_default();
                    *cancelled.borrow_mut() = true;
                    renaming.set(None);
                }
                _ => {}
            }
        })
    };
    // The typed text lives in the state: a module re-render must not reset it.
    let oninput = {
        let renaming = renaming.clone();
        Callback::from(move |event: InputEvent| {
            renaming.set(Some(
                event.target_unchecked_into::<HtmlInputElement>().value(),
            ))
        })
    };
    let onblur = Callback::from(move |event: FocusEvent| {
        commit(event.target_unchecked_into::<HtmlInputElement>().value())
    });
    let draggable = props.on_reorder.is_some() && renaming.is_none();
    let ondragstart = {
        let key = tab.key.clone();
        Callback::from(move |_: DragEvent| {
            DRAGGED.with(|dragged| *dragged.borrow_mut() = Some(key.clone()))
        })
    };
    let ondrop = {
        let (on, msg, key) = (props.on.clone(), props.on_reorder.clone(), tab.key.clone());
        Callback::from(move |event: DragEvent| {
            event.prevent_default();
            if let Some(from) = DRAGGED.with(|dragged| dragged.borrow_mut().take())
                && from != key
            {
                send(&on, &msg, Some(serde_json::json!([from, key]).to_string()));
            }
        })
    };
    html! {
        <button type="button" role="tab" data-key={tab.key.clone()} data-testid={tab.key.clone()}
            aria-selected={props.selected.to_string()}
            class={classes!("tab-button", props.selected.then_some("active"))}
            draggable={draggable.then_some("true")} {onclick} {ondblclick} {ondragstart}
            ondragover={Callback::from(|event: DragEvent| event.prevent_default())} {ondrop}>
            if let Some(value) = &*renaming {
                <input ref={field} class="tab-label-input" data-testid={format!("{}-rename", props.strip)}
                    value={value.clone()} onclick={Callback::from(|event: MouseEvent| event.stop_propagation())}
                    ondblclick={Callback::from(|event: MouseEvent| event.stop_propagation())}
                    {oninput} {onkeydown} {onblur}/>
            } else {
                <span class="tab-label">{tab.label.clone()}</span>
            }
            if !tab.indicator.is_empty() {
                {crate::indicator::indicator(indicator_state(&tab.indicator), format!("{} · {}", tab.label, tab.indicator))}
            }
        </button>
    }
}

#[derive(Properties, PartialEq)]
struct ListProps {
    list: Rc<ui::List>,
    variant: String,
    data_key: Option<AttrValue>,
    on: Callback<Action>,
}

/// The visible window of a long list; scrolling asks the module for another window.
#[function_component(VirtualList)]
fn virtual_list(props: &ListProps) -> Html {
    let list = &props.list;
    let before = f64::from(list.start) * ROW;
    let shown = list.rows.len() as u32;
    let after = f64::from(list.total.saturating_sub(list.start + shown)) * ROW;
    let onscroll = {
        let (on, msg, start, total) = (
            props.on.clone(),
            list.on_range.clone(),
            list.start,
            list.total,
        );
        Callback::from(move |event: Event| {
            let element = event.target_unchecked_into::<Element>();
            let first = (f64::from(element.scroll_top()) / ROW).floor() as u32;
            let count = (f64::from(element.client_height()) / ROW).ceil() as u32 + 1;
            let last = (first + count).min(total);
            if first < start || last > start + shown {
                let from = first.saturating_sub(count);
                send(
                    &on,
                    &msg,
                    Some(format!("{from}:{}", (last + count).min(total))),
                );
            }
        })
    };
    let class = classes!(
        "ui-list",
        (!props.variant.is_empty()).then(|| format!("is-{}", props.variant))
    );
    html! {
        <div {class} data-key={props.data_key.clone()} data-testid={props.data_key.clone()} data-total={list.total.to_string()} {onscroll}>
            if before > 0.0 {<div class="ui-list-space" style={format!("height:{before}px")}/>}
            {children(&list.rows, &props.on)}
            if after > 0.0 && list.on_range.is_some() {<div class="ui-list-space" style={format!("height:{after}px")}/>}
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct InputProps {
    input: ui::Input,
    variant: String,
    data_key: Option<AttrValue>,
    on: Callback<Action>,
}

/// A text field that keeps what is typed; the module sets its text only when it changes it.
#[function_component(TextInput)]
fn text_input(props: &InputProps) -> Html {
    let field = use_node_ref();
    let timer = use_mut_ref(|| None::<(i32, Closure<dyn FnMut()>)>);
    // The text the module was last told; a module that has seen it and answers with another
    // text changes the field even when its text equals the one it set before.
    let told = use_mut_ref(|| None::<String>);
    let apply = {
        let field = field.clone();
        move |value: &str, force: bool, told: Option<&str>| {
            let current = field
                .cast::<HtmlInputElement>()
                .map(|element| element.value())
                .or_else(|| {
                    field
                        .cast::<web_sys::HtmlTextAreaElement>()
                        .map(|element| element.value())
                });
            let Some(current) = current else { return };
            if current == value || !(force || told == Some(current.as_str())) {
                return;
            }
            if let Some(element) = field.cast::<HtmlInputElement>() {
                element.set_value(value);
            } else if let Some(element) = field.cast::<web_sys::HtmlTextAreaElement>() {
                element.set_value(value);
            }
        }
    };
    {
        let apply = apply.clone();
        use_effect_with(props.input.value.clone(), move |value| {
            if let Some(value) = value {
                apply(value, true, None);
            }
        });
    }
    {
        let (value, told) = (props.input.value.clone(), told.clone());
        use_effect(move || {
            if let Some(value) = value {
                apply(&value, false, told.borrow().as_deref());
            }
        });
    }
    let module = use_context::<ModuleId>().map(|id| id.0).unwrap_or_default();
    let slot = *use_state(|| NEXT_INPUT.with(|next| next.replace(next.get() + 1)));
    {
        let timer = timer.clone();
        use_effect_with((), move |()| {
            move || {
                if let Some((id, _)) = timer.borrow_mut().take() {
                    crate::dom::window().clear_timeout_with_handle(id);
                }
                PENDING.with(|pending| pending.borrow_mut().remove(&slot));
            }
        });
    }
    let oninput = {
        let (on, msg, timer, told) = (
            props.on.clone(),
            props.input.on_input.clone(),
            timer.clone(),
            told.clone(),
        );
        Callback::from(move |event: InputEvent| {
            if msg.is_none() {
                return;
            }
            let value = event.target_unchecked_into::<HtmlInputElement>().value();
            if let Some((id, _)) = timer.borrow_mut().take() {
                crate::dom::window().clear_timeout_with_handle(id);
            }
            let pending = value.clone();
            let callback = {
                let (on, msg, cell, told) = (on.clone(), msg.clone(), timer.clone(), told.clone());
                Closure::<dyn FnMut()>::new(move || {
                    PENDING.with(|pending| pending.borrow_mut().remove(&slot));
                    cell.borrow_mut().take();
                    *told.borrow_mut() = Some(value.clone());
                    send(&on, &msg, Some(value.clone()));
                })
            };
            if let Ok(id) = crate::dom::window()
                .set_timeout_with_callback_and_timeout_and_arguments_0(
                    callback.as_ref().unchecked_ref(),
                    DEBOUNCE,
                )
            {
                if let Some(input) = msg.clone() {
                    PENDING.with(|slots| {
                        slots
                            .borrow_mut()
                            .insert(slot, (module.clone(), input, pending, id))
                    });
                }
                *timer.borrow_mut() = Some((id, callback));
            }
        })
    };
    let changed = use_mut_ref(|| None::<String>);
    let change = {
        let (on, input, msg, timer, told, changed) = (
            props.on.clone(),
            props.input.on_input.clone(),
            props.input.on_change.clone(),
            timer.clone(),
            told.clone(),
            changed.clone(),
        );
        move |value: String| {
            *told.borrow_mut() = Some(value.clone());
            *changed.borrow_mut() = Some(value.clone());
            // A pending `on_input` goes first, so the module sees the text before the change.
            if let Some((id, _)) = timer.borrow_mut().take() {
                crate::dom::window().clear_timeout_with_handle(id);
                PENDING.with(|pending| pending.borrow_mut().remove(&slot));
                send(&on, &input, Some(value.clone()));
            }
            send(&on, &msg, Some(value));
        }
    };
    let onchange = {
        let change = change.clone();
        Callback::from(move |event: Event| {
            change(event.target_unchecked_into::<HtmlInputElement>().value())
        })
    };
    // Leaving a field commits a text the module has not seen as a change, even one that
    // was set without typing (the browser's `change` misses it).
    let onblur = {
        let (change, module) = (change.clone(), props.input.value.clone());
        Callback::from(move |event: FocusEvent| {
            let value = event.target_unchecked_into::<HtmlInputElement>().value();
            if changed.borrow().as_deref() != Some(value.as_str())
                && module.as_deref() != Some(value.as_str())
            {
                change(value);
            }
        })
    };
    let multiline = props.variant == "multiline";
    let onkeydown = Callback::from(move |event: KeyboardEvent| {
        if event.key() != "Enter" || event.is_composing() {
            return;
        }
        // A multiline field sends with Enter and breaks lines with Shift+Enter.
        if multiline {
            if event.shift_key() || event.ctrl_key() || event.alt_key() || event.meta_key() {
                return;
            }
            event.prevent_default();
            event.stop_propagation();
            if event.repeat() {
                return;
            }
        }
        change(event.target_unchecked_into::<HtmlInputElement>().value());
    });
    let class = classes!(
        "ui-input",
        (!props.variant.is_empty()).then(|| format!("is-{}", props.variant))
    );
    let placeholder = props.input.placeholder.clone();
    // The text is set by the effect above only: a field keeps what is typed until the module
    // sends another text.
    if multiline {
        return html! {
            <textarea ref={field} {class} data-key={props.data_key.clone()} data-testid={props.data_key.clone()}
                aria-label={placeholder.clone()} aria-keyshortcuts="Enter" {placeholder} {oninput} {onkeydown}/>
        };
    }
    // A secret field hides what is typed.
    let kind = if props.variant == "secret" {
        "password"
    } else {
        "text"
    };
    html! {
        <input ref={field} type={kind} {class} data-key={props.data_key.clone()} data-testid={props.data_key.clone()}
            autocomplete="off" spellcheck="false" {placeholder}
            {oninput} {onchange} {onblur} {onkeydown}/>
    }
}

#[derive(Properties, PartialEq)]
struct EditorProps {
    editor: ui::Editor,
    data_key: Option<AttrValue>,
    on: Callback<Action>,
}

/// An editor of `language`: what the installed [`extension::Languages`] makes of it, a
/// highlighting mode by default.
#[function_component(TextEditor)]
fn text_editor(props: &EditorProps) -> Html {
    let readonly = props.editor.on_change.is_none();
    let handle = {
        let (language, text) = (props.editor.language.clone(), props.editor.text.clone());
        use_state(move || {
            let (mode, extension) = extension::languages().editor(&language);
            let setup = Setup {
                mode,
                readonly,
                ..Setup::default()
            };
            editor::Handle::with_extension(setup, extension, &text)
        })
    };
    {
        let handle = (*handle).clone();
        let language = AttrValue::from(props.editor.language.clone());
        use_effect_with((), move |()| {
            let id = NEXT_EDITOR.with(|next| {
                next.set(next.get() + 1);
                next.get()
            });
            if set_hints(&handle, &language) {
                HINT_EDITORS.with(|editors| editors.borrow_mut().insert(id, (handle, language)));
            }
            move || {
                HINT_EDITORS.with(|editors| editors.borrow_mut().remove(&id));
            }
        });
    }
    {
        let handle = (*handle).clone();
        use_effect_with(
            (props.editor.text.clone(), readonly),
            move |(text, readonly)| {
                handle.set_readonly(*readonly);
                handle.set_text(text);
            },
        );
    }
    let on = {
        let (handle, on, msg) = (
            (*handle).clone(),
            props.on.clone(),
            props.editor.on_change.clone(),
        );
        Callback::from(move |message| {
            for event in handle.update(message) {
                if let EditorEvent::Changed(text) = event {
                    send(&on, &msg, Some(text));
                }
            }
        })
    };
    let testid = props.data_key.clone().unwrap_or_default();
    html! {
        <div class="ui-editor" data-key={props.data_key.clone()}>
            <editor::View handle={(*handle).clone()} {on} {testid} label={testid_label(&props.editor.language)}/>
        </div>
    }
}

fn testid_label(language: &str) -> AttrValue {
    AttrValue::from(format!("Editor · {language}"))
}
