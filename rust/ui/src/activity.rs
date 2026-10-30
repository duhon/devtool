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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Signal {
    seen: Option<Indicator>,
}

impl Signal {
    pub fn observe(&mut self, state: Indicator) {
        self.seen = (state != Indicator::Running).then_some(state);
    }

    pub fn indicator(self, state: Indicator, visible: bool) -> Indicator {
        if state != Indicator::Running && (visible || self.seen == Some(state)) {
            Indicator::Ready
        } else {
            state
        }
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
