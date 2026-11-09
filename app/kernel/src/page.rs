//! Code run on the page DevTools inspects (`chrome.devtools.inspectedWindow.eval`) and the
//! requests DevTools recorded there.

use crate::js::{call, error, get, window};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;

const UNAVAILABLE: &str = "Page tools are available only in the DevTools extension attached to a browser tab, not in standalone Console.";

fn inspected() -> Result<JsValue, String> {
    let inspected = get(
        &get(&get(&js_sys::global(), "chrome"), "devtools"),
        "inspectedWindow",
    );
    if !get(&inspected, "eval").is_function() || get(&inspected, "tabId").as_f64().is_none() {
        return Err(UNAVAILABLE.into());
    }
    Ok(inspected)
}

/// Evaluates `expression` on the inspected page; the answer is its JSON value.
pub async fn eval(expression: &str) -> Result<Value, String> {
    let inspected = inspected()?;
    let expression = expression.to_owned();
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
    let value = await_reply(promise).await?;
    serde_wasm_bindgen::from_value(value)
        .map_err(|error| format!("Invalid inspected page reply: {error}"))
}

/// The requests DevTools recorded: `{entries: [{url, method, status, time, bodySize, started}]}`.
pub async fn network() -> Result<Value, String> {
    inspected()?;
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
    let entries: Vec<Value> = js_sys::Array::from(&entries)
        .iter()
        .map(|entry| {
            let request = get(&entry, "request");
            let response = get(&entry, "response");
            json!({
                "url": get(&request, "url").as_string(),
                "method": get(&request, "method").as_string(),
                "status": get(&response, "status").as_f64(),
                "time": get(&entry, "time").as_f64(),
                "bodySize": get(&response, "bodySize").as_f64(),
                "started": get(&entry, "startedDateTime")
                    .as_string()
                    .map(|value| js_sys::Date::parse(&value))
                    .filter(|value| value.is_finite()),
            })
        })
        .collect();
    Ok(json!({ "entries": entries }))
}

/// Waits for the inspected page at most 15 seconds.
async fn await_reply(promise: js_sys::Promise) -> Result<JsValue, String> {
    let window = window();
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
