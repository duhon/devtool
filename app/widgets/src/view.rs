//! A module's UI tree (`services_sdk::ui::Node`) as widgets; actions go back to the module as
//! `Ui` messages. Modules have no HTML or CSS of their own: only the widgets and their
//! variants (`primary`, `danger`, `muted`, `link`, `heading`, `badge`, …; several may be given,
//! separated by spaces).
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

/// `is-<name>` for each space-separated name of a variant.
fn variants(variant: &str) -> Classes {
    variant
        .split_whitespace()
        .map(|name| format!("is-{name}"))
        .collect()
}

fn classes(node: &Node, kind: &'static str) -> Classes {
    classes!(kind, variants(&node.variant))
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

fn title(node: &Node) -> Option<AttrValue> {
    (!node.title.is_empty()).then(|| AttrValue::from(node.title.clone()))
}

fn aria_label(node: &Node) -> Option<AttrValue> {
    (!node.label.is_empty()).then(|| AttrValue::from(node.label.clone()))
}

/// Only web addresses are opened from a module's tree.
fn web_address(url: &str) -> bool {
    let url = url.trim().to_ascii_lowercase();
    url.starts_with("https://") || url.starts_with("http://")
}

/// The tab buttons of a strip and its `+`.
fn strip_items(
    key: &str,
    tabs: &ui::Tabs,
    active: Option<&ui::Tab>,
    on: &Callback<Action>,
) -> Html {
    html! {<>
        {for tabs.tabs.iter().map(|tab| {
            let selected = active.is_some_and(|active| active.key == tab.key);
            html! {<TabButton key={tab.key.clone()} tab={Rc::new(tab.clone())} {selected}
                strip={key.to_owned()} on_select={tabs.on_select.clone()}
                on_reorder={tabs.on_reorder.clone()} on={on.clone()}/>}
        })}
        {add_button(key, tabs, on)}
    </>}
}

/// The `+` after the last tab: it asks `add_prompt` first when there is one.
fn add_button(key: &str, tabs: &ui::Tabs, on: &Callback<Action>) -> Html {
    let Some(msg) = tabs.on_add.clone() else {
        return Html::default();
    };
    let (on, prompt) = (on.clone(), tabs.add_prompt.clone());
    let testid = if tabs.add_key.is_empty() {
        format!("{key}-add")
    } else {
        tabs.add_key.clone()
    };
    let title = if tabs.add_title.is_empty() {
        "Add".to_owned()
    } else {
        tabs.add_title.clone()
    };
    let onclick = Callback::from(move |_| {
        if prompt.is_empty() {
            on.emit((msg.clone(), None));
        } else if let Ok(Some(answer)) = crate::dom::window().prompt_with_message(&prompt) {
            let answer = answer.trim();
            if !answer.is_empty() {
                on.emit((msg.clone(), Some(answer.to_owned())));
            }
        }
    });
    html! {
        <button type="button" class="tab-button ui-tab-add" data-testid={testid}
            aria-label={title.clone()} {title} {onclick}>{"+"}</button>
    }
}

pub fn render(node: &Node, on: &Callback<Action>) -> Html {
    let Some(kind) = &node.kind else {
        return Html::default();
    };
    let data_key = key(node);
    let (tip, aria) = (title(node), aria_label(node));
    match kind {
        Kind::PanelFrame(frame) => {
            let (header, tab) = panel_header(frame, on);
            html! {
                <section class={classes(node, "ui-panel")} data-key={data_key.clone()} data-testid={data_key}>
                    <header class="ui-panel-title">
                        {header}
                        if !frame.actions.is_empty() {
                            <div class="ui-panel-actions">{children(&frame.actions, on)}</div>
                        }
                    </header>
                    <div class="ui-panel-body">
                        if let Some(tab) = tab {
                            <div class="ui-tab-body" role="tabpanel" data-key={format!("{}-body", tab.key)} data-testid={format!("{}-body", tab.key)}>
                                {children(&tab.body, on)}
                            </div>
                        }
                        {children(&frame.body, on)}
                    </div>
                    if !frame.footer.is_empty() {
                        <footer class="ui-panel-footer">{children(&frame.footer, on)}</footer>
                    }
                </section>
            }
        }
        Kind::Tabs(tabs) => {
            let active = tabs
                .tabs
                .iter()
                .find(|tab| tab.key == tabs.active)
                .or(tabs.tabs.first());
            html! {
                <div class={classes(node, "ui-tabs")} data-key={data_key.clone()} data-testid={data_key}>
                    <div class="ui-tab-strip" role="tablist">
                        {strip_items(&node.key, tabs, active, on)}
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
                data_key={data_key} label={aria} on={on.clone()}/>
        },
        Kind::Table(table) => {
            let span = table
                .rows
                .iter()
                .map(|row| row.cells.len())
                .chain([table.columns.len()])
                .max()
                .unwrap_or(1)
                .max(1);
            // A column of width 0 fits its content; the others share the rest and clip.
            let fit = table.widths.contains(&0) && table.widths.iter().any(|width| *width > 0);
            let clipped: &[u32] = if fit { &table.widths } else { &[] };
            html! {
                <table class={classes!(classes(node, "ui-table"),
                    (!table.widths.is_empty() && !fit).then_some("is-fixed"), fit.then_some("is-fit"))}
                    data-key={data_key.clone()} data-testid={data_key} title={tip} aria-label={aria}>
                    if !table.widths.is_empty() {
                        <colgroup>
                            {for table.widths.iter().map(|width| html! {
                                <col style={(*width > 0).then(|| format!("width:{width}%"))}/>
                            })}
                        </colgroup>
                    }
                    if table.columns.iter().any(|column| !column.is_empty()) {
                        <thead><tr>{for table.columns.iter().map(|column| html! {<th>{column.clone()}</th>})}</tr></thead>
                    }
                    <tbody>
                        {for table.rows.iter().map(|row| table_row(row, span, clipped, on))}
                    </tbody>
                </table>
            }
        }
        Kind::Row(row) => html! {
            <div class={classes(node, "ui-row")} data-key={data_key.clone()} data-testid={data_key} title={tip} aria-label={aria}>{children(&row.children, on)}</div>
        },
        Kind::Column(column) => html! {
            <div class={classes(node, "ui-column")} data-key={data_key.clone()} data-testid={data_key} title={tip} aria-label={aria}>{children(&column.children, on)}</div>
        },
        Kind::Text(text) => html! {
            <span class={classes(node, "ui-text")} data-key={data_key.clone()} data-testid={data_key} title={tip} aria-label={aria}>{text.clone()}</span>
        },
        Kind::Markdown(text) => html! {
            <div class={classes(node, "ui-markdown")} data-key={data_key.clone()} data-testid={data_key} title={tip}>{crate::markdown::render(text)}</div>
        },
        Kind::Tree(bytes) => {
            let value: serde_json::Value = serde_json::from_slice(bytes).unwrap_or_default();
            html! {
                <pre class={classes(node, "ui-tree")} data-key={data_key.clone()} data-testid={data_key} title={tip}>
                    {serde_json::to_string_pretty(&value).unwrap_or_default()}
                </pre>
            }
        }
        Kind::Input(input) => html! {
            <TextInput input={input.clone()} variant={node.variant.clone()} data_key={data_key}
                title={tip} label={aria} on={on.clone()}/>
        },
        Kind::Select(select) => {
            let (on, msg) = (on.clone(), select.on_change.clone());
            html! {
                <select class={classes(node, "ui-select")} data-key={data_key.clone()} data-testid={data_key}
                    title={tip} aria-label={aria}
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
        Kind::Combobox(combobox) => html! {
            <Combobox combobox={combobox.clone()} variant={node.variant.clone()} data_key={data_key}
                title={tip} label={aria} on={on.clone()}/>
        },
        Kind::Checkbox(checkbox) => {
            let (on, msg) = (on.clone(), checkbox.on_change.clone());
            html! {
                <label class={classes(node, "ui-checkbox")} data-key={data_key.clone()} data-testid={data_key} title={tip}>
                    <input type="checkbox" checked={checkbox.checked}
                        onchange={Callback::from(move |event: Event| {
                            let checked = event.target_unchecked_into::<HtmlInputElement>().checked();
                            send(&on, &msg, Some(checked.to_string()));
                        })}/>
                    {checkbox.label.clone()}
                </label>
            }
        }
        Kind::Slider(slider) => html! {
            <Slider slider={slider.clone()} variant={node.variant.clone()} data_key={data_key}
                title={tip} label={aria} on={on.clone()}/>
        },
        Kind::Button(button) => {
            let (on, msg, confirm) = (on.clone(), button.on_click.clone(), button.confirm.clone());
            let onclick = Callback::from(move |_| {
                if confirm.is_empty()
                    || crate::dom::window()
                        .confirm_with_message(&confirm)
                        .unwrap_or(false)
                {
                    send(&on, &msg, None);
                }
            });
            html! {
                <button type="button" class={classes(node, "ui-button")} data-key={data_key.clone()} data-testid={data_key}
                    title={tip} aria-label={aria} disabled={button.disabled}
                    {onclick}>
                    {button.label.clone()}</button>
            }
        }
        Kind::Anchor(anchor) if web_address(&anchor.url) => html! {
            <a class={classes(node, "ui-link")} data-key={data_key.clone()} data-testid={data_key}
                href={anchor.url.trim().to_owned()} target="_blank" rel="noopener noreferrer" title={tip}>
                {anchor.label.clone()}</a>
        },
        Kind::Anchor(anchor) => html! {
            <span class={classes(node, "ui-text")} data-key={data_key.clone()} data-testid={data_key} title={tip}>
                {anchor.label.clone()}</span>
        },
        Kind::Field(field) => html! {
            <label class={classes(node, "ui-field")} data-key={data_key.clone()} data-testid={data_key} title={tip}>
                <span class="ui-field-label">{field.label.clone()}</span>
                {children(&field.control, on)}
            </label>
        },
        Kind::Indicator(indicator) => {
            let state = indicator_state(&indicator.state);
            html! {
                <span class={classes(node, "ui-indicator")} data-key={data_key.clone()} data-testid={data_key} data-state={indicator.state.clone()} title={tip}>
                    {crate::indicator::indicator(state, indicator.label.clone())}
                    if !indicator.label.is_empty() {
                        <span class="ui-indicator-label">{indicator.label.clone()}</span>
                    }
                </span>
            }
        }
        Kind::Editor(editor) => html! {
            // A new language is a new editor: its setup is fixed at creation.
            <TextEditor key={format!("{}\u{1f}{}", node.key, editor.language)} editor={editor.clone()}
                variant={node.variant.clone()} data_key={data_key} label={aria} on={on.clone()}/>
        },
    }
}

/// A row of a table, then its detail across the width of the table.
fn table_row(row: &ui::TableRow, span: usize, clipped: &[u32], on: &Callback<Action>) -> Html {
    let onclick = row.on_click.clone().map(|msg| {
        let on = on.clone();
        Callback::from(move |event: MouseEvent| {
            // Buttons, fields and links in the row do their own thing.
            let inner = event
                .target_dyn_into::<web_sys::Element>()
                .and_then(|target| {
                    target
                        .closest("button, input, select, textarea, a, label")
                        .ok()
                        .flatten()
                });
            if inner.is_none() {
                send(&on, &Some(msg.clone()), None);
            }
        })
    });
    let class = classes!(
        variants(&row.variant),
        onclick.is_some().then_some("is-clickable")
    );
    let mut rows = vec![html! {
        <tr key={row.key.clone()} {class} {onclick} data-key={row.key.clone()} data-testid={row.key.clone()}>
            {for row.cells.iter().enumerate().map(|(index, cell)| html! {
                <td class={clipped.get(index).is_some_and(|width| *width > 0).then_some("is-clipped")}>{render(cell, on)}</td>
            })}
        </tr>
    }];
    if !row.detail.is_empty() {
        let key = format!("{}-detail", row.key);
        rows.push(html! {
            <tr key={key.clone()} class="ui-table-detail" data-key={key.clone()} data-testid={key}>
                <td colspan={span.to_string()}>{children(&row.detail, on)}</td>
            </tr>
        });
    }
    Html::from(yew::virtual_dom::VList::with_children(
        rows,
        Some(row.key.clone().into()),
    ))
}

/// A panel's header row: its title, or its own tabs and the active one (none when no tab has
/// the active key).
fn panel_header<'a>(
    frame: &'a ui::PanelFrame,
    on: &Callback<Action>,
) -> (Html, Option<&'a ui::Tab>) {
    use ui::panel_frame::Header;
    match &frame.header {
        Some(Header::Tabs(node)) => {
            let Some(Kind::Tabs(tabs)) = &node.kind else {
                return (render(node, on), None);
            };
            let active = tabs.tabs.iter().find(|tab| tab.key == tabs.active);
            let data_key = key(node);
            let strip = html! {
                <div class={classes(node, "ui-panel-tabs")} role="tablist" data-key={data_key.clone()} data-testid={data_key}>
                    {strip_items(&node.key, tabs, active, on)}
                </div>
            };
            (strip, active)
        }
        Some(Header::Title(title)) => (
            html! {
                <span class="tab-button active">
                    {title.clone()}
                    if !frame.indicator.is_empty() {
                        {crate::indicator::indicator(indicator_state(&frame.indicator), format!("{title} · {}", frame.indicator))}
                    }
                </span>
            },
            None,
        ),
        None => (Html::default(), None),
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
    let close = tab
        .on_close
        .clone()
        .filter(|_| renaming.is_none())
        .map(|msg| {
            let (on, key, confirm) = (props.on.clone(), tab.key.clone(), tab.confirm.clone());
            let ask = move || {
                confirm.is_empty()
                    || crate::dom::window()
                        .confirm_with_message(&confirm)
                        .unwrap_or(false)
            };
            let run = {
                let ask = ask.clone();
                move || {
                    if ask() {
                        on.emit((msg.clone(), Some(key.clone())));
                    }
                }
            };
            let onclick = {
                let run = run.clone();
                Callback::from(move |event: MouseEvent| {
                    event.stop_propagation();
                    run();
                })
            };
            let onkeydown = Callback::from(move |event: KeyboardEvent| {
                if matches!(event.key().as_str(), "Enter" | " ") {
                    event.prevent_default();
                    event.stop_propagation();
                    run();
                }
            });
            let testid = if tab.close_key.is_empty() {
                format!("{}-close-{}", props.strip, tab.key)
            } else {
                tab.close_key.clone()
            };
            html! {
                <span class="tab-close" role="button" tabindex="0" title="Close"
                    aria-label={format!("Close {}", tab.label)} data-testid={testid}
                    {onclick} {onkeydown}>{"×"}</span>
            }
        });
    let tip = (!tab.title.is_empty()).then(|| AttrValue::from(tab.title.clone()));
    html! {
        <button type="button" role={(!tab.plain).then_some("tab")} data-key={tab.key.clone()} data-testid={tab.key.clone()}
            aria-selected={(!tab.plain).then(|| props.selected.to_string())} title={tip}
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
            {close}
        </button>
    }
}

#[derive(Properties, PartialEq)]
struct ListProps {
    list: Rc<ui::List>,
    variant: String,
    data_key: Option<AttrValue>,
    label: Option<AttrValue>,
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
    let class = classes!("ui-list", variants(&props.variant));
    let node = use_node_ref();
    // The row that becomes the primary one scrolls into view, once.
    let primary = list
        .rows
        .iter()
        .find(|row| row.variant.split_whitespace().any(|name| name == "primary"))
        .map(|row| row.key.clone());
    {
        let node = node.clone();
        use_effect_with(primary, move |primary| {
            if primary.is_some()
                && let Some(list) = node.cast::<Element>()
                && let Ok(Some(row)) = list.query_selector(".is-primary")
            {
                let options = web_sys::ScrollIntoViewOptions::new();
                options.set_block(web_sys::ScrollLogicalPosition::Nearest);
                row.scroll_into_view_with_scroll_into_view_options(&options);
            }
        });
    }
    html! {
        <div ref={node} {class} data-key={props.data_key.clone()} data-testid={props.data_key.clone()} aria-label={props.label.clone()} data-total={list.total.to_string()} {onscroll}>
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
    title: Option<AttrValue>,
    label: Option<AttrValue>,
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
    let class = classes!("ui-input", variants(&props.variant));
    let placeholder = props.input.placeholder.clone();
    let aria = props
        .label
        .clone()
        .or_else(|| (!placeholder.is_empty()).then(|| AttrValue::from(placeholder.clone())));
    let title = props.title.clone();
    let size = (props.input.size > 0).then(|| props.input.size.to_string());
    // The text is set by the effect above only: a field keeps what is typed until the module
    // sends another text.
    if multiline {
        return html! {
            <textarea ref={field} {class} data-key={props.data_key.clone()} data-testid={props.data_key.clone()}
                aria-label={aria} {title} aria-keyshortcuts="Enter" {placeholder} {oninput} {onkeydown}/>
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
            autocomplete="off" spellcheck="false" {placeholder} aria-label={aria} {title} {size}
            {oninput} {onchange} {onblur} {onkeydown}/>
    }
}

#[derive(Properties, PartialEq)]
struct EditorProps {
    editor: ui::Editor,
    variant: String,
    data_key: Option<AttrValue>,
    label: Option<AttrValue>,
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
    let module = use_context::<ModuleId>().map(|id| id.0).unwrap_or_default();
    let slot = *use_state(|| NEXT_INPUT.with(|next| next.replace(next.get() + 1)));
    let timer = use_mut_ref(|| None::<(i32, Closure<dyn FnMut()>)>);
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
    let on = {
        let (handle, on, msg, wait) = (
            (*handle).clone(),
            props.on.clone(),
            props.editor.on_change.clone(),
            props.editor.debounce_ms,
        );
        Callback::from(move |message| {
            for event in handle.update(message) {
                let EditorEvent::Changed(text) = event else {
                    continue;
                };
                let Some(input) = msg.clone().filter(|_| wait > 0) else {
                    send(&on, &msg, Some(text));
                    continue;
                };
                if let Some((id, _)) = timer.borrow_mut().take() {
                    crate::dom::window().clear_timeout_with_handle(id);
                }
                let callback = {
                    let (on, input, cell, text) =
                        (on.clone(), input.clone(), timer.clone(), text.clone());
                    Closure::<dyn FnMut()>::new(move || {
                        PENDING.with(|pending| pending.borrow_mut().remove(&slot));
                        cell.borrow_mut().take();
                        on.emit((input.clone(), Some(text.clone())));
                    })
                };
                if let Ok(id) = crate::dom::window()
                    .set_timeout_with_callback_and_timeout_and_arguments_0(
                        callback.as_ref().unchecked_ref(),
                        wait as i32,
                    )
                {
                    PENDING.with(|slots| {
                        slots
                            .borrow_mut()
                            .insert(slot, (module.clone(), input, text, id))
                    });
                    *timer.borrow_mut() = Some((id, callback));
                }
            }
        })
    };
    let testid = props.data_key.clone().unwrap_or_default();
    let label = props
        .label
        .clone()
        .unwrap_or_else(|| testid_label(&props.editor.language));
    // An editor fills its panel or tab edge to edge; a viewer does so only when asked to.
    let flush = !readonly || props.variant.split_whitespace().any(|name| name == "flush");
    html! {
        <div class={classes!("ui-editor", readonly.then_some("is-viewer"), flush.then_some("is-flush"), variants(&props.variant))}
            data-key={props.data_key.clone()}>
            <editor::View handle={(*handle).clone()} {on} {testid} {label}
                placeholder={props.editor.placeholder.clone()}/>
        </div>
    }
}

fn testid_label(language: &str) -> AttrValue {
    AttrValue::from(format!("Editor · {language}"))
}

#[derive(Properties, PartialEq)]
struct ComboboxProps {
    combobox: ui::Combobox,
    variant: String,
    data_key: Option<AttrValue>,
    title: Option<AttrValue>,
    label: Option<AttrValue>,
    on: Callback<Action>,
}

/// The options that contain `query` in their label, value or detail; all when it is empty.
fn matching<'a>(options: &'a [ui::Choice], query: &str) -> Vec<&'a ui::Choice> {
    let query = query.trim().to_lowercase();
    options
        .iter()
        .filter(|choice| {
            query.is_empty()
                || [&choice.label, &choice.value, &choice.detail]
                    .iter()
                    .any(|text| text.to_lowercase().contains(&query))
        })
        .collect()
}

/// A select that filters its options while typing.
#[function_component(Combobox)]
fn combobox(props: &ComboboxProps) -> Html {
    let combobox = &props.combobox;
    let key = props.data_key.clone().unwrap_or_default();
    let open = use_state_eq(|| false);
    // What was typed since the field got focus; none shows the chosen option.
    let query = use_state_eq(|| None::<String>);
    let active = use_state_eq(|| 0usize);
    let selected = combobox
        .options
        .iter()
        .find(|choice| choice.value == combobox.value);
    let shown = matching(&combobox.options, query.as_deref().unwrap_or_default());
    let text = match &*query {
        Some(query) => query.clone(),
        None => selected
            .map(|choice| choice.label.clone())
            .unwrap_or_default(),
    };
    let close = {
        let (open, query, active) = (open.clone(), query.clone(), active.clone());
        move || {
            open.set(false);
            query.set(None);
            active.set(0);
        }
    };
    let choose = {
        let (on, msg, close) = (props.on.clone(), combobox.on_change.clone(), close.clone());
        move |value: String| {
            send(&on, &msg, Some(value));
            close();
        }
    };
    let onfocus = {
        let open = open.clone();
        Callback::from(move |event: FocusEvent| {
            event.target_unchecked_into::<HtmlInputElement>().select();
            open.set(true);
        })
    };
    let oninput = {
        let (open, query, active) = (open.clone(), query.clone(), active.clone());
        Callback::from(move |event: InputEvent| {
            query.set(Some(
                event.target_unchecked_into::<HtmlInputElement>().value(),
            ));
            active.set(0);
            open.set(true);
        })
    };
    let onblur = {
        let close = close.clone();
        Callback::from(move |_: FocusEvent| close())
    };
    let onkeydown = {
        let (open, active, close, choose) =
            (open.clone(), active.clone(), close.clone(), choose.clone());
        let values: Vec<String> = shown.iter().map(|choice| choice.value.clone()).collect();
        Callback::from(move |event: KeyboardEvent| {
            if event.is_composing() {
                return;
            }
            let count = values.len();
            match event.key().as_str() {
                "ArrowDown" | "ArrowUp" if count > 0 => {
                    event.prevent_default();
                    let down = event.key() == "ArrowDown";
                    let current = (*active).min(count - 1);
                    active.set(if down {
                        (current + 1) % count
                    } else {
                        (current + count - 1) % count
                    });
                    open.set(true);
                }
                "Enter" => {
                    event.prevent_default();
                    if !event.repeat()
                        && *open
                        && let Some(value) = values.get((*active).min(count.saturating_sub(1)))
                    {
                        choose(value.clone());
                    }
                }
                "Escape" if *open => {
                    event.prevent_default();
                    event.stop_propagation();
                    close();
                }
                _ => {}
            }
        })
    };
    let current = (*active).min(shown.len().saturating_sub(1));
    html! {
        <div class={classes!("ui-combobox", variants(&props.variant))} title={props.title.clone()}>
            <div class="ui-combobox-current">
                <input type="text" role="combobox" class="ui-input" autocomplete="off" spellcheck="false"
                    data-key={props.data_key.clone()} data-testid={props.data_key.clone()}
                    aria-label={props.label.clone()} aria-expanded={open.to_string()}
                    aria-autocomplete="list" disabled={combobox.disabled} value={text}
                    {onfocus} {oninput} {onblur} {onkeydown}/>
                if let Some(choice) = selected.filter(|choice| !choice.detail.is_empty()) {
                    <span class="ui-combobox-detail" data-testid={format!("{key}-detail")}>{choice.detail.clone()}</span>
                }
            </div>
            if *open {
                <div class="ui-combobox-options" role="listbox" data-testid={format!("{key}-options")}>
                    if shown.is_empty() {
                        <span class="ui-combobox-empty">{combobox.empty.clone()}</span>
                    }
                    {for shown.iter().enumerate().map(|(index, choice)| {
                        let value = choice.value.clone();
                        let choose = choose.clone();
                        html! {
                            <button type="button" role="option" key={choice.value.clone()}
                                class={classes!("ui-combobox-option", (index == current).then_some("active"))}
                                data-testid={format!("{key}-option-{}", choice.value)}
                                title={choice.value.clone()} aria-selected={(choice.value == combobox.value).to_string()}
                                onmousedown={Callback::from(|event: MouseEvent| event.prevent_default())}
                                onclick={Callback::from(move |_| choose(value.clone()))}>
                                <span>{choice.label.clone()}</span>
                                if !choice.detail.is_empty() {
                                    <span class="ui-combobox-detail">{choice.detail.clone()}</span>
                                }
                            </button>
                        }
                    })}
                </div>
            }
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct SliderProps {
    slider: ui::Slider,
    variant: String,
    data_key: Option<AttrValue>,
    title: Option<AttrValue>,
    label: Option<AttrValue>,
    on: Callback<Action>,
}

/// Where `value` sits on the track, 0 to 100.
fn percent(value: f64, min: f64, max: f64) -> f64 {
    if max > min {
        ((value - min) / (max - min) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// A slider; with labels it is a track with a tick and a labelled button per step.
#[function_component(Slider)]
fn slider(props: &SliderProps) -> Html {
    let slider = &props.slider;
    let key = props.data_key.clone().unwrap_or_default();
    let send_value = {
        let (on, msg, current) = (props.on.clone(), slider.on_change.clone(), slider.value);
        move |value: String| {
            // `input` and `change` both fire for a move: the module hears it once.
            if value.parse::<f64>().ok() != Some(current) {
                send(&on, &msg, Some(value));
            }
        }
    };
    let oninput = {
        let send_value = send_value.clone();
        Callback::from(move |event: InputEvent| {
            send_value(event.target_unchecked_into::<HtmlInputElement>().value())
        })
    };
    let onchange = {
        let send_value = send_value.clone();
        Callback::from(move |event: Event| {
            send_value(event.target_unchecked_into::<HtmlInputElement>().value())
        })
    };
    let input = html! {
        <input type="range" class="ui-slider-input" data-key={props.data_key.clone()} data-testid={props.data_key.clone()}
            aria-label={props.label.clone()} title={props.title.clone()} disabled={slider.disabled}
            min={slider.min.to_string()} max={slider.max.to_string()}
            step={slider.step.to_string()} value={slider.value.to_string()}
            {oninput} {onchange}/>
    };
    let class = classes!(
        "ui-slider",
        variants(&props.variant),
        slider.disabled.then_some("is-disabled")
    );
    if slider.labels.is_empty() {
        return html! {<div {class}>{input}</div>};
    }
    let at = |value: f64| percent(value, slider.min, slider.max);
    let fill = at(slider.value);
    let last = slider.labels.len() - 1;
    let step = |index: usize| slider.min + index as f64 * slider.step;
    html! {
        <div {class}>
            <div class="ui-slider-track">
                <div class="ui-slider-rail">
                <div class="ui-slider-fill" style={format!("width:{fill}%")}/>
                {for (0..slider.labels.len()).map(|index| html! {
                    <span class={classes!("ui-slider-tick", (step(index) <= slider.value).then_some("on"))}
                        style={format!("left:{}%", at(step(index)))}/>
                })}
                if let Some(marker) = slider.marker.filter(|marker| (*marker as usize) <= last) {
                    <span class="ui-slider-marker" title={slider.marker_title.clone()}
                        style={format!("left:{}%", at(step(marker as usize)))}/>
                }
                <span class="ui-slider-thumb" style={format!("left:{fill}%")}/>
                </div>
                {input}
            </div>
            <div class="ui-slider-levels">
                {for slider.labels.iter().enumerate().map(|(index, label)| {
                    let value = step(index);
                    let (position, shift) = match index {
                        0 => ("left:0".to_owned(), ""),
                        _ if index == last => ("right:0".to_owned(), ""),
                        _ => (format!("left:{}%", at(value)), "transform:translateX(-50%)"),
                    };
                    let on = send_value.clone();
                    html! {
                        <button type="button" tabindex="-1" disabled={slider.disabled}
                            class={classes!("ui-slider-level", (value == slider.value).then_some("selected"))}
                            data-testid={format!("{key}-level-{label}")} style={format!("{position};{shift}")}
                            onclick={Callback::from(move |_| on(value.to_string()))}>{label.clone()}</button>
                    }
                })}
            </div>
        </div>
    }
}
