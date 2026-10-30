//! Console parts of packaged modules. A manifest with a `ui` hash has a `ui.wasm` built
//! with `services_module_ui::export!`: the panel fetches it from the host once per hash,
//! keeps it in IndexedDB, instantiates it and attaches it to the service registry.

use super::session_database::request;
use super::transport::{Reply, Transport};
use super::{call, error, get, set, window};
use base64::Engine;
use js_sys::{Array, Object, Promise, Uint8Array, WebAssembly};
use serde_json::{Value, json};
use services_module_ui::{Imported, Remote, Sections, ServiceUi};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};
use web_sys::{IdbDatabase, IdbTransactionMode};
use yew::Callback;

const DATABASE: &str = "servicesConsoleModules";
const STORE: &str = "ui";

struct Part {
    hash: String,
    module: WebAssembly::Module,
    /// `None` while a fresh instance replaces one that trapped.
    exports: Option<JsValue>,
}

thread_local! {
    static PARTS: RefCell<BTreeMap<String, Part>> = RefCell::default();
}

/// Loads the console parts that registered modules declare and the panel lacks (or has
/// at another hash), then calls `done`. A part that fails to load reports why on use.
pub fn prepare(transport: &Rc<Transport>, done: Callback<()>) {
    let wanted: Vec<(String, String, String)> = crate::services::all()
        .iter()
        .filter_map(|manifest| {
            let hash = manifest.ui.clone()?;
            let current = PARTS.with(|parts| {
                parts
                    .borrow()
                    .get(&manifest.id)
                    .is_some_and(|part| part.hash == hash)
            });
            (!current).then(|| (manifest.id.clone(), manifest.name.clone(), hash))
        })
        .collect();
    if wanted.is_empty() {
        done.emit(());
        return;
    }
    let transport = transport.clone();
    spawn_local(async move {
        let cache = match open().await {
            Ok(database) => Some(Rc::new(database)),
            Err(failure) => {
                web_sys::console::warn_1(
                    &format!("Console parts are not cached: {failure}").into(),
                );
                None
            }
        };
        let loads = Array::new();
        for (id, name, hash) in wanted {
            let cache = cache.clone();
            let transport = transport.clone();
            loads.push(&future_to_promise(async move {
                if let Err(failure) = load(cache.as_deref(), &transport, &id, &hash).await {
                    let failure = format!("The {name} console part failed to load: {failure}");
                    web_sys::console::error_1(&failure.clone().into());
                    crate::services::attach(&id, Box::leak(Box::new(Unavailable(failure))));
                }
                Ok(JsValue::UNDEFINED)
            }));
        }
        let _ = JsFuture::from(Promise::all(&loads)).await;
        if let Some(cache) = cache {
            cache.close();
        }
        done.emit(());
    });
}

async fn load(
    cache: Option<&IdbDatabase>,
    transport: &Rc<Transport>,
    id: &str,
    hash: &str,
) -> Result<(), String> {
    let cached = match cache {
        Some(database) => read(database, id, hash).await.unwrap_or_else(|failure| {
            web_sys::console::warn_1(&failure.into());
            None
        }),
        None => None,
    };
    let (hash, bytes) = match cached {
        Some(bytes) => (hash.to_owned(), bytes),
        None => {
            let (hash, bytes) = fetch(transport, id).await?;
            if let Some(database) = cache
                && let Err(failure) = write(database, id, &hash, &bytes).await
            {
                web_sys::console::warn_1(&failure.into());
            }
            (hash, bytes)
        }
    };
    let source = JsFuture::from(WebAssembly::instantiate_buffer(&bytes, &Object::new()))
        .await
        .map_err(error)?;
    let module: WebAssembly::Module = get(&source, "module").dyn_into().map_err(error)?;
    let exports = get(&get(&source, "instance"), "exports");
    let abi = call(&exports, "services_ui_abi", &[]).map_err(error)?;
    if abi.as_f64() != Some(f64::from(services_module_ui::ABI_VERSION)) {
        return Err(format!(
            "it was built for console ABI {abi:?}, the panel needs {}",
            services_module_ui::ABI_VERSION
        ));
    }
    PARTS.with(|parts| {
        parts.borrow_mut().insert(
            id.to_owned(),
            Part {
                hash,
                module,
                exports: Some(exports),
            },
        )
    });
    let owner = id.to_owned();
    crate::services::attach(
        id,
        Box::leak(Box::new(Remote(move |request| invoke(&owner, request)))),
    );
    Ok(())
}

/// Asks the host for a module's `ui.wasm`: its hash and bytes.
async fn fetch(transport: &Rc<Transport>, id: &str) -> Result<(String, Vec<u8>), String> {
    let slot = Rc::new(RefCell::new(None));
    let message = json!({"service": "modules", "op": "ui", "module": id});
    let promise = Promise::new(&mut |resolve, _| {
        let slot = slot.clone();
        transport.request(
            message.clone(),
            false,
            Callback::from(move |reply: Reply| {
                if !matches!(reply, Reply::Data(_)) && slot.borrow().is_none() {
                    *slot.borrow_mut() = Some(reply);
                    let _ = resolve.call0(&JsValue::UNDEFINED);
                }
            }),
        );
    });
    JsFuture::from(promise).await.map_err(error)?;
    let reply = slot.borrow_mut().take();
    match reply {
        Some(Reply::Result(value)) => {
            let text = |key: &str| value[key].as_str().ok_or("the host sent no console part");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(text("wasm")?)
                .map_err(|e| e.to_string())?;
            Ok((text("ui")?.to_owned(), bytes))
        }
        Some(Reply::Error(value)) => Err(value["error"]
            .as_str()
            .unwrap_or("the host refused the console part")
            .to_owned()),
        _ => Err("the host sent no console part".into()),
    }
}

async fn open() -> Result<IdbDatabase, String> {
    let factory = window()
        .indexed_db()
        .map_err(error)?
        .ok_or("IndexedDB is unavailable")?;
    let opening = factory.open_with_u32(DATABASE, 1).map_err(error)?;
    let upgrading = opening.clone();
    let upgrade = Closure::<dyn FnMut()>::new(move || {
        let created = upgrading
            .result()
            .and_then(|value| value.dyn_into::<IdbDatabase>())
            .and_then(|database| database.create_object_store(STORE));
        if let Err(failure) = created {
            web_sys::console::error_1(&failure);
        }
    });
    opening.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
    let opened = request(opening.clone().into()).await;
    opening.set_onupgradeneeded(None);
    opened?.dyn_into::<IdbDatabase>().map_err(error)
}

async fn read(database: &IdbDatabase, id: &str, hash: &str) -> Result<Option<Vec<u8>>, String> {
    let store = database
        .transaction_with_str(STORE)
        .and_then(|transaction| transaction.object_store(STORE))
        .map_err(error)?;
    let value = request(store.get(&id.into()).map_err(error)?).await?;
    if get(&value, "ui").as_string().as_deref() != Some(hash) {
        return Ok(None);
    }
    Ok(get(&value, "wasm")
        .dyn_into::<Uint8Array>()
        .ok()
        .map(|bytes| bytes.to_vec()))
}

async fn write(database: &IdbDatabase, id: &str, hash: &str, bytes: &[u8]) -> Result<(), String> {
    let store = database
        .transaction_with_str_and_mode(STORE, IdbTransactionMode::Readwrite)
        .and_then(|transaction| transaction.object_store(STORE))
        .map_err(error)?;
    let value = Object::new();
    set(&value, "ui", &hash.into());
    set(&value, "wasm", &Uint8Array::from(bytes).into());
    request(store.put_with_key(&value, &id.into()).map_err(error)?)
        .await
        .map(drop)
}

/// One console call into a module's instance; a trap restarts the instance.
fn invoke(id: &str, request: String) -> Result<String, String> {
    let exports = PARTS
        .with(|parts| parts.borrow().get(id).and_then(|part| part.exports.clone()))
        .ok_or("The console part is restarting; try again")?;
    exchange(&exports, &request).map_err(|failure| {
        restart(id);
        format!("The console part failed: {}", error(failure))
    })
}

fn exchange(exports: &JsValue, request: &str) -> Result<String, JsValue> {
    // Pointers come back as signed 32-bit numbers.
    let pointer = |value: JsValue| {
        value
            .as_f64()
            .map(|number| number as i64 as u32)
            .ok_or_else(|| JsValue::from_str("The console part returned no pointer"))
    };
    let memory: WebAssembly::Memory = get(exports, "memory").dyn_into()?;
    let len = request.len() as u32;
    let at = pointer(call(exports, "services_ui_alloc", &[len.into()])?)?;
    Uint8Array::new_with_byte_offset_and_length(&memory.buffer(), at, len)
        .copy_from(request.as_bytes());
    let answer = pointer(call(exports, "services_ui_call", &[at.into(), len.into()])?)?;
    let view = Uint8Array::new(&memory.buffer());
    let mut header = [0; 4];
    view.subarray(answer, answer + 4).copy_to(&mut header);
    let mut text = vec![0; u32::from_le_bytes(header) as usize];
    view.subarray(answer + 4, answer + 4 + text.len() as u32)
        .copy_to(&mut text);
    call(exports, "services_ui_free", &[answer.into()])?;
    String::from_utf8(text).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Replaces a trapped instance with a fresh one of the same module.
fn restart(id: &str) {
    let Some((hash, module)) = PARTS.with(|parts| {
        let mut parts = parts.borrow_mut();
        let part = parts.get_mut(id)?;
        part.exports = None;
        Some((part.hash.clone(), part.module.clone()))
    }) else {
        return;
    };
    let id = id.to_owned();
    spawn_local(async move {
        match JsFuture::from(WebAssembly::instantiate_module(&module, &Object::new())).await {
            Ok(instance) => PARTS.with(|parts| {
                if let Some(part) = parts.borrow_mut().get_mut(&id)
                    && part.hash == hash
                {
                    part.exports = Some(get(&instance, "exports"));
                }
            }),
            Err(failure) => web_sys::console::error_1(&failure),
        }
    });
}

/// A console part that failed to load: every call reports why.
struct Unavailable(String);

impl ServiceUi for Unavailable {
    fn check(&self, _: &str) -> Result<(), String> {
        Err(self.0.clone())
    }
    fn request(&self, _: &Sections) -> Result<Value, String> {
        Err(self.0.clone())
    }
    fn import(&self, _: &str) -> Result<Imported, String> {
        Err(self.0.clone())
    }
    fn export(&self, _: &Value) -> Result<String, String> {
        Err(self.0.clone())
    }
}
