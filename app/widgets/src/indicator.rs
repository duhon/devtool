//! The state light: running, new and viewed outcomes, as tabs, buttons and checks show them.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum Indicator {
    #[default]
    Ready,
    Success,
    Warning,
    Error,
    Running,
}

impl Indicator {
    pub fn status(text: &str, running: bool) -> Self {
        if running {
            Self::Running
        } else if text.contains("ERROR") {
            Self::Error
        } else if text.contains("WARNING") || text.contains("Stopped") {
            Self::Warning
        } else if text.is_empty() {
            Self::Ready
        } else {
            Self::Success
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Running => "running",
        }
    }
}

/// How long, in milliseconds, a finished status must stay on screen to count as seen, so an
/// outcome that flashes by while the user switches away still lights its tab.
pub const SEEN_AFTER_MS: f64 = 500.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Signal {
    seen: Option<Indicator>,
    /// The finished state on screen and when it appeared there.
    shown: Option<(Indicator, f64)>,
}

impl Signal {
    /// Notes that `state` is on screen at `now` (milliseconds). Returns how much longer it must
    /// stay there to count as seen; the caller observes again after that delay.
    pub fn observe(&mut self, state: Indicator, now: f64) -> Option<f64> {
        if state == Indicator::Running {
            *self = Self::default();
            return None;
        }
        if self.seen == Some(state) {
            return None;
        }
        let since = match self.shown {
            Some((shown, since)) if shown == state => since,
            _ => now,
        };
        let wait = since + SEEN_AFTER_MS - now;
        if state == Indicator::Ready || wait <= 0.0 {
            self.seen = Some(state);
            self.shown = None;
            None
        } else {
            self.shown = Some((state, since));
            Some(wait)
        }
    }

    /// Notes that the status is off screen, so showing it again starts a new count.
    pub fn hide(&mut self) {
        self.shown = None;
    }

    pub fn indicator(self, state: Indicator, visible: bool) -> Indicator {
        if state != Indicator::Running && (visible || self.seen == Some(state)) {
            Indicator::Ready
        } else {
            state
        }
    }
}

/// The earlier of two pending [`Signal::observe`] delays.
pub fn sooner(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

pub fn summary(states: impl IntoIterator<Item = Indicator>) -> (Indicator, String) {
    let mut highest = Indicator::Ready;
    let mut counts = [0; 5];
    for state in states {
        highest = highest.max(state);
        counts[state as usize] += 1;
    }
    (
        highest,
        format!(
            "{} running, {} errors, {} warnings/stopped/waiting, {} successful, {} ready",
            counts[4], counts[3], counts[2], counts[1], counts[0]
        ),
    )
}

#[cfg(target_arch = "wasm32")]
pub use view::{CSS, SeenTimer, indicator};

#[cfg(target_arch = "wasm32")]
mod view {
    use super::Indicator;
    use std::{cell::Cell, rc::Rc};
    use wasm_bindgen::{JsCast, closure::Closure};
    use yew::prelude::*;

    pub const CSS: &str = include_str!("indicator.css");

    /// Re-renders a panel once a status on screen has stayed there long enough to count as seen.
    #[derive(Default)]
    pub struct SeenTimer(Rc<Cell<Option<i32>>>);

    impl SeenTimer {
        /// Calls `rerender` after `wait` milliseconds unless an earlier call is already pending;
        /// that one re-renders first and the panel schedules again.
        pub fn schedule(&self, wait: Option<f64>, rerender: Callback<()>) {
            let Some(wait) = wait else {
                return;
            };
            if self.0.get().is_some() {
                return;
            }
            let pending = self.0.clone();
            let fire = Closure::once_into_js(move || {
                pending.set(None);
                rerender.emit(());
            });
            match crate::dom::window().set_timeout_with_callback_and_timeout_and_arguments_0(
                fire.unchecked_ref(),
                wait.ceil() as i32,
            ) {
                Ok(id) => self.0.set(Some(id)),
                Err(error) => web_sys::console::error_1(&error),
            }
        }
    }

    impl Drop for SeenTimer {
        fn drop(&mut self) {
            if let Some(id) = self.0.take() {
                crate::dom::window().clear_timeout_with_handle(id);
            }
        }
    }

    pub fn indicator(state: Indicator, title: String) -> Html {
        html! {
            <span class={classes!("console-state-light",format!("is-{}",state.name()))}
                data-state={state.name()} role="img" aria-label={title.clone()} {title}/>
        }
    }
}
