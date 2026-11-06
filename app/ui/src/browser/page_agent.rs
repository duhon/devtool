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
/// Global keys that cache the injected page agent and its content version on the inspected page.
const AGENT_KEY: &str = "__servicesDevtoolsPageAgentFn";
const VERSION_KEY: &str = "__servicesDevtoolsPageAgentVersion";
/// Sentinel the fast path returns when the cached agent is missing or stale and must be re-injected.
const NEEDS_INJECTION: &str = "__servicesDevtoolsPageAgentReinject";
/// Content hash of the agent script, so an updated Console re-injects instead of reusing a stale copy.
const SCRIPT_VERSION: u64 = fnv1a(SCRIPT.as_bytes());
const fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}
pub fn is_tool(tool: &str) -> bool {
    matches!(
        tool,
        "page_snapshot"
            | "page_inspect"
            | "page_read"
            | "page_search"
            | "page_options"
            | "page_context"
            | "page_errors"
            | "page_network"
            | "page_wait"
            | "page_click"
            | "page_fill"
            | "page_select"
            | "page_focus"
            | "page_set_checked"
            | "page_press_key"
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
    pub page_id: Value,
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
    grant: Option<(String, Value)>,
    access: BTreeMap<u32, String>,
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
        let access = state.access.remove(&turn);
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
        if let Some(access) = access {
            spawn_local(async move {
                if let Err(error) =
                    evaluate(&json!({"tool":"access_cleanup","accessId":access})).await
                {
                    web_sys::console::error_1(
                        &format!("Could not release page inspection: {error}").into(),
                    );
                }
            });
        }
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
            } else {
                state.grant = Some((pending.approval.origin, pending.approval.page_id));
            }
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
                    "page_click"
                        | "page_fill"
                        | "page_select"
                        | "page_focus"
                        | "page_set_checked"
                        | "page_press_key"
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
        let writes = matches!(
            request.tool.as_str(),
            "page_click"
                | "page_fill"
                | "page_select"
                | "page_focus"
                | "page_set_checked"
                | "page_press_key"
        );
        {
            let mut state = self.0.borrow_mut();
            let identity = (origin.to_owned(), metadata["pageId"].clone());
            if state.grant.as_ref().is_some_and(|grant| grant != &identity) {
                state.grant = None;
            }
            if writes && request.arguments["pageId"] != metadata["pageId"] {
                return Err("The document changed. Read a fresh snapshot before acting.".into());
            }
            if writes && state.denied.contains(&request.turn) {
                return Err(
                    "Page writing was denied for this Agent turn; reading remains available."
                        .into(),
                );
            }
            if writes && state.grant.as_ref() != Some(&identity) {
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
                        page_id: metadata["pageId"].clone(),
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
        let access = {
            let mut state = self.0.borrow_mut();
            if let std::collections::btree_map::Entry::Vacant(entry) =
                state.access.entry(request.turn)
            {
                entry.insert(super::agent_panel::id()?);
            }
            state.access[&request.turn].clone()
        };
        args["accessId"] = json!(access);
        if request.tool == "page_network" {
            if args["pageId"] != metadata["pageId"] {
                return Err(
                    "The document changed. Read a fresh snapshot before reading Network.".into(),
                );
            }
            let value = read_network(&args, &metadata).await?;
            self.check(request.turn)?;
            let current = evaluate(&json!({"tool":"metadata"})).await?;
            self.check(request.turn)?;
            if current["origin"] != metadata["origin"] || current["pageId"] != metadata["pageId"] {
                return Err(
                    "The document changed while reading Network; inspect fresh state.".into(),
                );
            }
            return Ok(Some(value));
        }
        if request.tool == "page_wait" {
            let timeout = match args.get("timeoutMs") {
                None => 5000,
                Some(value) => value
                    .as_u64()
                    .filter(|value| *value <= 10_000)
                    .ok_or("timeoutMs must be an integer from 0 to 10000.")?,
            };
            let started = js_sys::Date::now();
            loop {
                self.check(request.turn)?;
                let value = evaluate(&args).await?;
                self.check(request.turn)?;
                if value["matched"] == true {
                    return Ok(Some(value));
                }
                if js_sys::Date::now() - started >= timeout as f64 {
                    return Err(format!(
                        "Page wait timed out after {timeout} ms; the requested state was not observed."
                    ));
                }
                delay(100).await?;
            }
        }
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
            delay(250).await?;
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

async fn delay(milliseconds: i32) -> Result<(), String> {
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        if let Err(error) = super::window()
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, milliseconds)
        {
            let _ = reject.call1(&JsValue::NULL, &error);
        }
    });
    JsFuture::from(promise).await.map_err(error)?;
    Ok(())
}

async fn read_network(args: &Value, metadata: &Value) -> Result<Value, String> {
    let range = |name, default, maximum| match args.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|value| *value <= maximum)
            .ok_or_else(|| format!("Invalid Network {name}.")),
    };
    let offset = range("offset", 0, u32::MAX.into())? as usize;
    let limit = range("limit", 20, 100)? as usize;
    if limit == 0 {
        return Err("Network limit must be positive.".into());
    }
    let query = match args.get("query") {
        None => String::new(),
        Some(value) => value
            .as_str()
            .filter(|value| value.chars().count() <= 200)
            .ok_or("Network query must be a string of at most 200 characters.")?
            .to_lowercase(),
    };
    let network = get(
        &get(&get(&js_sys::global(), "chrome"), "devtools"),
        "network",
    );
    if !get(&network, "getHAR").is_function() {
        return Err("Network inspection is unavailable in this DevTools context.".into());
    }
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let callback = Closure::once_into_js(move |value: JsValue| {
            let _ = resolve.call1(&JsValue::NULL, &value);
        });
        if let Err(error) = call(&network, "getHAR", &[callback]) {
            let _ = reject.call1(&JsValue::NULL, &error);
        }
    });
    let har = await_reply(promise).await?;
    let entries = get(&har, "entries");
    if !js_sys::Array::is_array(&entries) {
        return Err("DevTools returned an invalid Network log.".into());
    }
    let time_origin = metadata["pageId"]
        .as_str()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .ok_or("The inspected document has no navigation time.")?;
    let mut items = Vec::new();
    let mut omitted = 0;
    for entry in js_sys::Array::from(&entries).iter() {
        let started = get(&entry, "startedDateTime")
            .as_string()
            .map(|value| js_sys::Date::parse(&value))
            .filter(|value| value.is_finite());
        if started.is_none_or(|value| value < time_origin.floor()) {
            omitted += 1;
            continue;
        }
        let request = get(&entry, "request");
        let Some(url) = get(&request, "url").as_string() else {
            return Err("DevTools returned a Network request without a URL.".into());
        };
        let url = url::Url::parse(&url).map_err(|_| "DevTools returned an invalid request URL.")?;
        if !matches!(url.scheme(), "http" | "https") {
            omitted += 1;
            continue;
        }
        let sanitized = format!("{}{}", url.origin().ascii_serialization(), url.path());
        if !sanitized.to_lowercase().contains(&query) {
            continue;
        }
        let response = get(&entry, "response");
        let method = get(&request, "method")
            .as_string()
            .ok_or("DevTools returned a Network request without a method.")?;
        items.push(json!({
            "url": sanitized.chars().take(600).collect::<String>(),
            "urlTruncated":sanitized.chars().count()>600,
            "method":method.chars().take(20).collect::<String>(),
            "methodTruncated":method.chars().count()>20,
            "status":get(&response,"status").as_f64(),
            "durationMs":get(&entry,"time").as_f64(),
            "bodySize":get(&response,"bodySize").as_f64(),
        }));
    }
    if offset > items.len() {
        return Err("Network offset is out of range.".into());
    }
    let mut result = json!({"pageId":metadata["pageId"],"origin":metadata["origin"],"untrusted":true,
        "offset":offset,"total":items.len(),"items":[],"nextOffset":null,"omittedRequests":omitted,
        "limitations":["Only requests recorded by DevTools since this document's navigation; earlier history may be absent.",
            "No headers, bodies, URL credentials, query strings or fragments. The Network log is live; entries may change.",
            "Earlier/unattributed navigation timestamps and non-HTTP(S) URLs are omitted and counted."]});
    for (index, item) in items.iter().enumerate().skip(offset).take(limit) {
        result["items"].as_array_mut().unwrap().push(item.clone());
        result["nextOffset"] = if index + 1 < items.len() {
            json!(index + 1)
        } else {
            Value::Null
        };
        if result.to_string().len() > 6000 {
            result["items"].as_array_mut().unwrap().pop();
            result["nextOffset"] = json!(index);
            break;
        }
    }
    Ok(result)
}

async fn evaluate(args: &Value) -> Result<Value, String> {
    let chrome = get(&js_sys::global(), "chrome");
    let inspected = get(&get(&chrome, "devtools"), "inspectedWindow");
    if !get(&inspected, "eval").is_function() || get(&inspected, "tabId").as_f64().is_none() {
        return Err("Page tools are available only in the DevTools extension attached to a browser tab, not in standalone Console.".into());
    }
    // The ~40 KB agent is injected once per document and reused; a content version guard re-injects
    // only after navigation wipes the page globals or an update changes the script.
    let probe = format!(
        "(function(a){{var g=globalThis;return g.{VERSION_KEY}==={SCRIPT_VERSION}?g.{AGENT_KEY}(a):{{{NEEDS_INJECTION}:true}};}})({args})"
    );
    let value = eval_inspected(&inspected, &probe).await?;
    let value = if get(&value, NEEDS_INJECTION).as_bool() == Some(true) {
        let inject = format!(
            "(function(a){{var g=globalThis;g.{AGENT_KEY}={SCRIPT};g.{VERSION_KEY}={SCRIPT_VERSION};return g.{AGENT_KEY}(a);}})({args})"
        );
        eval_inspected(&inspected, &inject).await?
    } else {
        value
    };
    serde_wasm_bindgen::from_value(value)
        .map_err(|error| format!("Invalid inspected page reply: {error}"))
}

async fn eval_inspected(inspected: &JsValue, expression: &str) -> Result<JsValue, String> {
    let expression = expression.to_owned();
    let inspected = inspected.clone();
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
    await_reply(promise).await
}

async fn await_reply(promise: js_sys::Promise) -> Result<JsValue, String> {
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
    value.map_err(error)
}
