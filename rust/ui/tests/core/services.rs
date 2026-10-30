//! The console's module registry: built-in seed, host lists and what the
//! console derives from manifests (tabs, labels, sections, new documents).

use serde_json::json;
use services_ui::{console, services, storage};

#[test]
fn the_built_in_modules_seed_the_tabs_labels_and_order() {
    assert_eq!(*services::all(), services::seed());
    assert_eq!(
        services::ids(),
        ["rest", "mysql", "graphql", "grpc", "splunk", "terminal"]
    );
    let labels: Vec<_> = services::ids()
        .iter()
        .map(|id| services::label(id))
        .collect();
    assert_eq!(
        labels,
        ["REST", "MySQL", "GraphQL", "gRPC", "Splunk", "Terminal"]
    );
    assert_eq!(services::label("unknown"), "unknown");
    assert_eq!(
        services::default_order(),
        ["rest", "mysql", "graphql", "grpc", "splunk", "terminal"]
    );
    assert_eq!(services::default_service(), "rest");
    assert_eq!(storage::console_state()["service"], "rest");
}

#[test]
fn sections_and_new_documents_come_from_the_manifests() {
    for (service, sections) in [
        ("mysql", &["endpoint", "options", "query"][..]),
        ("rest", &["endpoint", "headers", "body"]),
        ("graphql", &["endpoint", "headers", "variables", "query"]),
        ("grpc", &["endpoint", "headers", "proto", "message"]),
        ("splunk", &["endpoint", "headers", "options", "search"]),
        ("terminal", &["endpoint", "command", "env"]),
    ] {
        assert_eq!(console::markers(service), sections, "{service}");
        let mut template = vec!["check"];
        template.extend(sections);
        assert_eq!(services::template_sections(service), template, "{service}");
        assert!(services::has_template_section(service, "check"));
        assert!(!services::has_template_section(service, "response"));
    }
    assert!(console::markers("unknown").is_empty());
    assert!(services::template_sections("unknown").is_empty());
}

#[test]
fn host_module_lists_are_validated_before_they_replace_the_registry() {
    let seed = services::seed();
    assert_eq!(services::parse(&json!({"modules": seed})).unwrap(), seed);
    assert_eq!(services::install(seed.clone()), Ok(false));
    let broken = |change: fn(&mut Vec<services_contracts::manifest::Manifest>)| {
        let mut list = seed.clone();
        change(&mut list);
        services::parse(&json!({"modules": list}))
    };
    // Every module can be removed in the settings.
    assert_eq!(broken(|list| list.clear()), Ok(Vec::new()));
    assert!(broken(|list| list[1].id = list[0].id.clone()).is_err());
    assert!(broken(|list| list[0].id.clear()).is_err());
    assert!(broken(|list| list[0].console.payload = "missing".into()).is_err());
    assert!(
        broken(|list| {
            list[0].console.sections.remove(0);
        })
        .is_err()
    );
    assert!(
        broken(|list| {
            let again = list[0].console.sections[1].clone();
            list[0].console.sections.push(again)
        })
        .is_err()
    );
    fn rest(
        list: &mut [services_contracts::manifest::Manifest],
    ) -> &mut Vec<services_contracts::manifest::Section> {
        let at = list.iter().position(|m| m.id == "rest").unwrap();
        &mut list[at].console.sections
    }
    assert!(broken(|list| rest(list)[0].connection = false).is_err());
    assert!(broken(|list| rest(list)[1].param = None).is_err());
    assert!(broken(|list| rest(list)[2].param = None).is_ok());
    assert!(services::parse(&json!({"modules": [{"name": "no id"}]})).is_err());
    assert!(services::parse(&json!({})).is_err());
}
