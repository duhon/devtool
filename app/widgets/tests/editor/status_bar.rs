//! The status bar's labels. UI-behaviour tests.

use services_widgets::status_bar::duration;

#[test]
fn durations_use_readable_labels() {
    assert_eq!(duration(1500.0), "1.5s");
    assert_eq!(duration(60000.0), "1min");
}
