use serde_json::json;
use services_testkit::Host;

#[tokio::test]
async fn saved_templates_are_global_per_section_and_conflict_checked() {
    let host = Host::new();
    let save = |service: &str, section: &str, text: &str, base: &str| {
        json!({"service":"templates","op":"save",
        "serviceName":service,"section":section,"text":text,"base":base,"sessionVariables":{"OTHER":"not a template"}})
    };
    let text = "@my_create_book comment\n{\n  \"id\": 123,\n  \"token\": \"{{var:TOKEN}}\"\n}";
    let saved = host.send(save("rest", "body", text, "")).await;
    assert_eq!(saved.ok()["templates"]["rest"]["body"], text, "{saved}");
    let other = host.reopen();
    let loaded = other.op("templates", "get", json!({})).await;
    assert_eq!(loaded.ok()["templates"]["rest"]["body"], text, "{loaded}");
    let changed = "@different\n{\"id\":2}";
    let conflict = other.send(save("rest", "body", changed, "")).await;
    assert!(conflict.error().contains("CONFLICT"), "{conflict}");
    other
        .send(save("mysql", "query", "@my_create_book SQL\nSELECT 1;", ""))
        .await
        .ok();
    other
        .send(save("rest", "body", "@dup\n1\n@dup\n2", text))
        .await
        .error();
    other
        .send(save("rest", "query", "@wrong\nno", ""))
        .await
        .error();
    other.send(save("rest", "body", changed, text)).await.ok();
    let loaded = host.op("templates", "get", json!({})).await;
    assert_eq!(
        loaded.ok()["templates"]["rest"]["body"],
        changed,
        "{loaded}"
    );
    assert_eq!(
        loaded.ok()["templates"]["mysql"]["query"],
        "@my_create_book SQL\nSELECT 1;",
        "{loaded}"
    );
    other.send(save("rest", "body", "", changed)).await.ok();
    std::fs::write(host.path().join(".console-templates.json"), "not-json").unwrap();
    host.op("templates", "get", json!({})).await.error();
}
