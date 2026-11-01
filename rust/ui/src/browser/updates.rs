use super::{call, error, get, views::OperationStatus, window};
use crate::activity::{Indicator, Signal};
use serde_json::{Value, json};
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
    pub signal: Signal,
    pub status: OperationStatus,
}

impl Default for State {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            target: None,
            error: None,
            signal: Default::default(),
            status: OperationStatus {
                debug: json!({"operation":"Updates"}),
                ..Default::default()
            },
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

    pub fn indicator(&self) -> Indicator {
        if self.error.is_some() {
            return Indicator::Error;
        }
        match self.phase {
            Phase::Preparing | Phase::Installing | Phase::Reloading => Indicator::Running,
            Phase::Error => Indicator::Error,
            _ => Indicator::Ready,
        }
    }

    pub fn fail(&mut self, message: String) {
        self.phase = if matches!(self.phase, Phase::Installed | Phase::Reloading) {
            Phase::Installed
        } else {
            Phase::Error
        };
        self.error = Some(message);
        self.signal = Default::default();
        self.record(Value::Null);
    }

    pub fn begin(&mut self, phase: Phase, operation: &str, details: Value) {
        self.status = OperationStatus::event(operation, js_sys::Date::now(), None);
        self.phase = phase;
        self.error = None;
        self.record(details);
    }

    pub fn transition(&mut self, phase: Phase) {
        self.phase = phase;
        self.error = None;
        self.signal = Default::default();
        self.record(Value::Null);
    }

    pub fn record(&mut self, details: Value) {
        let now = js_sys::Date::now();
        self.status.text = self.text().to_owned();
        self.status.running = self.busy();
        let mut events = self.status.debug["events"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        events.push(json!({
            "at":now,"phase":self.phase,"status":self.status.text,"details":details
        }));
        let (source, version) = self.target.clone().unzip();
        self.status.debug = json!({
            "operation":self.status.debug["operation"],
            "phase":self.phase,"status":self.status.text,
            "started":self.status.started,"elapsedMs":(now-self.status.started).max(0.0),
            "currentVersion":env!("CARGO_PKG_VERSION"),
            "source":source,"version":version,"error":self.error,"events":events
        });
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

pub fn reload() -> Result<(), String> {
    ensure_single_window()?;
    let runtime = get(&get(&js_sys::global(), "chrome"), "runtime");
    call(&runtime, "reload", &[]).map_err(error)?;
    Ok(())
}

pub fn ensure_single_window() -> Result<(), String> {
    let extension = get(&get(&js_sys::global(), "chrome"), "extension");
    let views = call(&extension, "getViews", &[]).map_err(error)?;
    for view in js_sys::Array::from(&views).iter() {
        if view == JsValue::from(window()) {
            continue;
        }
        let document = get(&view, "document");
        if !document.is_null()
            && !document.is_undefined()
            && !call(
                &document,
                "querySelector",
                &[JsValue::from_str(".console-root")],
            )
            .map_err(error)?
            .is_null()
        {
            return Err("Close other Services DevTools windows before updating or reloading, to preserve their requests and unsaved work.".into());
        }
    }
    Ok(())
}
