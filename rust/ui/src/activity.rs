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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewed_outcomes_go_dark_without_suppressing_running_or_new_outcomes() {
        let mut signal = Signal::default();
        assert_eq!(signal.indicator(Indicator::Error, false), Indicator::Error);
        assert_eq!(signal.indicator(Indicator::Error, true), Indicator::Ready);
        signal.observe(Indicator::Error);
        assert_eq!(signal.indicator(Indicator::Error, false), Indicator::Ready);
        assert_eq!(
            signal.indicator(Indicator::Success, false),
            Indicator::Success
        );
        assert_eq!(
            signal.indicator(Indicator::Running, true),
            Indicator::Running
        );
        signal.observe(Indicator::Running);
        assert_eq!(
            signal.indicator(Indicator::Running, false),
            Indicator::Running
        );
        assert_eq!(signal.indicator(Indicator::Error, false), Indicator::Error);
        assert_eq!(
            Signal::default().indicator(Indicator::Error, false),
            Indicator::Error
        );
    }

    #[test]
    fn running_takes_priority_without_hiding_counts() {
        let (state, title) = summary([
            Indicator::Success,
            Indicator::Error,
            Indicator::Running,
            Indicator::Warning,
        ]);
        assert_eq!(state, Indicator::Running);
        assert_eq!(
            title,
            "1 running, 1 errors, 1 warnings/stopped/waiting, 1 successful, 0 ready"
        );
        assert_eq!(
            summary([Indicator::Success, Indicator::Warning, Indicator::Error]).0,
            Indicator::Error
        );
        assert_eq!(summary([]).0, Indicator::Ready);
    }

    #[test]
    fn cancellation_is_not_success_and_errors_are_not_warnings() {
        assert_eq!(Indicator::status("", false), Indicator::Ready);
        assert_eq!(Indicator::status("Request · OK", false), Indicator::Success);
        assert_eq!(Indicator::status("Stopped", false), Indicator::Warning);
        assert_eq!(
            Indicator::status("WARNING · schema", false),
            Indicator::Warning
        );
        assert_eq!(Indicator::status("ERROR · query", false), Indicator::Error);
        assert_eq!(Indicator::status("Stopping...", true), Indicator::Running);
    }
}
