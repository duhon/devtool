//! The wasm modules the Kernel runs (the Host's `wasm` list): the panel fetches each one
//! from the Host once per digest, keeps it in IndexedDB, instantiates it and starts it in the
//! Kernel. The last module list is cached so a new window opens on the same tabs.

use super::registry;
use crate::Callback;
use crate::Effect;
use crate::host::{Reply, Transport};
use crate::js::{error, get, set, window};
use crate::runtime::Started;
use crate::storage::database::request;
use base64::Engine;
use js_sys::{Object, Promise, Uint8Array};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{IdbDatabase, IdbTransactionMode};

const MODULES: &str = "servicesConsole.modules";

/// The module list of the last host reply; the built-in one stays without a valid cache.
pub fn restore() -> bool {
    let cached: Option<Value> = window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(MODULES).ok().flatten())
        .and_then(|raw| serde_json::from_str(&raw).ok());
    let Some(cached) = cached else {
        return false;
    };
    registry::parse(&cached).is_ok_and(|modules| {
        registry::declare(modules);
        true
    })
}

/// Installs and caches the host's module list and the modules the Kernel runs; `true` when
/// either changed.
pub fn accept(value: &Value) -> Result<bool, String> {
    let modules = registry::parse(value)?;
    if let Some(storage) = window().local_storage().ok().flatten() {
        let cached = json!({"wasm": value["wasm"]}).to_string();
        let _ = storage.set_item(MODULES, &cached);
    }
    Ok(registry::declare(modules))
}

/// Starts the declared modules the Kernel lacks (or runs at another digest) for the drivers
/// the Host offers, all at once, `first` asked for first. `loaded` hears of each module as
/// it starts, with its first effects, then once more with `done`. A module that fails to
/// start reports why on use.
pub fn prepare(transport: &Rc<Transport>, first: &str, loaded: Callback<Started>) {
    let transport = transport.clone();
    let first = first.to_owned();
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
                registry::configure(drivers);
            }
            Err(failure) => {
                web_sys::console::warn_1(&format!("The Host drivers are unknown: {failure}").into())
            }
        }
        let (effects, mut wanted) = registry::retain();
        if !effects.is_empty() {
            loaded.emit(Started {
                effects,
                ..Started::default()
            });
        }
        if wanted.is_empty() {
            loaded.emit(Started {
                done: true,
                ..Started::default()
            });
            return;
        }
        wanted.sort_by_key(|(manifest, _)| manifest.id != first);
        let cache = match open().await {
            Ok(database) => Some(Rc::new(database)),
            Err(failure) => {
                web_sys::console::warn_1(&format!("Modules are not cached: {failure}").into());
                None
            }
        };
        let left = Rc::new(std::cell::Cell::new(wanted.len()));
        for (manifest, digest) in wanted {
            let (transport, cache, loaded, left) = (
                transport.clone(),
                cache.clone(),
                loaded.clone(),
                left.clone(),
            );
            spawn_local(async move {
                let id = manifest.id.clone();
                let name = manifest.name.clone();
                match load(cache.as_deref(), &transport, manifest, &digest).await {
                    Ok(effects) => loaded.emit(Started {
                        effects,
                        started: vec![id],
                        done: false,
                    }),
                    Err(failure) => {
                        let failure = format!("The {name} module failed to load: {failure}");
                        web_sys::console::error_1(&failure.clone().into());
                        registry::fail(&id, failure);
                    }
                }
                left.set(left.get() - 1);
                if left.get() == 0 {
                    if let Some(cache) = cache {
                        cache.close();
                    }
                    loaded.emit(Started {
                        done: true,
                        ..Started::default()
                    });
                }
            });
        }
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
    let instance = crate::wasm::instantiate(&bytes).await?;
    registry::install(manifest, &digest, Box::new(instance))
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

const DATABASE: &str = "servicesConsoleModules";
const STORE: &str = "ui";

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
