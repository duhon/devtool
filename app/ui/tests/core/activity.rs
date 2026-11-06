//! Activity indicator: how running, new and viewed outcomes are summarized
//! on tabs. UI-behaviour tests.

use services_ui::activity::{Indicator, SEEN_AFTER_MS, Signal, sooner, summary};

#[test]
fn viewed_outcomes_go_dark_without_suppressing_running_or_new_outcomes() {
    let mut signal = Signal::default();
    assert_eq!(signal.indicator(Indicator::Error, false), Indicator::Error);
    assert_eq!(signal.indicator(Indicator::Error, true), Indicator::Ready);
    assert_eq!(signal.observe(Indicator::Error, 0.0), Some(SEEN_AFTER_MS));
    assert_eq!(signal.observe(Indicator::Error, SEEN_AFTER_MS), None);
    assert_eq!(signal.indicator(Indicator::Error, false), Indicator::Ready);
    assert_eq!(
        signal.indicator(Indicator::Success, false),
        Indicator::Success
    );
    assert_eq!(
        signal.indicator(Indicator::Running, true),
        Indicator::Running
    );
    assert_eq!(signal.observe(Indicator::Running, 0.0), None);
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
fn an_outcome_counts_as_seen_only_after_staying_on_screen() {
    let mut signal = Signal::default();
    assert_eq!(
        signal.observe(Indicator::Error, 1000.0),
        Some(SEEN_AFTER_MS)
    );
    assert_eq!(
        signal.observe(Indicator::Error, 1100.0),
        Some(SEEN_AFTER_MS - 100.0)
    );
    signal.hide();
    assert_eq!(
        signal.indicator(Indicator::Error, false),
        Indicator::Error,
        "An error that flashed by while the user switched away must stay lit"
    );
    assert_eq!(
        signal.observe(Indicator::Error, 1600.0),
        Some(SEEN_AFTER_MS),
        "Showing it again starts a new count"
    );
    assert_eq!(
        signal.observe(Indicator::Warning, 1700.0),
        Some(SEEN_AFTER_MS)
    );
    assert_eq!(signal.observe(Indicator::Warning, 2200.0), None);
    assert_eq!(
        signal.indicator(Indicator::Warning, false),
        Indicator::Ready
    );
    assert_eq!(signal.observe(Indicator::Ready, 2300.0), None);
    assert_eq!(sooner(Some(300.0), Some(200.0)), Some(200.0));
    assert_eq!(sooner(None, Some(200.0)), Some(200.0));
    assert_eq!(sooner(None, None), None);
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
