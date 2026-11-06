//! Settings: the sources of modules and shared settings (dictionary, templates, hints and
//! skills), GitHub repositories and folders in order. Later sources override earlier ones and
//! the user's own changes override them all. Checks run automatically; applying a source's
//! settings and installing its modules wait for the user.

use super::{
    transport::{Reply, Transport},
    views::OperationStatus,
};
use serde_json::{Value, json};
use std::rc::Rc;
use wasm_bindgen::JsValue;
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

#[derive(Properties, Clone)]
pub struct Props {
    pub transport: Rc<Transport>,
    /// The last check of the sources; `None` before the first one finishes.
    pub check: Option<Rc<Value>>,
    pub checking: bool,
    /// Checks the sources again; `true` also checks the recently checked ones.
    pub oncheck: Callback<bool>,
    /// A source's settings were applied.
    pub onapplied: Callback<()>,
    pub onstatus: Callback<OperationStatus>,
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
            && self.onapplied == other.onapplied
            && self.onstatus == other.onstatus
    }
}

/// A source being added.
#[derive(Clone, Default)]
struct Draft {
    name: String,
    /// `github` or `folder`.
    kind: String,
    repository: String,
    branch: String,
    path: String,
}

pub enum Msg {
    Loaded(Reply),
    Move(usize, bool),
    /// Starts or cancels renaming the source at an index.
    Rename(Option<usize>),
    RenameInput(String),
    RenameSave,
    /// Asks to confirm removing the source at an index, or cancels.
    Remove(Option<usize>),
    RemoveConfirm,
    Adding(bool),
    Field(&'static str, String),
    Add,
    TokenInput(String),
    /// Saves the entered token, or clears the saved one.
    Token(bool),
    Apply(String),
    Done(&'static str, f64, Reply),
}

pub struct SourcesSettings {
    sources: Vec<Value>,
    token_set: bool,
    warning: Option<String>,
    loaded: bool,
    busy: bool,
    renaming: Option<(usize, String)>,
    removing: Option<usize>,
    draft: Option<Draft>,
    token: String,
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}

/// A date and time from seconds since the Unix epoch.
fn when(seconds: &Value) -> String {
    let date = js_sys::Date::new(&JsValue::from_f64(
        seconds.as_f64().unwrap_or_default() * 1e3,
    ));
    date.to_locale_string("en-US", &JsValue::UNDEFINED)
        .as_string()
        .unwrap_or_default()
}

fn location(source: &Value) -> String {
    if source["kind"] == "github" {
        let mut out = text(source, "repository");
        if let Some(branch) = source["branch"].as_str() {
            out.push_str(&format!(" @ {branch}"));
        }
        if let Some(path) = source["path"].as_str() {
            out.push_str(&format!(" / {path}"));
        }
        out
    } else {
        text(source, "path")
    }
}

fn change_text(change: &Value) -> String {
    format!(
        "{} {} {}",
        text(change, "kind"),
        text(change, "key"),
        text(change, "change")
    )
}

/// Whether version `a` is newer than `b`; versions are dotted numbers.
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |version: &str| -> Vec<u64> {
        version
            .split(['.', '-', '+'])
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    parts(a) > parts(b)
}

/// What a check found for the user: the sources that failed, and the settings and installed
/// modules that can be updated.
pub fn summary(check: &Value) -> (Vec<String>, Vec<String>) {
    let sources = check["sources"].as_array().cloned().unwrap_or_default();
    let mut errors = Vec::new();
    let mut updates = Vec::new();
    for source in &sources {
        let name = text(source, "name");
        if let Some(error) = source["error"].as_str() {
            errors.push(format!("{name}: {error}"));
        }
        let changed = source["changes"]
            .as_array()
            .is_some_and(|changes| !changes.is_empty());
        if source["configs"] == true && (source["applied"] != true || changed) {
            updates.push(format!("settings of {name}"));
        }
    }
    for (id, installed) in check["installed"].as_object().into_iter().flatten() {
        let (Some(slug), Some(version)) =
            (installed["source"].as_str(), installed["version"].as_str())
        else {
            continue;
        };
        let newest = sources
            .iter()
            .filter(|source| source["slug"] == slug)
            .flat_map(|source| source["modules"].as_array().cloned().unwrap_or_default())
            .filter(|offer| offer["id"] == id.as_str() && offer.get("unsupported").is_none())
            .filter_map(|offer| offer["version"].as_str().map(str::to_owned))
            .find(|offered| newer(offered, version));
        if let Some(newest) = newest {
            updates.push(format!("{id} {newest}"));
        }
    }
    (errors, updates)
}

impl SourcesSettings {
    fn load(&mut self, value: &Value) {
        self.sources = value["sources"].as_array().cloned().unwrap_or_default();
        self.token_set = value["tokenSet"] == true;
        self.warning = value["warning"].as_str().map(str::to_owned);
    }

    fn request(&mut self, ctx: &Context<Self>, operation: &'static str, message: Value) {
        let started = js_sys::Date::now();
        self.busy = true;
        ctx.props()
            .onstatus
            .emit(OperationStatus::event(operation, started, None));
        ctx.props().transport.request(
            message,
            false,
            ctx.link()
                .callback(move |reply| Msg::Done(operation, started, reply)),
        );
    }

    /// Saves `list`; configured sources are sent with their slug so they keep their data.
    fn save(&mut self, ctx: &Context<Self>, operation: &'static str, list: Vec<Value>) {
        let entries: Vec<Value> = list
            .into_iter()
            .map(|mut source| {
                if let Some(slug) = source.get("slug").cloned() {
                    source["id"] = slug;
                }
                source
            })
            .collect();
        self.request(
            ctx,
            operation,
            json!({"service":"sources","op":"save","sources":entries}),
        );
    }

    fn checked<'a>(&self, ctx: &'a Context<Self>, slug: &str) -> Option<&'a Value> {
        ctx.props()
            .check
            .as_deref()?
            .get("sources")?
            .as_array()?
            .iter()
            .find(|source| source["slug"] == slug)
    }

    fn view_check(&self, ctx: &Context<Self>, slug: &str) -> Html {
        let Some(checked) = self.checked(ctx, slug) else {
            let state = if ctx.props().checking {
                "Checking…"
            } else {
                "Not checked yet"
            };
            return html! {<span class="module-settings-detail">{state}</span>};
        };
        let changes = checked["changes"].as_array().cloned().unwrap_or_default();
        let applied = checked["applied"] == true;
        let configs = checked["configs"] == true;
        let problems = checked["problems"].as_array().cloned().unwrap_or_default();
        let source = slug.to_owned();
        html! {
            <>
                <span class="module-settings-detail" data-testid="source-checked">{format!("Checked {}", when(&checked["checkedAt"]))}</span>
                if let Some(error) = checked["error"].as_str() {
                    <span class="source-settings-error" data-testid="source-error">{error}</span>
                }
                if configs && (!applied || !changes.is_empty()) {
                    <div class="source-settings-changes" data-testid="source-changes">
                        <span>{if applied {"Settings changed:"} else {"Settings not applied yet:"}}</span>
                        <ul>
                            {for changes.iter().map(|change| html! {
                                <li data-testid="source-change">
                                    {change_text(change)}
                                    if change["overridden"] == true {
                                        <span class="settings-origin" title="Your own change stays on top of this one">{"local override"}</span>
                                    }
                                </li>
                            })}
                        </ul>
                        <button type="button" class="tab-button" data-testid="source-apply" disabled={self.busy}
                            onclick={ctx.link().callback(move |_| Msg::Apply(source.clone()))}>{"Apply"}</button>
                    </div>
                } else if configs {
                    <span class="module-settings-detail" data-testid="source-up-to-date">{"Settings up to date"}</span>
                }
                if !problems.is_empty() {
                    <ul class="agent-skills-problems" data-testid="source-problems">
                        {for problems.iter().map(|problem| html!{<li>{problem.as_str().unwrap_or_default()}</li>})}
                    </ul>
                }
            </>
        }
    }

    fn view_source(&self, ctx: &Context<Self>, index: usize, source: &Value) -> Html {
        let slug = text(source, "slug");
        let name = text(source, "name");
        let last = index + 1 == self.sources.len();
        let renaming = self.renaming.as_ref().filter(|(at, _)| *at == index);
        let removing = self.removing == Some(index);
        let applied = &source["applied"];
        html! {
            <li class="source-settings-row" data-testid="source" data-source={slug.clone()}>
                <div class="source-settings-head">
                    if let Some((_, input)) = renaming {
                        <input class="source-settings-input" data-testid="source-name-input" value={input.clone()} spellcheck="false" autocomplete="off"
                            oninput={ctx.link().callback(|event:InputEvent|Msg::RenameInput(event.target_unchecked_into::<HtmlInputElement>().value()))}
                            onkeydown={ctx.link().batch_callback(|event:KeyboardEvent| {
                                (event.key()=="Enter" && !event.is_composing() && !event.repeat()).then(|| {
                                    event.prevent_default();
                                    Msg::RenameSave
                                })
                            })}/>
                        <button type="button" class="tab-button" data-testid="source-rename-save" disabled={self.busy}
                            onclick={ctx.link().callback(|_| Msg::RenameSave)}>{"Save"}</button>
                        <button type="button" class="tab-button" data-testid="source-rename-cancel"
                            onclick={ctx.link().callback(|_| Msg::Rename(None))}>{"Cancel"}</button>
                    } else {
                        <span class="module-settings-name">
                            <span data-testid="source-name">{&name}</span>
                            <span class="module-settings-detail" title={location(source)}>{location(source)}</span>
                        </span>
                        <button type="button" class="tab-button" data-testid="source-up" aria-label={format!("Move {name} up")} title="Move up"
                            disabled={self.busy || index == 0} onclick={ctx.link().callback(move |_| Msg::Move(index, true))}>{"↑"}</button>
                        <button type="button" class="tab-button" data-testid="source-down" aria-label={format!("Move {name} down")} title="Move down"
                            disabled={self.busy || last} onclick={ctx.link().callback(move |_| Msg::Move(index, false))}>{"↓"}</button>
                        <button type="button" class="tab-button" data-testid="source-rename" disabled={self.busy}
                            onclick={ctx.link().callback(move |_| Msg::Rename(Some(index)))}>{"Rename"}</button>
                        <button type="button" class="tab-button" data-testid="source-remove" disabled={self.busy || removing}
                            onclick={ctx.link().callback(move |_| Msg::Remove(Some(index)))}>{"Remove"}</button>
                    }
                </div>
                if removing {
                    <div class="module-settings-confirm" role="group" data-testid="source-remove-confirm-group">
                        <p>{format!("Remove source \"{name}\"? Its applied settings and skills are removed; modules installed from it stay.")}</p>
                        <button type="button" class="tab-button" data-testid="source-remove-confirm" disabled={self.busy}
                            onclick={ctx.link().callback(|_| Msg::RemoveConfirm)}>{"Remove"}</button>
                        <button type="button" class="tab-button" data-testid="source-remove-cancel"
                            onclick={ctx.link().callback(|_| Msg::Remove(None))}>{"Cancel"}</button>
                    </div>
                }
                if applied.is_object() {
                    <span class="module-settings-detail" data-testid="source-applied">
                        {format!("Settings applied {}", when(&applied["at"]))}
                        if let Some(revision) = applied["revision"].as_str().filter(|revision| !revision.is_empty()) {
                            {format!(" · {}", revision.chars().take(7).collect::<String>())}
                        }
                    </span>
                }
                {self.view_check(ctx, &slug)}
            </li>
        }
    }

    fn view_draft(&self, ctx: &Context<Self>, draft: &Draft) -> Html {
        let field = |key: &'static str,
                     label: &'static str,
                     value: &str,
                     placeholder: &'static str| {
            html! {
                <label class="source-settings-field">
                    <span>{label}</span>
                    <input data-testid={format!("source-add-{key}")} value={value.to_owned()} {placeholder} spellcheck="false" autocomplete="off"
                        oninput={ctx.link().callback(move |event:InputEvent|Msg::Field(key, event.target_unchecked_into::<HtmlInputElement>().value()))}/>
                </label>
            }
        };
        let github = draft.kind != "folder";
        html! {
            <div class="source-settings-add" role="group" aria-label="Add source" data-testid="source-add-form">
                {field("name", "Name", &draft.name, "Team")}
                <label class="source-settings-field">
                    <span>{"Kind"}</span>
                    <select data-testid="source-add-kind" value={draft.kind.clone()}
                        onchange={ctx.link().callback(|event:Event|Msg::Field("kind", event.target_unchecked_into::<HtmlSelectElement>().value()))}>
                        <option value="github" selected={github}>{"GitHub repository"}</option>
                        <option value="folder" selected={!github}>{"Folder"}</option>
                    </select>
                </label>
                if github {
                    {field("repository", "Repository", &draft.repository, "owner/repo")}
                    {field("branch", "Branch", &draft.branch, "default branch")}
                    {field("path", "Folder in it", &draft.path, "repository root")}
                } else {
                    {field("path", "Folder", &draft.path, "/path/to/folder")}
                }
                <div class="console-settings-actions">
                    <button type="button" class="tab-button" data-testid="source-add-save" disabled={self.busy}
                        onclick={ctx.link().callback(|_| Msg::Add)}>{"Add"}</button>
                    <button type="button" class="tab-button" data-testid="source-add-cancel"
                        onclick={ctx.link().callback(|_| Msg::Adding(false))}>{"Cancel"}</button>
                </div>
            </div>
        }
    }
}

impl Component for SourcesSettings {
    type Message = Msg;
    type Properties = Props;

    fn create(ctx: &Context<Self>) -> Self {
        ctx.props().transport.request(
            json!({"service":"sources","op":"get"}),
            false,
            ctx.link().callback(Msg::Loaded),
        );
        Self {
            sources: Vec::new(),
            token_set: false,
            warning: None,
            loaded: false,
            busy: true,
            renaming: None,
            removing: None,
            draft: None,
            token: String::new(),
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Loaded(reply) => {
                self.busy = false;
                self.loaded = true;
                match &reply {
                    Reply::Result(value) => self.load(value),
                    _ => ctx.props().onstatus.emit(OperationStatus::event(
                        "Load sources",
                        js_sys::Date::now(),
                        Some(&reply),
                    )),
                }
            }
            Msg::Move(index, up) => {
                let other = if up {
                    index.checked_sub(1)
                } else {
                    Some(index + 1)
                };
                let Some(other) = other.filter(|other| *other < self.sources.len()) else {
                    return false;
                };
                let mut list = self.sources.clone();
                list.swap(index, other);
                self.save(ctx, "Reorder sources", list);
            }
            Msg::Rename(index) => {
                self.renaming =
                    index.and_then(|index| Some((index, text(self.sources.get(index)?, "name"))));
            }
            Msg::RenameInput(input) => {
                if let Some((_, name)) = &mut self.renaming {
                    *name = input;
                }
            }
            Msg::RenameSave => {
                let Some((index, name)) = self.renaming.clone() else {
                    return false;
                };
                let mut list = self.sources.clone();
                let Some(source) = list.get_mut(index) else {
                    return false;
                };
                source["name"] = json!(name);
                self.save(ctx, "Rename source", list);
            }
            Msg::Remove(index) => self.removing = index,
            Msg::RemoveConfirm => {
                let Some(index) = self.removing.filter(|index| *index < self.sources.len()) else {
                    return false;
                };
                let mut list = self.sources.clone();
                list.remove(index);
                self.save(ctx, "Remove source", list);
            }
            Msg::Adding(adding) => {
                self.draft = adding.then(|| Draft {
                    kind: "github".into(),
                    ..Draft::default()
                });
            }
            Msg::Field(key, value) => {
                let Some(draft) = &mut self.draft else {
                    return false;
                };
                match key {
                    "name" => draft.name = value,
                    "kind" => draft.kind = value,
                    "repository" => draft.repository = value,
                    "branch" => draft.branch = value,
                    _ => draft.path = value,
                }
            }
            Msg::Add => {
                let Some(draft) = self.draft.clone() else {
                    return false;
                };
                let entry = if draft.kind == "folder" {
                    json!({"name":draft.name,"kind":"folder","path":draft.path})
                } else {
                    json!({"name":draft.name,"kind":"github","repository":draft.repository,
                        "branch":draft.branch,"path":draft.path})
                };
                let mut list = self.sources.clone();
                list.push(entry);
                self.save(ctx, "Add source", list);
            }
            Msg::TokenInput(token) => self.token = token,
            Msg::Token(save) => {
                let token = save.then(|| self.token.trim().to_owned());
                let operation = if save {
                    "Save GitHub token"
                } else {
                    "Clear GitHub token"
                };
                self.request(
                    ctx,
                    operation,
                    json!({"service":"sources","op":"token","token":token}),
                );
            }
            Msg::Apply(slug) => self.request(
                ctx,
                "Apply source settings",
                json!({"service":"sources","op":"apply","source":slug}),
            ),
            Msg::Done(operation, started, reply) => {
                self.busy = false;
                let status = OperationStatus::event(operation, started, Some(&reply));
                if let Reply::Result(value) = &reply {
                    self.load(value);
                    match operation {
                        "Add source" => self.draft = None,
                        "Rename source" => self.renaming = None,
                        "Remove source" => self.removing = None,
                        "Save GitHub token" | "Clear GitHub token" => self.token.clear(),
                        _ => {}
                    }
                    // Applying, reordering, renaming or removing a source changes what's in effect.
                    if !matches!(
                        operation,
                        "Add source" | "Save GitHub token" | "Clear GitHub token"
                    ) {
                        ctx.props().onapplied.emit(());
                    }
                    ctx.props().oncheck.emit(false);
                }
                ctx.props().onstatus.emit(status);
            }
        }
        true
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        html! {
            <div class="source-settings" data-testid="source-settings">
                <strong>{"Sources"}</strong>
                <span class="agent-skills-hint">{"Modules and shared settings (dictionary, templates, hints, skills) come from these sources. Later sources override earlier ones; your own changes override them all."}</span>
                if let Some(warning) = &self.warning {
                    <p class="source-settings-error" data-testid="sources-warning">{warning}</p>
                }
                <ul class="module-settings-list">
                    {for self.sources.iter().enumerate().map(|(index, source)| self.view_source(ctx, index, source))}
                </ul>
                if let Some(draft) = &self.draft {
                    {self.view_draft(ctx, draft)}
                }
                <div class="console-settings-actions">
                    if self.draft.is_none() {
                        <button type="button" class="tab-button" data-testid="source-add" disabled={self.busy || !self.loaded}
                            onclick={ctx.link().callback(|_| Msg::Adding(true))}>{"Add source…"}</button>
                    }
                    <button type="button" class="tab-button" data-testid="sources-check" disabled={ctx.props().checking}
                        onclick={ctx.props().oncheck.reform(|_| true)}>{if ctx.props().checking {"Checking…"} else {"Check now"}}</button>
                </div>
                <label for="sources-token">{"GitHub token"}</label>
                <div class="agent-skills-folder">
                    <input id="sources-token" type="password" data-testid="sources-token" autocomplete="off" spellcheck="false"
                        placeholder={if self.token_set {"Saved; enter another to replace it"} else {"Optional"}}
                        value={self.token.clone()} disabled={!self.loaded}
                        oninput={ctx.link().callback(|event:InputEvent|Msg::TokenInput(event.target_unchecked_into::<HtmlInputElement>().value()))}/>
                    <button type="button" class="tab-button" data-testid="sources-token-save" disabled={self.busy || self.token.trim().is_empty()}
                        onclick={ctx.link().callback(|_| Msg::Token(true))}>{"Save"}</button>
                    if self.token_set {
                        <button type="button" class="tab-button" data-testid="sources-token-clear" disabled={self.busy}
                            onclick={ctx.link().callback(|_| Msg::Token(false))}>{"Clear"}</button>
                    }
                </div>
                <span class="agent-skills-hint">{"For private repositories and higher GitHub limits. Without a saved token, the token of a signed-in GitHub CLI (gh auth token) is used."}</span>
            </div>
        }
    }
}
