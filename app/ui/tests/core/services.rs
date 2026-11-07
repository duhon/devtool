//! The console's services: the modules the Kernel runs, their tabs, labels, order and the
//! section markers their modules complete.

use services_ui::{console, services, storage};

#[test]
fn the_linked_modules_give_the_tabs_labels_and_order() {
    assert_eq!(
        services::ids(),
        [
            "rest",
            "mysql",
            "graphql",
            "grpc",
            "splunk",
            "terminal",
            "artemis",
            "opensearch"
        ]
    );
    let labels: Vec<_> = services::ids()
        .iter()
        .map(|id| services::label(id))
        .collect();
    assert_eq!(
        labels,
        [
            "REST",
            "MySQL",
            "GraphQL",
            "gRPC",
            "Splunk",
            "Terminal",
            "Artemis",
            "OpenSearch"
        ]
    );
    assert_eq!(services::label("unknown"), "unknown");
    assert_eq!(services::default_order(), services::ids());
    assert_eq!(services::default_service(), "rest");
    assert_eq!(storage::console_state()["service"], "rest");
}

#[test]
fn section_markers_come_from_the_modules() {
    for (service, sections) in [
        ("mysql", &["endpoint", "options", "query"][..]),
        // The console's own `#options` (Repeat, Repeat-Until) come last.
        (
            "grpc",
            &["endpoint", "headers", "proto", "message", "options"],
        ),
        ("splunk", &["endpoint", "headers", "options", "search"]),
        ("terminal", &["endpoint", "command", "env", "options"]),
        (
            "artemis",
            &["endpoint", "headers", "action", "options", "body"],
        ),
    ] {
        assert_eq!(console::markers(service), sections, "{service}");
        let mut templates = vec!["check"];
        templates.extend(
            sections
                .iter()
                .copied()
                .filter(|section| *section != "check"),
        );
        assert_eq!(services::template_sections(service), templates, "{service}");
    }
    assert!(console::markers("unknown").is_empty());
}
