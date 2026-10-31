//! Settings: the installed modules, the modules the sources offer, installing from a source or
//! a package file and removing a package. The host changes the modules folder; the panel then
//! restarts its host, which loads the change. Removal keeps the module's consoles, history,
//! templates and hints unless the user deletes them too.

use super::{
    error,
    sources_settings::newer,
    transport::{Reply, Transport},
    views::OperationStatus,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, rc::Rc};
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

#[derive(Properties, Clone)]
pub struct Props {
    pub transport: Rc<Transport>,
    /// The last check of the sources, with their module offers.
    pub check: Option<Rc<Value>>,
    pub onstatus: Callback<OperationStatus>,
    /// A module was removed with its data; the panel drops what it keeps for the module.
    pub onforget: Callback<String>,
    /// The host restarted with a changed modules folder.
    pub onchange: Callback<()>,
}

impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool {
        let same_check = match (&self.check, &other.check) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        Rc::ptr_eq(&self.transport, &other.transport)
            && same_check
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
    /// Picks the version of a source's module: `(source slug, module, version)`.
    Version(String, String, String),
    /// Installs `(source slug, module, version)`.
    Install(String, String, String),
    Changed(&'static str, f64, Reply),
}

struct Row {
    /// The package folder: the module id, or the folder of a package that failed to load.
    folder: String,
    title: String,
    detail: String,
    version: String,
    failed: bool,
}

/// The versions of one module a source offers, newest first.
struct Offered<'a> {
    source: &'a str,
    slug: &'a str,
    id: &'a str,
    versions: Vec<&'a Value>,
}

/// Every source's offers, grouped by module.
fn offered(check: &Value) -> Vec<Offered<'_>> {
    let mut out: Vec<Offered> = Vec::new();
    for source in check["sources"].as_array().into_iter().flatten() {
        for offer in source["modules"].as_array().into_iter().flatten() {
            let id = offer["id"].as_str().unwrap_or_default();
            let slug = source["slug"].as_str().unwrap_or_default();
            match out.last_mut() {
                Some(last) if last.slug == slug && last.id == id => last.versions.push(offer),
                _ => out.push(Offered {
                    source: source["name"].as_str().unwrap_or_default(),
                    slug,
                    id,
                    versions: vec![offer],
                }),
            }
        }
    }
    out
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
    /// The picked version of a source's module, by `(source slug, module)`.
    versions: BTreeMap<(String, String), String>,
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
            version: text(module, "version"),
            failed: false,
        })
        .chain(failed.map(|package| Row {
            folder: text(package, "module"),
            title: text(package, "module"),
            detail: text(package, "error"),
            version: String::new(),
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
            versions: BTreeMap::new(),
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
            Msg::Version(slug, id, version) => {
                self.versions.insert((slug, id), version);
            }
            Msg::Install(slug, id, version) => self.change(
                ctx,
                "Install module",
                json!({"service":"modules","op":"install","source":slug,"module":id,"version":version}),
            ),
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
        let check = ctx.props().check.as_deref();
        let offers = check.map(offered).unwrap_or_default();
        let source_of =
            |id: &str| check.and_then(|check| check["installed"][id]["source"].as_str());
        let name_of = |slug: &str| {
            offers
                .iter()
                .find(|offered| offered.slug == slug)
                .map(|offered| offered.source.to_owned())
                .or_else(|| {
                    check?["sources"]
                        .as_array()?
                        .iter()
                        .find(|source| source["slug"] == slug)?["name"]
                        .as_str()
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| slug.to_owned())
        };
        // The newest supported version of an installed module in its source, if newer.
        let update = |row: &Row| {
            let slug = source_of(&row.folder)?;
            let offered = offers
                .iter()
                .find(|offered| offered.slug == slug && offered.id == row.folder)?;
            let version = offered
                .versions
                .iter()
                .find(|offer| offer.get("unsupported").is_none())?["version"]
                .as_str()?;
            newer(version, &row.version).then(|| (slug.to_owned(), version.to_owned()))
        };
        html! {
            <div class="module-settings" data-testid="module-settings">
                <strong>{"Modules"}</strong>
                if self.rows.is_empty() {
                    <span class="agent-skills-hint" data-testid="settings-modules-empty">{"No modules installed yet. Install them from a source below or from a package file."}</span>
                }
                <ul class="module-settings-list">
                    {for self.rows.iter().map(|row| {
                        let folder = row.folder.clone();
                        let confirming = self.confirming.as_deref() == Some(row.folder.as_str());
                        let source = (!row.failed).then(|| source_of(&row.folder)).flatten();
                        let update = (!row.failed).then(|| update(row)).flatten();
                        html! {
                            <li class={classes!("module-settings-row", row.failed.then_some("failed"))}
                                data-testid="settings-module" data-module={row.folder.clone()}>
                                <span class="module-settings-name">
                                    <span>{&row.title}</span>
                                    <span class="module-settings-detail" title={row.detail.clone()}>{&row.detail}</span>
                                </span>
                                if let Some(slug) = source {
                                    <span class="settings-origin" data-testid="settings-module-source" title="The source it was installed from">{name_of(slug)}</span>
                                }
                                if let Some((slug, version)) = update {
                                    <button type="button" class="tab-button" data-testid="settings-module-update" disabled={self.busy}
                                        onclick={ctx.link().callback({
                                            let (folder, version) = (row.folder.clone(), version.clone());
                                            move |_| Msg::Install(slug.clone(), folder.clone(), version.clone())
                                        })}>{format!("Update to {version}")}</button>
                                }
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
                if !offers.is_empty() {
                    <span class="module-settings-subtitle">{"Available from sources"}</span>
                    <ul class="module-settings-list" data-testid="settings-module-offers">
                        {for offers.iter().map(|offered| {
                            let key = (offered.slug.to_owned(), offered.id.to_owned());
                            let picked = self.versions.get(&key).cloned().unwrap_or_else(|| {
                                offered.versions.iter()
                                    .find(|offer| offer.get("unsupported").is_none())
                                    .or(offered.versions.first())
                                    .and_then(|offer| offer["version"].as_str())
                                    .unwrap_or_default()
                                    .to_owned()
                            });
                            let offer = offered.versions.iter().find(|offer| offer["version"] == picked.as_str());
                            let unsupported = offer.and_then(|offer| offer["unsupported"].as_str());
                            let notes = offer.and_then(|offer| offer["notes"].as_str()).unwrap_or_default();
                            let installed = self.rows.iter().find(|row| !row.failed && row.folder == offered.id);
                            let from = installed.and_then(|_| source_of(offered.id));
                            let here = from == Some(offered.slug);
                            let current = here && installed.is_some_and(|row| row.version == picked);
                            let label = match (installed, here) {
                                _ if current => "Installed",
                                (Some(_), true) => "Install this version",
                                (Some(_), false) => "Install instead",
                                (None, _) => "Install",
                            };
                            let detail = match (installed, from) {
                                (Some(row), Some(slug)) if !here => format!("from {} · installed {} from {}", offered.source, row.version, name_of(slug)),
                                (Some(row), None) => format!("from {} · installed {} from a file", offered.source, row.version),
                                _ => format!("from {}", offered.source),
                            };
                            let (slug, id) = key.clone();
                            let install = {
                                let (slug, id, picked) = (slug.clone(), id.clone(), picked.clone());
                                ctx.link().callback(move |_| Msg::Install(slug.clone(), id.clone(), picked.clone()))
                            };
                            html! {
                                <li class="module-settings-row" data-testid="settings-module-offer" data-source={slug.clone()} data-module={id.clone()}>
                                    <span class="module-settings-name">
                                        <span>{offered.id}</span>
                                        <span class="module-settings-detail" title={detail.clone()}>{detail}</span>
                                    </span>
                                    <select data-testid="settings-module-offer-version" aria-label={format!("Version of {}", offered.id)}
                                        onchange={ctx.link().callback(move |event: Event| Msg::Version(slug.clone(), id.clone(), event.target_unchecked_into::<HtmlSelectElement>().value()))}>
                                        {for offered.versions.iter().map(|offer| {
                                            let version = offer["version"].as_str().unwrap_or_default().to_owned();
                                            html!{<option value={version.clone()} selected={version == picked}>{version}</option>}
                                        })}
                                    </select>
                                    <button type="button" class="tab-button" data-testid="settings-module-offer-install"
                                        disabled={self.busy || current || unsupported.is_some()} onclick={install}>{label}</button>
                                    if let Some(reason) = unsupported {
                                        <span class="source-settings-error" data-testid="settings-module-offer-unsupported">{reason}</span>
                                    }
                                    if !notes.trim().is_empty() {
                                        <details class="module-settings-notes">
                                            <summary>{"Release notes"}</summary>
                                            <pre>{notes}</pre>
                                        </details>
                                    }
                                </li>
                            }
                        })}
                    </ul>
                }
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
