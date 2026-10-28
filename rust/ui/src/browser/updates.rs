use super::{call, error, get, window};
use crate::activity::{Indicator, Signal};
use services_contracts::updates::{Release, Source};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{AbortController, Request, RequestCache, RequestCredentials, RequestInit, Response};

#[derive(Clone, Copy, PartialEq)]
pub enum Phase {
    Checking,
    Current,
    Available,
    Preparing,
    Installing,
    Installed,
    Reloading,
    Unconfigured,
    Unsupported,
    Error,
}

pub struct State {
    pub phase: Phase,
    pub source: Option<Source>,
    pub release: Option<Release>,
    pub error: Option<String>,
    pub generation: u64,
    pub signal: Signal,
}

impl State {
    pub fn new(native: bool) -> Self {
        Self {
            phase: if native {
                Phase::Checking
            } else {
                Phase::Unsupported
            },
            source: None,
            release: None,
            error: None,
            generation: 0,
            signal: Default::default(),
        }
    }

    pub fn busy(&self) -> bool {
        matches!(
            self.phase,
            Phase::Preparing | Phase::Installing | Phase::Reloading
        )
    }

    pub fn indicator(&self) -> Indicator {
        match self.phase {
            Phase::Available => Indicator::Success,
            Phase::Preparing | Phase::Installing | Phase::Reloading => Indicator::Running,
            Phase::Error => Indicator::Error,
            _ => Indicator::Ready,
        }
    }

    pub fn fail(&mut self, message: String) {
        self.phase = Phase::Error;
        self.error = Some(message);
        self.signal = Default::default();
    }

    pub fn text(&self) -> &str {
        match self.phase {
            Phase::Checking => "Checking for updates...",
            Phase::Current => "Up to date",
            Phase::Available => "A new version is available",
            Phase::Preparing => "Saving the workspace before updating...",
            Phase::Installing => {
                "Downloading and installing the update through the native runtime..."
            }
            Phase::Installed => "Update installed. Reload the extension to finish.",
            Phase::Reloading => "Reloading the extension...",
            Phase::Unconfigured => "Update source is not configured",
            Phase::Unsupported => "Application updates are available in the Chrome extension",
            Phase::Error => self.error.as_deref().unwrap_or("Update failed"),
        }
    }
}

pub async fn check(source: &Source) -> Result<Release, String> {
    let url = source.manifest_url()?;
    let controller = AbortController::new().map_err(error)?;
    let options = RequestInit::new();
    options.set_credentials(RequestCredentials::Include);
    options.set_cache(RequestCache::NoStore);
    options.set_signal(Some(&controller.signal()));
    let request = Request::new_with_str_and_init(&url, &options).map_err(error)?;
    let abort = controller.clone();
    let timeout = Closure::once(move || abort.abort());
    let timer = window()
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            timeout.as_ref().unchecked_ref(),
            15_000,
        )
        .map_err(error)?;
    let result = JsFuture::from(window().fetch_with_request(&request)).await;
    window().clear_timeout_with_handle(timer);
    drop(timeout);
    if controller.signal().aborted() {
        return Err("Update check timed out. Check your connection and GitHub sign-in.".into());
    }
    let response: Response = result
        .map_err(|value| format!("Could not check updates: {}", error(value)))?
        .dyn_into()
        .map_err(error)?;
    if !response.ok() {
        return Err(format!(
            "GitHub update metadata is unavailable (HTTP {}). Sign in to GitHub and check repository access; the first release may not be published yet.",
            response.status()
        ));
    }
    let content_type = response
        .headers()
        .get("content-type")
        .map_err(error)?
        .unwrap_or_default();
    if content_type.contains("text/html") || response.url().starts_with("https://github.com/login")
    {
        return Err("Sign in to GitHub in this Chrome profile and check repository access.".into());
    }
    let text = JsFuture::from(response.text().map_err(error)?)
        .await
        .map_err(error)?
        .as_string()
        .ok_or("GitHub update metadata is not text")?;
    if text.len() > 64 * 1024 {
        return Err("GitHub update metadata exceeds the size limit".into());
    }
    let release: Release = serde_json::from_str(&text)
        .map_err(|failure| format!("Invalid GitHub update metadata: {failure}"))?;
    release.validate(source)?;
    Ok(release)
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
