use super::{call, error, get, js, window};
use crate::activity::{Indicator, Signal};
use js_sys::Promise;
use serde_json::json;
use services_contracts::updates::{Release, Source};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{AbortController, Request, RequestCache, RequestCredentials, RequestInit, Response};

#[derive(Clone, Copy, PartialEq)]
pub enum Phase {
    Checking,
    Current,
    Available,
    Downloading,
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
            Phase::Downloading | Phase::Installing | Phase::Reloading
        )
    }

    pub fn indicator(&self) -> Indicator {
        match self.phase {
            Phase::Available => Indicator::Success,
            Phase::Downloading | Phase::Installing | Phase::Reloading => Indicator::Running,
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
            Phase::Downloading => "Downloading the update...",
            Phase::Installing => "Installing the update...",
            Phase::Installed => "Update installed. Reload the extension to finish.",
            Phase::Reloading => "Reloading the extension...",
            Phase::Unconfigured => "Update source is not configured",
            Phase::Unsupported => {
                "Browser-authenticated updates are available in the Chrome extension"
            }
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

async fn chrome(owner: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, String> {
    let promise = call(owner, method, args)
        .map_err(error)?
        .dyn_into::<Promise>()
        .map_err(|_| format!("Chrome {method} did not return a promise"))?;
    JsFuture::from(promise).await.map_err(error)
}

async fn pause() -> Result<(), String> {
    let promise = Promise::new(&mut |resolve, reject| {
        if let Err(failure) =
            window().set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 250)
            && let Err(error) = reject.call1(&JsValue::UNDEFINED, &failure)
        {
            web_sys::console::error_1(&error);
        }
    });
    JsFuture::from(promise).await.map_err(error)?;
    Ok(())
}

pub async fn download(release: &Release) -> Result<String, String> {
    let downloads = get(&get(&js_sys::global(), "chrome"), "downloads");
    let filename = format!(
        "ServicesDevTools/updates/{}/{}/{}",
        release.tag,
        uuid::Uuid::now_v7(),
        release.archive
    );
    let id = chrome(
        &downloads,
        "download",
        &[js(&json!({"url":release.archive_url,"filename":filename,"saveAs":false,"conflictAction":"uniquify"}))],
    )
    .await?
    .as_f64()
    .filter(|value| value.is_finite() && value.fract() == 0.0 && *value > 0.0 && *value <= u32::MAX as f64)
    .ok_or("Chrome did not return an update download ID")? as u32;
    let deadline = js_sys::Date::now() + 10.0 * 60.0 * 1000.0;
    loop {
        let results = chrome(&downloads, "search", &[js(&json!({"id":id}))]).await?;
        let item = js_sys::Array::from(&results).get(0);
        match get(&item, "state").as_string().as_deref() {
            Some("complete") => {
                return get(&item, "filename")
                    .as_string()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "Chrome did not return the downloaded archive path".into());
            }
            Some("interrupted") => {
                return Err(format!(
                    "Update download was interrupted: {}",
                    get(&item, "error")
                        .as_string()
                        .unwrap_or_else(|| "unknown reason".into())
                ));
            }
            Some("in_progress") => {}
            _ => return Err("Chrome update download disappeared".into()),
        }
        if js_sys::Date::now() >= deadline {
            chrome(&downloads, "cancel", &[id.into()])
                .await
                .map_err(|failure| {
                    format!("Update download timed out; cancellation also failed: {failure}")
                })?;
            return Err("Update download timed out".into());
        }
        pause().await?;
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
