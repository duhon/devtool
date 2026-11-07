//! History panel: a saved run shows its source, the time and outcome, then the body its
//! module rendered, intact. UI-behaviour tests.

use services_ui::format::{MAX_RESPONSE_CHARS, history_document};

#[test]
fn a_saved_run_shows_its_rendered_body_under_a_timed_outcome() {
    let source = "#check saved\n#endpoint\nhttps://fixture.invalid\n#search\nindex=fixture";
    let body = "| n |\n| 1 |";
    let (text, regions) =
        history_document(&format!("{source}\n\n#responce OK\n{body}"), "1:00:00 PM");
    assert_eq!(text, format!("{source}\n#responce 1:00:00 PM · OK\n{body}"));
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].request, 0);
    assert!(
        text.lines()
            .nth(regions[0].header)
            .unwrap()
            .starts_with("#responce")
    );
    assert_eq!(regions[0].end, text.split('\n').count());
}

#[test]
fn history_display_handles_partial_plain_and_large_responses() {
    let prefix = "#check example";
    let partial = format!("{prefix}\n\n#responce OK\n{{\"debug\":");
    assert_eq!(
        history_document(&partial, "1:00:00 PM").0,
        format!("{prefix}\n#responce 1:00:00 PM · OK\n{{\"debug\":")
    );
    // Only the first response marker belongs to the run.
    let plain = "line one\n\n#response error\nThis is part of the actual response.";
    let snapshot = format!("{prefix}\n\n#responce OK\n{plain}");
    assert!(history_document(&snapshot, "1:00:00 PM").0.ends_with(plain));
    let large = "é".repeat(MAX_RESPONSE_CHARS + 4);
    let snapshot = format!("{prefix}\n\n#responce OK\n{large}");
    // A large saved body shows whole, unlike the console's response.
    assert!(
        history_document(&snapshot, "1:00:00 PM")
            .0
            .ends_with(&large)
    );
    for (header, outcome) in [
        ("#responce WARNING", "WARNING"),
        ("#responce ERROR", "ERROR"),
        ("#responce NOTICE", "NOTICE"),
        ("#responce Stopped", "Stopped"),
        ("#response error", "ERROR"),
    ] {
        let snapshot = format!("{prefix}\n\n{header}\nbody");
        let display = history_document(&snapshot, "1:00:00 PM").0;
        assert_eq!(
            display,
            format!("{prefix}\n#responce 1:00:00 PM · {outcome}\nbody")
        );
    }
    let unanswered = format!("{prefix}\n#endpoint\nx");
    assert_eq!(
        history_document(&unanswered, "1:00:00 PM"),
        (unanswered.clone(), vec![])
    );
}
