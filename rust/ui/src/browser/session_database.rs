use super::{error, window};
use crate::sessions::Library;
use js_sys::{Function, Promise};
use std::rc::Rc;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{IdbDatabase, IdbRequest, IdbTransaction, IdbTransactionMode};

const DATABASE: &str = "servicesConsole";
const STORE: &str = "sessions";
const KEY: &str = "library";

fn settle(callback: &Function, value: &JsValue) {
    if let Err(error) = callback.call1(&JsValue::UNDEFINED, value) {
        web_sys::console::error_1(&error);
    }
}

pub(super) async fn request(request: IdbRequest) -> Result<JsValue, String> {
    let mut success = None;
    let mut failure = None;
    let promise = Promise::new(&mut |resolve, reject| {
        let pending = request.clone();
        let failed_result = reject.clone();
        success = Some(Closure::<dyn FnMut()>::new(move || {
            match pending.result() {
                Ok(value) => settle(&resolve, &value),
                Err(error) => settle(&failed_result, &error),
            }
        }));
        let pending = request.clone();
        failure = Some(Closure::<dyn FnMut()>::new(move || {
            let detail = match pending.error() {
                Ok(Some(error)) => error.into(),
                Ok(None) => "IndexedDB request failed".into(),
                Err(error) => error,
            };
            settle(&reject, &detail);
        }));
        request.set_onsuccess(success.as_ref().map(|f| f.as_ref().unchecked_ref()));
        request.set_onerror(failure.as_ref().map(|f| f.as_ref().unchecked_ref()));
    });
    let result = JsFuture::from(promise).await.map_err(error);
    request.set_onsuccess(None);
    request.set_onerror(None);
    result
}

struct Transaction {
    raw: IdbTransaction,
    completion: Promise,
    complete: Option<Closure<dyn FnMut()>>,
    abort: Option<Closure<dyn FnMut()>>,
    finished: bool,
}
impl Transaction {
    fn new(raw: IdbTransaction) -> Self {
        let mut complete = None;
        let mut abort = None;
        let completion = Promise::new(&mut |resolve, reject| {
            complete = Some(Closure::<dyn FnMut()>::new(move || {
                settle(&resolve, &JsValue::UNDEFINED);
            }));
            let transaction = raw.clone();
            abort = Some(Closure::<dyn FnMut()>::new(move || {
                let detail = transaction
                    .error()
                    .map(JsValue::from)
                    .unwrap_or_else(|| "Session storage transaction was aborted".into());
                settle(&reject, &detail);
            }));
            raw.set_oncomplete(complete.as_ref().map(|f| f.as_ref().unchecked_ref()));
            raw.set_onabort(abort.as_ref().map(|f| f.as_ref().unchecked_ref()));
        });
        Self {
            raw,
            completion,
            complete,
            abort,
            finished: false,
        }
    }
    async fn finish(mut self) -> Result<(), String> {
        let result = JsFuture::from(self.completion.clone()).await.map_err(error);
        self.finished = true;
        result.map(|_| ())
    }
}
impl Drop for Transaction {
    fn drop(&mut self) {
        if !self.finished {
            // A rejected read/merge must never leave a partially written transaction.
            if let Err(error) = self.raw.abort() {
                web_sys::console::warn_1(&error);
            }
            // Observe rejection when an early error bypasses finish().
            let completion = JsFuture::from(self.completion.clone());
            let handlers = (self.complete.take(), self.abort.take());
            let raw = self.raw.clone();
            spawn_local(async move {
                if let Err(error) = completion.await {
                    web_sys::console::warn_1(&error);
                }
                raw.set_oncomplete(None);
                raw.set_onabort(None);
                drop(handlers);
            });
        } else {
            self.raw.set_oncomplete(None);
            self.raw.set_onabort(None);
        }
    }
}

struct Connection {
    raw: IdbDatabase,
    _version_change: Closure<dyn FnMut()>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.raw.set_onversionchange(None);
        self.raw.close();
    }
}
#[derive(Clone)]
pub struct Database(Rc<Connection>);
impl Database {
    pub async fn open() -> Result<Self, String> {
        let factory = window()
            .indexed_db()
            .map_err(error)?
            .ok_or("IndexedDB is unavailable; saved sessions were not changed")?;
        let opening = factory.open_with_u32(DATABASE, 1).map_err(error)?;
        let upgrading = opening.clone();
        let upgrade = Closure::<dyn FnMut()>::new(move || {
            let result = upgrading
                .result()
                .and_then(|value| value.dyn_into::<IdbDatabase>())
                .and_then(|database| database.create_object_store(STORE));
            if let Err(error) = result {
                web_sys::console::error_1(&error);
                if let Some(transaction) = upgrading.transaction()
                    && let Err(error) = transaction.abort()
                {
                    web_sys::console::error_1(&error);
                }
            }
        });
        opening.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
        let result = request(opening.clone().into()).await;
        opening.set_onupgradeneeded(None);
        let raw = result?.dyn_into::<IdbDatabase>().map_err(error)?;
        let closing = raw.clone();
        let version_change = Closure::<dyn FnMut()>::new(move || closing.close());
        raw.set_onversionchange(Some(version_change.as_ref().unchecked_ref()));
        Ok(Self(Rc::new(Connection {
            raw,
            _version_change: version_change,
        })))
    }
    pub async fn load(&self) -> Result<Option<Library>, String> {
        let transaction = Transaction::new(self.0.raw.transaction_with_str(STORE).map_err(error)?);
        let store = transaction.raw.object_store(STORE).map_err(error)?;
        let value = request(store.get(&KEY.into()).map_err(error)?).await?;
        let library = decode(value)?;
        transaction.finish().await?;
        Ok(library)
    }
    pub async fn change(
        &self,
        change: impl FnOnce(Option<Library>) -> Result<Library, String>,
    ) -> Result<Library, String> {
        let transaction = Transaction::new(
            self.0
                .raw
                .transaction_with_str_and_mode(STORE, IdbTransactionMode::Readwrite)
                .map_err(error)?,
        );
        let store = transaction.raw.object_store(STORE).map_err(error)?;
        let value = request(store.get(&KEY.into()).map_err(error)?).await?;
        let previous = decode(value)?;
        let revision = previous
            .as_ref()
            .map(|library| library.revision)
            .unwrap_or(0);
        let mut next = change(previous)?;
        next.revision = revision
            .checked_add(1)
            .ok_or("Session revision exhausted")?;
        let raw = serde_json::to_string(&next).map_err(|error| error.to_string())?;
        store
            .put_with_key(&raw.into(), &KEY.into())
            .map_err(error)?;
        transaction.finish().await?;
        Ok(next)
    }
}

fn decode(value: JsValue) -> Result<Option<Library>, String> {
    if value.is_undefined() {
        Ok(None)
    } else {
        let raw = value
            .as_string()
            .ok_or("Invalid IndexedDB session record")?;
        Library::restore(&raw).map(Some)
    }
}
