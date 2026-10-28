use super::{call, error, get};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use yew::Callback;

const SCRIPT: &str = include_str!("../../assets/page-agent.js");
pub fn is_tool(tool: &str) -> bool {
    matches!(
        tool,
        "page_snapshot"
            | "page_click"
            | "page_fill"
            | "page_select"
            | "page_scroll"
            | "page_guide_show"
            | "page_guide_clear"
    )
}

#[derive(Clone)]
pub struct Request {
    pub session: String,
    pub turn: u32,
    pub tool: String,
    pub arguments: Value,
    pub reply: Callback<Value>,
}
#[derive(Clone)]
pub struct Approval {
    pub session: String,
    pub origin: String,
    pub url: String,
    pub title: String,
}
struct Pending {
    request: Request,
    approval: Approval,
}
#[derive(Clone)]
struct Guide {
    turn: u32,
    id: String,
    origin: String,
    page_id: Value,
}
#[derive(Default)]
struct State {
    active: BTreeSet<u32>,
    denied: BTreeSet<u32>,
    grants: BTreeMap<u32, String>,
    pending: Option<Pending>,
    busy: bool,
    guided: BTreeSet<u32>,
    guide: Option<Guide>,
}
#[derive(Clone, Default)]
pub struct Controller(Rc<RefCell<State>>);
impl Controller {
    pub fn start(&self, turn: u32) {
        self.0.borrow_mut().active.insert(turn);
    }
    pub fn revoke(&self, turn: u32) {
        let mut state = self.0.borrow_mut();
        state.active.remove(&turn);
        state.denied.remove(&turn);
        state.grants.remove(&turn);
        state.guided.remove(&turn);
        let guide = if state.guide.as_ref().is_some_and(|guide| guide.turn == turn) {
            state.guide.take()
        } else {
            None
        };
        let pending = if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.request.turn == turn)
        {
            state.pending.take()
        } else {
            None
        };
        drop(state);
        if let Some(guide) = guide {
            let controller = self.clone();
            spawn_local(async move {
                if let Err(error) = controller.clear_guide(&guide).await {
                    web_sys::console::error_1(
                        &format!("Could not remove page guide: {error}").into(),
                    );
                }
            });
        }
        if let Some(pending) = pending {
            pending
                .request
                .reply
                .emit(json!({"error":"Page access revoked; Agent turn ended or stopped."}));
        }
    }
    pub fn shutdown(&self) {
        let turns = self.0.borrow().active.iter().copied().collect::<Vec<_>>();
        for turn in turns {
            self.revoke(turn);
        }
    }
    pub fn approval(&self, session: &str) -> Option<Approval> {
        self.0
            .borrow()
            .pending
            .as_ref()
            .filter(|pending| pending.approval.session == session)
            .map(|pending| pending.approval.clone())
    }
    pub fn decide(&self, session: &str, approve: bool, guide_only: bool, changed: Callback<()>) {
        let mut state = self.0.borrow_mut();
        if state
            .pending
            .as_ref()
            .is_none_or(|pending| pending.approval.session != session)
        {
            return;
        }
        let pending = state.pending.take().unwrap();
        if approve {
            if guide_only {
                state.guided.insert(pending.request.turn);
            }
            state
                .grants
                .insert(pending.request.turn, pending.approval.origin);
        } else {
            state.denied.insert(pending.request.turn);
        }
        drop(state);
        self.request(pending.request, changed);
    }
    pub fn request(&self, request: Request, changed: Callback<()>) {
        let controller = self.clone();
        spawn_local(async move {
            let result = controller.perform(&request).await;
            match result {
                Ok(Some(value)) => request.reply.emit(value),
                Ok(None) => {}
                Err(message) => request.reply.emit(json!({"error":message})),
            }
            changed.emit(());
        });
    }
    fn check(&self, turn: u32) -> Result<(), String> {
        let state = self.0.borrow();
        if !state.active.contains(&turn) {
            Err("Page access revoked; Agent turn ended or stopped.".into())
        } else if state.denied.contains(&turn) {
            Err("Page access was denied for this Agent turn.".into())
        } else {
            Ok(())
        }
    }
    async fn perform(&self, request: &Request) -> Result<Option<Value>, String> {
        self.check(request.turn)?;
        if !is_tool(&request.tool) {
            return Err("Unknown page tool.".into());
        }
        if request.tool == "page_guide_clear" {
            let guide = self.0.borrow().guide.clone();
            return match guide {
                Some(guide) if guide.turn == request.turn => {
                    self.clear_guide(&guide).await?;
                    Ok(Some(json!({"cleared":true})))
                }
                Some(_) => Err("Another Agent turn owns the active page guide.".into()),
                None => Ok(Some(json!({"cleared":false}))),
            };
        }
        {
            let mut state = self.0.borrow_mut();
            if request.tool == "page_guide_show" {
                state.guided.insert(request.turn);
            }
            if state.guided.contains(&request.turn)
                && matches!(
                    request.tool.as_str(),
                    "page_click" | "page_fill" | "page_select"
                )
            {
                return Err("This turn is in guide-only mode. The user must click, fill and select on the website themselves.".into());
            }
            if state.busy {
                return Err("Another page operation is in progress; wait before retrying.".into());
            }
            state.busy = true;
        }
        let result = self.perform_inner(request).await;
        self.0.borrow_mut().busy = false;
        result
    }
    async fn perform_inner(&self, request: &Request) -> Result<Option<Value>, String> {
        let metadata = evaluate(&json!({"tool":"metadata"})).await?;
        self.check(request.turn)?;
        let origin = metadata["origin"]
            .as_str()
            .ok_or("Page origin is unavailable.")?;
        let parsed = url::Url::parse(origin).map_err(|_| "Page origin is invalid.")?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.origin().ascii_serialization() != origin
        {
            return Err("Page tools require an inspected HTTP(S) page.".into());
        }
        {
            let mut state = self.0.borrow_mut();
            if state.grants.get(&request.turn).map(String::as_str) != Some(origin) {
                if state.pending.is_some() {
                    return Err("Page approval is already pending; wait before retrying.".into());
                }
                state.pending = Some(Pending {
                    request: request.clone(),
                    approval: Approval {
                        session: request.session.clone(),
                        origin: origin.into(),
                        url: metadata["url"]
                            .as_str()
                            .ok_or("Page URL is unavailable.")?
                            .into(),
                        title: metadata["title"].as_str().unwrap_or("").into(),
                    },
                });
                return Ok(None);
            }
        }
        self.check(request.turn)?;
        let mut args = request.arguments.clone();
        let object = args
            .as_object_mut()
            .ok_or("Page arguments must be an object.")?;
        object.insert("tool".into(), json!(request.tool));
        object.insert("origin".into(), json!(origin));
        if request.tool == "page_guide_show" {
            let guide = Guide {
                turn: request.turn,
                id: super::agent_panel::id()?,
                origin: origin.into(),
                page_id: args["pageId"].clone(),
            };
            if self.0.borrow().guide.is_some() {
                return Err("Another page guide is already active.".into());
            }
            args["guideId"] = json!(guide.id);
            self.0.borrow_mut().guide = Some(guide.clone());
            let result = async {
                let shown = evaluate(&args).await?;
                self.check(request.turn)?;
                if shown["shown"] != true {
                    return Err("The inspected page did not confirm the guide overlay.".into());
                }
                self.wait_guide(&guide).await
            }
            .await;
            let cleanup = self.clear_guide(&guide).await;
            return match (result, cleanup) {
                (Ok(value), Ok(())) => Ok(Some(value)),
                (Err(error), Ok(())) => Err(error),
                (Ok(_), Err(error)) => Err(format!("Page guide cleanup failed: {error}")),
                (Err(error), Err(cleanup)) => {
                    Err(format!("{error}; page guide cleanup failed: {cleanup}"))
                }
            };
        }
        let value = evaluate(&args).await?;
        self.check(request.turn)?;
        Ok(Some(value))
    }
    async fn wait_guide(&self, guide: &Guide) -> Result<Value, String> {
        let started = js_sys::Date::now();
        loop {
            self.check(guide.turn)?;
            if js_sys::Date::now() - started > 300_000.0 {
                return Err("Page guide timed out after five minutes without a Next, Back or Finish decision.".into());
            }
            let value = evaluate(&json!({"tool":"guide_status","origin":guide.origin,
                "pageId":guide.page_id,"guideId":guide.id}))
            .await?;
            self.check(guide.turn)?;
            match value["action"].as_str() {
                Some("waiting") => {}
                Some(
                    "next" | "back" | "finish" | "dismissed" | "target_lost" | "disconnected"
                    | "missing",
                ) => return Ok(value),
                _ => return Err("Invalid page guide action reply.".into()),
            }
            let delay = js_sys::Promise::new(&mut |resolve, reject| {
                if let Err(error) = super::window()
                    .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 250)
                {
                    let _ = reject.call1(&JsValue::NULL, &error);
                }
            });
            JsFuture::from(delay).await.map_err(error)?;
        }
    }
    async fn clear_guide(&self, guide: &Guide) -> Result<(), String> {
        evaluate(&json!({"tool":"guide_cleanup","guideId":guide.id})).await?;
        let mut state = self.0.borrow_mut();
        if state
            .guide
            .as_ref()
            .is_some_and(|current| current.id == guide.id)
        {
            state.guide = None;
        }
        Ok(())
    }
}

async fn evaluate(args: &Value) -> Result<Value, String> {
    let chrome = get(&js_sys::global(), "chrome");
    let inspected = get(&get(&chrome, "devtools"), "inspectedWindow");
    if !get(&inspected, "eval").is_function() || get(&inspected, "tabId").as_f64().is_none() {
        return Err("Page tools are available only in the DevTools extension attached to a browser tab, not in standalone Console.".into());
    }
    let expression = format!("{SCRIPT}({args})");
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let fail = reject.clone();
        let callback = Closure::once_into_js(move |value: JsValue, exception: JsValue| {
            if get(&exception, "isException").as_bool() == Some(true)
                || get(&exception, "isError").as_bool() == Some(true)
            {
                let message = get(&exception, "value").as_string()
                    .or_else(|| get(&exception, "description").as_string())
                    .unwrap_or_else(|| "Inspected page evaluation failed; navigation may have occurred. Read a fresh snapshot before retrying.".into());
                let _ = reject.call1(&JsValue::NULL, &message.into());
            } else {
                let _ = resolve.call1(&JsValue::NULL, &value);
            }
        });
        if let Err(error) = call(&inspected, "eval", &[expression.clone().into(), callback]) {
            let _ = fail.call1(&JsValue::NULL, &error);
        }
    });
    let window = super::window();
    let mut timer = None;
    let deadline = js_sys::Promise::new(&mut |_, reject| {
        let fail = reject.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            let _ = fail.call1(&JsValue::NULL, &"Inspected page did not respond within 15 seconds. Navigation may have occurred; inspect fresh state before retrying.".into());
        });
        match window.set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            15_000,
        ) {
            Ok(id) => timer = Some((id, callback)),
            Err(error) => {
                let _ = reject.call1(&JsValue::NULL, &error);
            }
        }
    });
    let value = JsFuture::from(js_sys::Promise::race(&js_sys::Array::of2(
        &promise, &deadline,
    )))
    .await;
    if let Some((id, _callback)) = timer {
        window.clear_timeout_with_handle(id);
    }
    let value = value.map_err(error)?;
    serde_wasm_bindgen::from_value(value)
        .map_err(|error| format!("Invalid inspected page reply: {error}"))
}
