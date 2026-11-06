//! A module's wasm in the browser: instantiated from its bytes, called through the
//! `sdk_*` exports, restarted after a trap.

use crate::modules::Instance;
use js_sys::{Function, Object, Reflect, Uint8Array, WebAssembly};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};

pub struct Wasm {
    module: WebAssembly::Module,
    /// `None` while a fresh instance replaces one that trapped.
    exports: Rc<RefCell<Option<JsValue>>>,
}

fn text(value: JsValue) -> String {
    value
        .as_string()
        .or_else(|| {
            value
                .dyn_ref::<js_sys::Error>()
                .map(|error| String::from(error.message()))
        })
        .unwrap_or_else(|| format!("{value:?}"))
}

fn get(target: &JsValue, key: &str) -> Result<JsValue, String> {
    Reflect::get(target, &key.into()).map_err(text)
}

fn invoke(exports: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let function: Function = Reflect::get(exports, &name.into())?.dyn_into()?;
    let args: js_sys::Array = args.iter().collect();
    function.apply(&JsValue::UNDEFINED, &args)
}

/// Compiles and starts a module; refuses one built for another ABI.
pub async fn instantiate(bytes: &[u8]) -> Result<Wasm, String> {
    let source = JsFuture::from(WebAssembly::instantiate_buffer(bytes, &Object::new()))
        .await
        .map_err(text)?;
    let module: WebAssembly::Module = get(&source, "module")?.dyn_into().map_err(text)?;
    let exports = get(&get(&source, "instance")?, "exports")?;
    let abi = invoke(&exports, "sdk_abi", &[]).map_err(|_| {
        "it exports no sdk_abi; was it built with services_sdk::export!?".to_owned()
    })?;
    if abi.as_f64() != Some(f64::from(services_sdk::ABI)) {
        return Err(format!(
            "it was built for module ABI {abi:?}, the App runs ABI {}",
            services_sdk::ABI
        ));
    }
    Ok(Wasm {
        module,
        exports: Rc::new(RefCell::new(Some(exports))),
    })
}

fn exchange(exports: &JsValue, request: &[u8]) -> Result<Vec<u8>, JsValue> {
    // Pointers come back as signed 32-bit numbers.
    let pointer = |value: JsValue| {
        value
            .as_f64()
            .map(|number| number as i64 as u32)
            .ok_or_else(|| JsValue::from_str("The module returned no pointer"))
    };
    let memory: WebAssembly::Memory = Reflect::get(exports, &"memory".into())?.dyn_into()?;
    let len = request.len() as u32;
    let at = pointer(invoke(exports, "sdk_alloc", &[len.into()])?)?;
    Uint8Array::new_with_byte_offset_and_length(&memory.buffer(), at, len).copy_from(request);
    let answer = pointer(invoke(exports, "sdk_call", &[at.into(), len.into()])?)?;
    // The call may have grown the memory: take a fresh view.
    let view = Uint8Array::new(&memory.buffer());
    let mut header = [0; 4];
    view.subarray(answer, answer + 4).copy_to(&mut header);
    let mut bytes = vec![0; u32::from_le_bytes(header) as usize];
    view.subarray(answer + 4, answer + 4 + bytes.len() as u32)
        .copy_to(&mut bytes);
    invoke(exports, "sdk_free", &[answer.into()])?;
    Ok(bytes)
}

impl Wasm {
    fn restart(&self) {
        *self.exports.borrow_mut() = None;
        let module = self.module.clone();
        let slot = self.exports.clone();
        spawn_local(async move {
            match JsFuture::from(WebAssembly::instantiate_module(&module, &Object::new())).await {
                Ok(instance) => match get(&instance, "exports") {
                    Ok(exports) => *slot.borrow_mut() = Some(exports),
                    Err(failure) => web_sys_log(&failure),
                },
                Err(failure) => web_sys_log(&text(failure)),
            }
        });
    }
}

fn web_sys_log(message: &str) {
    let console = js_sys::global();
    if let Ok(console) = Reflect::get(&console, &"console".into())
        && let Ok(error) = Reflect::get(&console, &"error".into())
        && let Ok(error) = error.dyn_into::<Function>()
    {
        let _ = error.call1(&console, &message.into());
    }
}

impl Instance for Wasm {
    fn call(&self, request: &[u8]) -> Result<Vec<u8>, String> {
        let exports = self
            .exports
            .borrow()
            .clone()
            .ok_or("the module is restarting; try again")?;
        exchange(&exports, request).map_err(|failure| {
            self.restart();
            text(failure)
        })
    }
}
