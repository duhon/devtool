//! The wasm modules the Kernel runs (the Host's `wasm` list): the panel fetches each one
//! from the Host once per digest, keeps it in IndexedDB beside the console parts,
//! instantiates it and starts it in the Kernel.

use super::error;
use super::module_ui::{open, read, write};
use super::transport::{Reply, Transport};
use base64::Engine;
use js_sys::Promise;
use serde_json::{Value, json};
use services_kernel::Effect;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::IdbDatabase;
use yew::Callback;

/// Starts the declared modules the Kernel lacks (or runs at another digest) for the drivers
/// the Host offers, then hands over their first effects and the ids that started. A module
/// that fails to start reports why on use.
pub fn prepare(transport: &Rc<Transport>, done: Callback<(Vec<Effect>, Vec<String>)>) {
    let transport = transport.clone();
    spawn_local(async move {
        let health = json!({"type": "health", "capabilities": {"chunkedResponses": true}});
        match ask(&transport, health).await {
            Ok(value) => {
                let drivers: BTreeMap<String, String> = value["drivers"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(name, version)| {
                        (name.clone(), version.as_str().unwrap_or("").to_owned())
                    })
                    .collect();
                crate::kernel::configure(drivers);
            }
            Err(failure) => {
                web_sys::console::warn_1(&format!("The Host drivers are unknown: {failure}").into())
            }
        }
        let (mut effects, wanted) = crate::kernel::retain();
        let mut started = Vec::new();
        if !wanted.is_empty() {
            let cache = match open().await {
                Ok(database) => Some(database),
                Err(failure) => {
                    web_sys::console::warn_1(&format!("Modules are not cached: {failure}").into());
                    None
                }
            };
            for (manifest, digest) in wanted {
                let id = manifest.id.clone();
                let name = manifest.name.clone();
                match load(cache.as_ref(), &transport, manifest, &digest).await {
                    Ok(first) => {
                        effects.extend(first);
                        started.push(id);
                    }
                    Err(failure) => {
                        let failure = format!("The {name} module failed to load: {failure}");
                        web_sys::console::error_1(&failure.clone().into());
                        crate::kernel::fail(&id, failure);
                    }
                }
            }
            if let Some(cache) = cache {
                cache.close();
            }
        }
        done.emit((effects, started));
    });
}

async fn load(
    cache: Option<&IdbDatabase>,
    transport: &Rc<Transport>,
    manifest: services_sdk::Manifest,
    digest: &str,
) -> Result<Vec<Effect>, String> {
    let key = format!("wasm:{}", manifest.id);
    let cached = match cache {
        Some(database) => read(database, &key, digest)
            .await
            .unwrap_or_else(|failure| {
                web_sys::console::warn_1(&failure.into());
                None
            }),
        None => None,
    };
    let (digest, bytes) = match cached {
        Some(bytes) => (digest.to_owned(), bytes),
        None => {
            let value = ask(
                transport,
                json!({"service": "modules", "op": "wasm", "module": manifest.id}),
            )
            .await?;
            let text = |key: &str| value[key].as_str().ok_or("the Host sent no module");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(text("wasm")?)
                .map_err(|e| e.to_string())?;
            let digest = text("digest")?.to_owned();
            if let Some(database) = cache
                && let Err(failure) = write(database, &key, &digest, &bytes).await
            {
                web_sys::console::warn_1(&failure.into());
            }
            (digest, bytes)
        }
    };
    let instance = services_kernel::wasm::instantiate(&bytes).await?;
    crate::kernel::install(manifest, &digest, Box::new(instance))
}

/// One Host request and its result.
async fn ask(transport: &Rc<Transport>, message: Value) -> Result<Value, String> {
    let slot = Rc::new(RefCell::new(None));
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
        Some(Reply::Result(value)) => Ok(value),
        Some(Reply::Error(value) | Reply::Exit(value)) => Err(value["error"]
            .as_str()
            .unwrap_or("the Host refused the request")
            .to_owned()),
        _ => Err("the Host sent no answer".into()),
    }
}
