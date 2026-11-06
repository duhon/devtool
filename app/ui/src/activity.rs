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
