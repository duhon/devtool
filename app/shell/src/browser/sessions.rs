//! The Host component: renders the Kernel's [`Startup`] of the window — Preparing/error
//! banners, then the App on the current session — and forwards Retry and session choices.

use super::{app::App, on};
use services_kernel::startup::{Browser, Startup};
use services_kernel::workspace::store::Shared;
use std::rc::Rc;
use yew::prelude::*;

#[derive(Properties, Clone)]
pub struct AppProps {
    pub sessions: Shared,
    pub session_id: u64,
    pub onselect: Callback<u64>,
    pub onretry: Callback<()>,
    pub storage_epoch: u64,
}
impl PartialEq for AppProps {
    fn eq(&self, other: &Self) -> bool {
        self.session_id == other.session_id
            && self.storage_epoch == other.storage_epoch
            && Rc::ptr_eq(&self.sessions, &other.sessions)
    }
}
pub enum Msg {
    /// The start changed: render it again.
    Startup,
    Select(u64),
    Retry,
}
pub struct Host {
    startup: Startup<Browser>,
}
impl Component for Host {
    type Message = Msg;
    type Properties = ();
    fn create(ctx: &Context<Self>) -> Self {
        Self {
            startup: Startup::browser(on(ctx.link().callback(|_| Msg::Startup))),
        }
    }
    fn update(&mut self, _: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Startup => {}
            Msg::Select(id) => self.startup.select(id),
            Msg::Retry => self.startup.retry(),
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let view = self.startup.view();
        if let Some(store) = view.workspace {
            html! {<>
                if let Some(error)=&view.error {<div class="banner banner-error" role="alert">{error}<button onclick={ctx.link().callback(|_|Msg::Retry)}>{"Retry"}</button></div>}
                <App key={format!("{}:{}",view.active,view.epoch)} sessions={store} session_id={view.active} storage_epoch={view.storage_epoch} onselect={ctx.link().callback(Msg::Select)} onretry={ctx.link().callback(|_|Msg::Retry)}/>
            </>}
        } else {
            html! {<div class="banner" role="status">{
                view.error.as_deref().unwrap_or("Preparing sessions…")
            }if view.error.is_some(){<button onclick={ctx.link().callback(|_|Msg::Retry)}>{"Retry"}</button>}</div>}
        }
    }
}
