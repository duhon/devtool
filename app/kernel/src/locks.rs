//! Web Locks modules ask for (`lock`), shared by every window of the App: the next holder
//! gets one once the previous lets it go, e.g. Splunk's pace of searches.

use crate::Callback;
use crate::js::{call, error, get, window};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use wasm_bindgen::{JsValue, closure::Closure};

/// A Web Lock: `release` lets it go once granted; dropping it before then gives it back as
/// soon as it is granted.
struct Lock {
    release: Rc<RefCell<Option<js_sys::Function>>>,
    dropped: Rc<Cell<bool>>,
}

impl Lock {
    fn release(&self) {
        self.dropped.set(true);
        if let Some(release) = self.release.borrow_mut().take() {
            let _ = release.call0(&JsValue::NULL);
        }
    }
}

/// The locks the modules asked for, by (module, name).
#[derive(Default)]
pub struct Locks(BTreeMap<(String, String), Lock>);

impl Locks {
    /// Asks the browser for the lock `name` of `module`; `granted` hears when it holds it.
    pub fn acquire(
        &mut self,
        module: &str,
        name: &str,
        granted: Callback<()>,
    ) -> Result<(), String> {
        let locks = get(window().navigator().as_ref(), "locks");
        if locks.is_undefined() {
            return Err("Locks are not available in this browser".into());
        }
        let release: Rc<RefCell<Option<js_sys::Function>>> = Rc::default();
        let dropped: Rc<Cell<bool>> = Rc::default();
        let (held, gone) = (release.clone(), dropped.clone());
        let callback = Closure::once_into_js(move |_: JsValue| -> js_sys::Promise {
            js_sys::Promise::new(&mut |resolve, _| {
                if gone.get() {
                    let _ = resolve.call0(&JsValue::NULL);
                } else {
                    *held.borrow_mut() = Some(resolve);
                    granted.emit(());
                }
            })
        });
        call(
            &locks,
            "request",
            &[
                JsValue::from_str(&format!("module:{module}:{name}")),
                callback,
            ],
        )
        .map_err(error)?;
        if let Some(previous) = self.0.insert(
            (module.to_owned(), name.to_owned()),
            Lock { release, dropped },
        ) {
            previous.release();
        }
        Ok(())
    }

    /// Lets the lock `name` of `module` go.
    pub fn release(&mut self, module: String, name: String) {
        if let Some(lock) = self.0.remove(&(module, name)) {
            lock.release();
        }
    }
}
