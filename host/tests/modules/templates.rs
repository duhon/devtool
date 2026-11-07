use serde_json::json;
use services_testkit::Host;

/// Modules declare no template sections yet (REFACTORING step 5), so the store only reads;
/// conflicts and section checks are covered by the store's unit tests.
#[tokio::test]
async fn templates_are_rejected_until_modules_declare_sections() {
    let host = Host::new();
    let saved = host
        .send(json!({"service":"templates","op":"save",
            "serviceName":"artemis","section":"body","text":"@kept\n{}","base":""}))
        .await;
    assert!(saved.error().contains("section"), "{saved}");
    let loaded = host.op("templates", "get", json!({})).await;
    assert_eq!(loaded.ok()["templates"], json!({}), "{loaded}");
    std::fs::write(host.path().join(".console-templates.json"), "not-json").unwrap();
    host.op("templates", "get", json!({})).await.error();
}
