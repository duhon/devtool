#![allow(dead_code)]
mod common;

use common::*;
use services_sdk::manifest::Manifest;

#[test]
fn the_manifest_parses_and_names_the_kubernetes_driver() {
    let manifest = Manifest::parse(include_str!("../manifest.json")).unwrap();
    assert_eq!(manifest.id, "kubernetes");
    assert_eq!(manifest.drivers["kubernetes"], "1");
}

#[test]
fn the_module_offers_the_sections_and_a_run_resolves_its_variables_first() {
    let mut module = Harness::default();
    let items = module.complete("#", 1).items;
    let labels: Vec<_> = items.iter().map(|item| item.label.as_str()).collect();
    for section in services_module_kubernetes::SECTIONS {
        assert!(
            labels.contains(&format!("#{section}").as_str()),
            "{labels:?}"
        );
    }
    let cmds = module.run("#target\nk8s://dev/default\n#action\nget pods");
    assert!(
        cmds.iter().any(|cmd| cmd.name == "vars.resolve"),
        "{cmds:?}"
    );
}
