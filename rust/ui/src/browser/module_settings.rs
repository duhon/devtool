//! Settings: the installed modules, installing a package file and removing a package. The
//! host changes the modules folder; the panel then restarts its host, which loads the change.
//! Removal keeps the module's consoles, history, templates and hints unless the user
//! deletes them too.

use super::{
    error,
    transport::{Reply, Transport},
    views::OperationStatus,
};
use serde_json::{Value, json};
use std::rc::Rc;
use web_sys::HtmlInputElement;
use yew::prelude::*;

#[derive(Properties, Clone)]
pub struct Props {
    pub transport: Rc<Transport>,
    pub onstatus: Callback<OperationStatus>,
    /// A module was removed with its data; the panel drops what it keeps for the module.
    pub onforget: Callback<String>,
    /// The host restarted with a changed modules folder.
    pub onchange: Callback<()>,
}

impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.transport, &other.transport)
            && self.onstatus == other.onstatus
            && self.onforget == other.onforget
            && self.onchange == other.onchange
    }
}

pub enum Msg {
    Load,
    Loaded(Reply),
    Pick,
    Chosen,
    Read(Result<Vec<u8>, String>),
    /// Asks to confirm removing the package folder.
    Remove(String),
    Forget(bool),
    Cancel,
    Confirm,
    Changed(&'static str, f64, Reply),
}

struct Row {
    /// The package folder: the module id, or the folder of a package that failed to load.
    folder: String,
    title: String,
    detail: String,
    failed: bool,
}

pub struct ModuleSettings {
    rows: Vec<Row>,
    busy: bool,
    input: NodeRef,
    /// The package folder whose removal waits for confirmation.
    confirming: Option<String>,
    /// Remove the module's consoles, history, templates and hints too.
    forget: bool,
    /// The package folder being removed with its data.
    forgetting: Option<String>,
}

fn rows(value: &Value) -> Vec<Row> {
    let text = |value: &Value, key: &str| value[key].as_str().unwrap_or_default().to_owned();
    let modules = value["modules"].as_array().into_iter().flatten();
    let failed = value["failed"].as_array().into_iter().flatten();
    modules
        .map(|module| Row {
            folder: text(module, "id"),
            title: text(module, "name"),
            detail: format!("{} {}", text(module, "id"), text(module, "version")),
            failed: false,
        })
        .chain(failed.map(|package| Row {
            folder: text(package, "module"),
            title: text(package, "module"),
            detail: text(package, "error"),
            failed: true,
        }))
        .collect()
}

impl ModuleSettings {
    fn change(&mut self, ctx: &Context<Self>, operation: &'static str, message: Value) {
        let started = js_sys::Date::now();
        self.busy = true;
        ctx.props()
            .onstatus
            .emit(OperationStatus::event(operation, started, None));
        ctx.props().transport.request(
            message,
            false,
            ctx.link()
                .callback(move |reply| Msg::Changed(operation, started, reply)),
        );
    }
}

impl Component for ModuleSettings {
    type Message = Msg;
    type Properties = Props;

    fn create(ctx: &Context<Self>) -> Self {
        ctx.link().send_message(Msg::Load);
        Self {
            rows: Vec::new(),
            busy: false,
            input: NodeRef::default(),
            confirming: None,
            forget: false,
            forgetting: None,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Load => {
                ctx.props().transport.request(
                    json!({"service":"modules"}),
                    false,
                    ctx.link().callback(Msg::Loaded),
                );
                return false;
            }
            Msg::Loaded(reply) => match reply {
                Reply::Result(value) => {
                    self.rows = rows(&value);
                    if !self
                        .rows
                        .iter()
                        .any(|row| self.confirming.as_ref() == Some(&row.folder))
                    {
                        self.confirming = None;
                    }
                }
                Reply::Error(_) => {
                    self.rows.clear();
                    self.confirming = None;
                }
                _ => return false,
            },
            Msg::Pick => {
                if let Some(input) = self.input.cast::<HtmlInputElement>() {
                    input.click();
                }
                return false;
            }
            Msg::Chosen => {
                let Some(input) = self.input.cast::<HtmlInputElement>() else {
                    return false;
                };
                let Some(file) = input.files().and_then(|files| files.get(0)) else {
                    return false;
                };
                // The same file can be chosen again after a failed install.
                input.set_value("");
                self.busy = true;
                let link = ctx.link().clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let bytes = wasm_bindgen_futures::JsFuture::from(file.array_buffer())
                        .await
                        .map(|buffer| js_sys::Uint8Array::new(&buffer).to_vec())
                        .map_err(error);
                    link.send_message(Msg::Read(bytes));
                });
            }
            Msg::Read(bytes) => {
                use base64::Engine;
                match bytes {
                    Ok(bytes) => self.change(
                        ctx,
                        "Install module",
                        json!({
                            "service": "modules",
                            "op": "install",
                            "file": base64::engine::general_purpose::STANDARD.encode(bytes),
                        }),
                    ),
                    Err(message) => {
                        self.busy = false;
                        ctx.props().onstatus.emit(OperationStatus::event(
                            "Install module",
                            js_sys::Date::now(),
                            Some(&Reply::Error(json!({"error":message}))),
                        ));
                    }
                }
            }
            Msg::Remove(folder) => {
                self.confirming = Some(folder);
                self.forget = false;
            }
            Msg::Forget(forget) => self.forget = forget,
            Msg::Cancel => self.confirming = None,
            Msg::Confirm => {
                let Some(folder) = self.confirming.take() else {
                    return false;
                };
                self.forgetting = self.forget.then(|| folder.clone());
                self.change(
                    ctx,
                    "Remove module",
                    json!({"service":"modules","op":"remove","module":folder,"forget":self.forget}),
                );
            }
            Msg::Changed(operation, started, reply) => {
                self.busy = false;
                let forgetting = self.forgetting.take();
                let mut status = OperationStatus::event(operation, started, Some(&reply));
                if let Reply::Result(value) = &reply {
                    status.text = if let Some(module) = value.get("installed") {
                        let replaced = value["replaced"]
                            .as_str()
                            .map(|version| format!(" (was {version})"))
                            .unwrap_or_default();
                        format!(
                            "Installed {} {}{replaced}",
                            module["name"].as_str().unwrap_or_default(),
                            module["version"].as_str().unwrap_or_default()
                        )
                    } else {
                        let removed = value["removed"].as_str().unwrap_or_default();
                        if let Some(warning) = value["warning"].as_str() {
                            format!("Removed {removed} · WARNING · {warning}")
                        } else if value["forgotten"] == true {
                            format!("Removed {removed} and its data")
                        } else {
                            format!("Removed {removed}")
                        }
                    };
                    if let Some(folder) = forgetting {
                        ctx.props().onforget.emit(folder);
                    }
                    // Only a new host process loads the changed modules folder.
                    ctx.props().transport.connect();
                    ctx.props().onchange.emit(());
                    ctx.link().send_message(Msg::Load);
                }
                ctx.props().onstatus.emit(status);
            }
        }
        true
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        html! {
            <div class="module-settings" data-testid="module-settings">
                <strong>{"Modules"}</strong>
                <ul class="module-settings-list">
                    {for self.rows.iter().map(|row| {
                        let folder = row.folder.clone();
                        let confirming = self.confirming.as_deref() == Some(row.folder.as_str());
                        html! {
                            <li class={classes!("module-settings-row", row.failed.then_some("failed"))}
                                data-testid="settings-module" data-module={row.folder.clone()}>
                                <span class="module-settings-name">
                                    <span>{&row.title}</span>
                                    <span class="module-settings-detail" title={row.detail.clone()}>{&row.detail}</span>
                                </span>
                                <button type="button" class="tab-button" data-testid="settings-module-remove" disabled={self.busy || confirming}
                                    onclick={ctx.link().callback(move |_| Msg::Remove(folder.clone()))}>{"Remove"}</button>
                                if confirming {
                                    <div class="module-settings-confirm" role="group" data-testid="settings-module-confirm">
                                        <p>{format!("Remove module \"{}\"? Its consoles, checks, history, templates and hints are kept and come back when it is installed again.", row.title)}</p>
                                        <label>
                                            <input type="checkbox" data-testid="settings-module-forget" checked={self.forget}
                                                onchange={ctx.link().callback(|e: Event| Msg::Forget(e.target_unchecked_into::<HtmlInputElement>().checked()))}/>
                                            {"Delete them too, in every session"}
                                        </label>
                                        <button type="button" class="tab-button" data-testid="settings-module-remove-confirm" disabled={self.busy}
                                            onclick={ctx.link().callback(|_| Msg::Confirm)}>{"Remove"}</button>
                                        <button type="button" class="tab-button" data-testid="settings-module-remove-cancel"
                                            onclick={ctx.link().callback(|_| Msg::Cancel)}>{"Cancel"}</button>
                                    </div>
                                }
                            </li>
                        }
                    })}
                </ul>
                <div class="console-settings-actions">
                    <button type="button" class="tab-button" data-testid="settings-module-install" disabled={self.busy}
                        onclick={ctx.link().callback(|_| Msg::Pick)}>{"Install from file…"}</button>
                    <input type="file" hidden=true accept={services_contracts::package::ARCHIVE_EXTENSION} ref={self.input.clone()}
                        data-testid="settings-module-file" onchange={ctx.link().callback(|_: Event| Msg::Chosen)}/>
                </div>
            </div>
        }
    }
}
