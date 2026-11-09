//! Installing a new version of the App from a source, then reloading the extension; only
//! one window may be open meanwhile.

#[cfg(target_arch = "wasm32")]
use crate::js::{call, error, get, window};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;

#[derive(Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Preparing,
    Installing,
    Installed,
    Reloading,
    Error,
}

/// Installing a new version of the application from a source, then reloading the extension.
pub struct State {
    pub phase: Phase,
    /// The source slug and version being installed.
    pub target: Option<(String, String)>,
    pub error: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            target: None,
            error: None,
        }
    }
}

impl State {
    pub fn busy(&self) -> bool {
        matches!(
            self.phase,
            Phase::Preparing | Phase::Installing | Phase::Reloading
        )
    }

    pub fn fail(&mut self, message: String) {
        self.phase = if matches!(self.phase, Phase::Installed | Phase::Reloading) {
            Phase::Installed
        } else {
            Phase::Error
        };
        self.error = Some(message);
    }

    pub fn transition(&mut self, phase: Phase) {
        self.phase = phase;
        self.error = None;
    }

    pub fn text(&self) -> &str {
        if let Some(error) = &self.error {
            return error;
        }
        match self.phase {
            Phase::Idle => "Ready",
            Phase::Preparing => "Saving the workspace before updating...",
            Phase::Installing => {
                "Downloading and installing the update through the native runtime..."
            }
            Phase::Installed => "Update installed. Reload the extension to finish.",
            Phase::Reloading => "Reloading the extension...",
            Phase::Error => self.error.as_deref().unwrap_or("Update failed"),
        }
    }
}

/// Reloads the extension unless another of its windows is open; `root` is what marks one
/// (a selector of the App's root element).
#[cfg(target_arch = "wasm32")]
pub fn reload(root: &str) -> Result<(), String> {
    ensure_single_window(root)?;
    let runtime = get(&get(&js_sys::global(), "chrome"), "runtime");
    call(&runtime, "reload", &[]).map_err(error)?;
    Ok(())
}

/// Refuses while another window of the extension shows the App (an element matching `root`).
#[cfg(target_arch = "wasm32")]
pub fn ensure_single_window(root: &str) -> Result<(), String> {
    let extension = get(&get(&js_sys::global(), "chrome"), "extension");
    let views = call(&extension, "getViews", &[]).map_err(error)?;
    for view in js_sys::Array::from(&views).iter() {
        if view == JsValue::from(window()) {
            continue;
        }
        let document = get(&view, "document");
        if !document.is_null()
            && !document.is_undefined()
            && !call(&document, "querySelector", &[JsValue::from_str(root)])
                .map_err(error)?
                .is_null()
        {
            return Err("Close other Services DevTools windows before updating or reloading, to preserve their requests and unsaved work.".into());
        }
    }
    Ok(())
}
