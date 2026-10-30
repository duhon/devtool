//! Activity indicator: how running, new and viewed outcomes are summarized
//! on tabs. UI-behaviour tests.

use services_ui::activity::{Indicator, Signal, summary};

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
