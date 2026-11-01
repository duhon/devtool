//! Settings: one table of the modules, each with the source it comes from — the installed
//! version, the newest one the source offers, installing, updating and removing. The
//! application itself is the `app` row and updates the same way, but can't be removed. The
//! host changes the modules folder; the panel then restarts its host, which loads the change.
//! Removal keeps the module's consoles, history, templates and hints unless the user deletes
//! them too.

use super::{
    sources_settings::newer,
    transport::{Reply, Transport},
    views::OperationStatus,
};
use serde_json::{Value, json};
use services_contracts::package::APPLICATION_ID;
use std::{collections::BTreeSet, rc::Rc};
use web_sys::HtmlInputElement;
use yew::prelude::*;

#[derive(Properties, Clone)]
pub struct Props {
    pub transport: Rc<Transport>,
    /// The last check of the sources: their offers and what is installed.
    pub check: Option<Rc<Value>>,
    pub checking: bool,
    /// Checks the sources again.
    pub oncheck: Callback<bool>,
    /// An application update is in progress.
    pub app_busy: bool,
    /// The source an application update was installed from, waiting for the reload.
    pub app_installed: Option<String>,
    /// Installs the application `(source slug, version)`.
    pub onapp: Callback<(String, String)>,
    pub onreload: Callback<()>,
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
            && self.checking == other.checking
            && self.oncheck == other.oncheck
            && self.app_busy == other.app_busy
            && self.app_installed == other.app_installed
            && self.onapp == other.onapp
            && self.onreload == other.onreload
            && self.onstatus == other.onstatus
            && self.onforget == other.onforget
            && self.onchange == other.onchange
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Detail {
    Description,
    Notes,
    NewNotes,
}

pub enum Msg {
    /// Shows or hides a detail of a row.
    Toggle(String, Detail),
    /// Asks to confirm removing a row's module: `(row key, module)`.
    Remove(String, String),
    Forget(bool),
    Cancel,
    Confirm,
    /// Installs `(source slug, module, version)`.
    Install(String, String, String),
    /// Opens a folder source.
    Open(String),
    Opened(f64, Reply),
    Changed(&'static str, f64, Reply),
}

/// A module with one source: what the source offers and what is installed from it. A module
/// installed from no source, or from one that no longer offers it, has a row of its own.
struct Row<'a> {
    id: &'a str,
    slug: Option<&'a str>,
    source: Option<&'a Value>,
    /// The versions the source offers, newest first.
    offers: Vec<&'a Value>,
    /// What is installed, when it is installed from this source.
    installed: Option<&'a Value>,
}

impl Row<'_> {
    fn key(&self) -> String {
        format!("{}/{}", self.slug.unwrap_or_default(), self.id)
    }

    fn app(&self) -> bool {
        self.id == APPLICATION_ID
    }

    fn title(&self) -> &str {
        self.installed
            .and_then(|installed| installed["name"].as_str())
            .or_else(|| self.offers.first()?["name"].as_str())
            .unwrap_or(self.id)
    }

    fn description(&self) -> &str {
        self.offers
            .first()
            .and_then(|offer| offer["description"].as_str())
            .or_else(|| self.installed?["description"].as_str())
            .unwrap_or_default()
    }

    /// The newest version the source offers that can be installed here.
    fn newest(&self) -> Option<&Value> {
        self.offers
            .iter()
            .copied()
            .find(|offer| offer.get("unsupported").is_none())
    }
}

fn rows(check: &Value) -> Vec<Row<'_>> {
    let sources: Vec<&Value> = check["sources"].as_array().into_iter().flatten().collect();
    let mut rows: Vec<Row> = Vec::new();
    for source in &sources {
        let slug = source["slug"].as_str();
        for offer in source["modules"].as_array().into_iter().flatten() {
            let id = offer["id"].as_str().unwrap_or_default();
            match rows.iter_mut().find(|row| row.id == id && row.slug == slug) {
                Some(row) => row.offers.push(offer),
                None => rows.push(Row {
                    id,
                    slug,
                    source: Some(source),
                    offers: vec![offer],
                    installed: None,
                }),
            }
        }
    }
    for (id, installed) in check["installed"].as_object().into_iter().flatten() {
        let slug = installed["source"].as_str();
        match rows
            .iter_mut()
            .find(|row| slug.is_some() && row.id == id && row.slug == slug)
        {
            Some(row) => row.installed = Some(installed),
            None => rows.push(Row {
                id,
                slug,
                source: sources
                    .iter()
                    .copied()
                    .find(|source| slug.is_some() && source["slug"].as_str() == slug),
                offers: Vec::new(),
                installed: Some(installed),
            }),
        }
    }
    rows.sort_by(|a, b| (!a.app(), a.id).cmp(&(!b.app(), b.id)));
    rows
}

pub struct ModuleSettings {
    busy: bool,
    /// The details shown, by row key.
    open: BTreeSet<(String, Detail)>,
    /// The row whose module waits for the removal to be confirmed: `(row key, module)`.
    confirming: Option<(String, String)>,
    /// Remove the module's consoles, history, templates and hints too.
    forget: bool,
    /// The module being removed with its data.
    forgetting: Option<String>,
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

    fn toggle(&self, ctx: &Context<Self>, key: &str, detail: Detail) -> Callback<MouseEvent> {
        let key = key.to_owned();
        ctx.link()
            .callback(move |_| Msg::Toggle(key.clone(), detail))
    }

    fn view_source(&self, ctx: &Context<Self>, row: &Row) -> Html {
        let Some(source) = row.source else {
            return html! {<span data-testid="settings-module-source">{"—"}</span>};
        };
        let name = source["name"].as_str().unwrap_or_default().to_owned();
        if source["kind"] == "folder" {
            let slug = row.slug.unwrap_or_default().to_owned();
            return html! {
                <button type="button" class="module-settings-link" data-testid="settings-module-source"
                    title={source["path"].as_str().unwrap_or_default().to_owned()}
                    onclick={ctx.link().callback(move |_| Msg::Open(slug.clone()))}>{name}</button>
            };
        }
        let repository = source["repository"].as_str().unwrap_or_default();
        let url = match source["path"].as_str() {
            Some(path) => format!(
                "https://github.com/{repository}/tree/{}/{path}",
                source["branch"].as_str().unwrap_or("HEAD")
            ),
            None => format!("https://github.com/{repository}"),
        };
        html! {
            <a class="module-settings-link" data-testid="settings-module-source" href={url}
                target="_blank" rel="noopener noreferrer">{name}</a>
        }
    }

    fn view_row(&self, ctx: &Context<Self>, row: &Row, app_version: Option<&str>) -> Html {
        let props = ctx.props();
        let key = row.key();
        let shown = |detail| self.open.contains(&(key.clone(), detail));
        let slug = row.slug.unwrap_or_default().to_owned();
        let version = row
            .installed
            .and_then(|installed| installed["version"].as_str());
        let error = row
            .installed
            .and_then(|installed| installed["error"].as_str());
        let base = if row.app() { app_version } else { version };
        let newest = row.newest().filter(|offer| {
            let offered = offer["version"].as_str().unwrap_or_default();
            match base {
                Some(base) if row.app() || row.installed.is_some() => newer(offered, base),
                _ => true,
            }
        });
        let new_version = newest
            .and_then(|offer| offer["version"].as_str())
            .unwrap_or_default()
            .to_owned();
        let unsupported = row
            .newest()
            .is_none()
            .then(|| row.offers.first()?["unsupported"].as_str())
            .flatten();
        let confirming = self
            .confirming
            .as_ref()
            .is_some_and(|(confirming, _)| *confirming == key);
        let reload = row.app() && props.app_installed.as_deref() == row.slug;
        let busy = self.busy || props.app_busy || props.app_installed.is_some() && row.app();
        let install = {
            let (slug, id, version) = (slug.clone(), row.id.to_owned(), new_version.clone());
            let app = row.app();
            let onapp = props.onapp.clone();
            ctx.link().batch_callback(move |_| {
                if app {
                    onapp.emit((slug.clone(), version.clone()));
                    None
                } else {
                    Some(Msg::Install(slug.clone(), id.clone(), version.clone()))
                }
            })
        };
        let description = row.description().to_owned();
        let notes = row
            .installed
            .and_then(|installed| installed["notes"].as_str())
            .unwrap_or_default()
            .to_owned();
        let new_notes = newest
            .and_then(|offer| offer["notes"].as_str())
            .unwrap_or_default()
            .to_owned();
        let details = |testid: &'static str, text: String| {
            html! {
                <tr class="module-settings-details" data-testid={testid}>
                    <td colspan="5"><pre>{if text.trim().is_empty() { "No details.".to_owned() } else { text }}</pre></td>
                </tr>
            }
        };
        html! {<>
            <tr class={classes!("module-settings-row", error.map(|_| "failed"))} data-testid="settings-module"
                data-module={row.id.to_owned()} data-source={slug.clone()}>
                <td>
                    <button type="button" class="module-settings-link" data-testid="settings-module-name"
                        title="Description" onclick={self.toggle(ctx, &key, Detail::Description)}>{row.title()}</button>
                </td>
                <td>{self.view_source(ctx, row)}</td>
                <td>
                    if row.installed.is_some() {
                        <button type="button" class={classes!("module-settings-link", "module-settings-version", error.map(|_| "failed"))}
                            data-testid="settings-module-version" title="Release notes"
                            onclick={self.toggle(ctx, &key, Detail::Notes)}>{version.unwrap_or("failed")}</button>
                    } else {
                        <span data-testid="settings-module-version">{"—"}</span>
                    }
                </td>
                <td>
                    if newest.is_some() {
                        <button type="button" class="module-settings-link" data-testid="settings-module-new-version"
                            title="Release notes" onclick={self.toggle(ctx, &key, Detail::NewNotes)}>{&new_version}</button>
                    }
                </td>
                <td class="module-settings-actions">
                    if reload {
                        <button type="button" class="tab-button" data-testid="settings-module-reload"
                            onclick={props.onreload.reform(|_| ())}>{"Reload extension"}</button>
                    } else if newest.is_some() && row.installed.is_some() {
                        <button type="button" class="tab-button" data-testid="settings-module-update"
                            disabled={busy} onclick={install}>{"Update"}</button>
                    } else if newest.is_some() {
                        <button type="button" class="tab-button" data-testid="settings-module-install"
                            disabled={busy} onclick={install}>{"Install"}</button>
                    }
                    if row.installed.is_some() {
                        <button type="button" class="tab-button" data-testid="settings-module-remove"
                            title={row.app().then_some("The application can't be removed")}
                            disabled={self.busy || confirming || row.app()}
                            onclick={{
                                let (key, id) = (key.clone(), row.id.to_owned());
                                ctx.link().callback(move |_| Msg::Remove(key.clone(), id.clone()))
                            }}>{"Remove"}</button>
                    }
                </td>
            </tr>
            if let Some(error) = error {
                <tr class="module-settings-details failed" data-testid="settings-module-error">
                    <td colspan="5">{error}</td>
                </tr>
            }
            if let Some(reason) = unsupported {
                <tr class="module-settings-details" data-testid="settings-module-unsupported">
                    <td colspan="5">{reason}</td>
                </tr>
            }
            if shown(Detail::Description) {{details("settings-module-description", description)}}
            if shown(Detail::Notes) {{details("settings-module-notes", notes)}}
            if shown(Detail::NewNotes) {{details("settings-module-new-notes", new_notes)}}
            if confirming {
                <tr class="module-settings-details">
                    <td colspan="5">
                        <div class="module-settings-confirm" role="group" data-testid="settings-module-confirm">
                            <p>{format!("Remove module \"{}\"? Its consoles, checks, history, templates and hints are kept and come back when it is installed again.", row.title())}</p>
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
                    </td>
                </tr>
            }
        </>}
    }
}

impl Component for ModuleSettings {
    type Message = Msg;
    type Properties = Props;

    fn create(_: &Context<Self>) -> Self {
        Self {
            busy: false,
            open: BTreeSet::new(),
            confirming: None,
            forget: false,
            forgetting: None,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Toggle(key, detail) => {
                if !self.open.remove(&(key.clone(), detail)) {
                    self.open.insert((key, detail));
                }
            }
            Msg::Remove(key, id) => {
                self.confirming = Some((key, id));
                self.forget = false;
            }
            Msg::Forget(forget) => self.forget = forget,
            Msg::Cancel => self.confirming = None,
            Msg::Confirm => {
                let Some((_, id)) = self.confirming.take() else {
                    return false;
                };
                self.forgetting = self.forget.then(|| id.clone());
                self.change(
                    ctx,
                    "Remove module",
                    json!({"service":"modules","op":"remove","module":id,"forget":self.forget}),
                );
            }
            Msg::Install(slug, id, version) => self.change(
                ctx,
                "Install module",
                json!({"service":"modules","op":"install","source":slug,"module":id,"version":version}),
            ),
            Msg::Open(slug) => {
                let started = js_sys::Date::now();
                ctx.props().transport.request(
                    json!({"service":"sources","op":"open","source":slug}),
                    false,
                    ctx.link()
                        .callback(move |reply| Msg::Opened(started, reply)),
                );
                return false;
            }
            Msg::Opened(started, reply) => {
                if !matches!(reply, Reply::Result(_)) {
                    ctx.props().onstatus.emit(OperationStatus::event(
                        "Open source",
                        started,
                        Some(&reply),
                    ));
                }
                return false;
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
                    if let Some(module) = forgetting {
                        ctx.props().onforget.emit(module);
                    }
                    // Only a new host process loads the changed modules folder.
                    ctx.props().transport.connect();
                    ctx.props().onchange.emit(());
                }
                ctx.props().onstatus.emit(status);
            }
        }
        true
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let props = ctx.props();
        let check = props.check.as_deref();
        let rows = check.map(rows).unwrap_or_default();
        let app_version =
            check.and_then(|check| check["installed"][APPLICATION_ID]["version"].as_str());
        html! {
            <div class="module-settings" data-testid="module-settings">
                <div class="module-settings-head">
                    <strong>{"Modules"}</strong>
                    <button type="button" class="tab-button" data-testid="settings-modules-check"
                        disabled={props.checking} onclick={props.oncheck.reform(|_| true)}>{"Check for updates"}</button>
                </div>
                if rows.is_empty() {
                    <span class="module-settings-hint" data-testid="settings-modules-empty">
                        {if props.checking { "Checking the sources…" } else { "Check the sources to see the modules." }}
                    </span>
                } else {
                    <table class="module-settings-table" data-testid="settings-modules">
                        <thead>
                            <tr><th>{"Name"}</th><th>{"Source"}</th><th>{"Version"}</th><th>{"New version"}</th><th></th></tr>
                        </thead>
                        <tbody>
                            {for rows.iter().map(|row| self.view_row(ctx, row, app_version))}
                        </tbody>
                    </table>
                }
            </div>
        }
    }
}
