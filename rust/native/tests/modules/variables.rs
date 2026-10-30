//! Variables and Dictionary replies carry the real environment
//! (`context.profiles.env`), so failure messages here never print them.

use serde_json::{Value, json};
use services_testkit::{
    DataDir, Host,
    http::{self, Server},
};

/// A REST request to the fixture's `/echo`, which answers with the body it
/// received, so the reply shows how the host resolved `body`.
fn echo(server: &Server, body: &str) -> Value {
    json!({"method":"POST","url":format!("{}/echo",server.url),"body":body})
}

async fn list(host: &Host, store: &str) -> Value {
    host.op(store, "list", json!({})).await.ok().clone()
}

#[tokio::test]
async fn legacy_profiles_migrate_once_into_independent_files() {
    let data = DataDir::new();
    let path = data.path().join(".variable-profiles.json");
    let legacy = json!({"order":["shop","var","default"],"profiles":{
        "shop":{"HOST":"fixture.invalid","FROM_VAR":"{{var:BASE}}"},
        "var":{"BASE":"local","HOST":"local.invalid","FROM_DICT":"{{shop:HOST}}"},
        "default":{"TOKEN":"preserved"}
    }});
    let original = serde_json::to_vec_pretty(&legacy).unwrap();
    std::fs::write(&path, &original).unwrap();
    let host = Host::on(data);
    let (server, _) = http::services().await;
    let vars = list(&host, "vars").await;
    let dictionary = list(&host, "dictionary").await;
    assert_eq!(vars["order"], json!(["var"]));
    assert!(vars["profiles"]["env"].is_object());
    assert!(vars["profiles"].get("shop").is_none());
    assert_eq!(dictionary["order"], json!(["shop", "default"]));
    assert!(dictionary["profiles"].get("var").is_none());
    assert!(dictionary["profiles"].get("env").is_none());
    assert_eq!(dictionary["resolvedProfiles"]["shop"]["FROM_VAR"], "local");
    assert_eq!(
        vars["resolvedProfiles"]["var"]["FROM_DICT"],
        "fixture.invalid"
    );
    assert_eq!(
        std::fs::read(host.path().join(".variable-profiles.json.pre-split.bak")).unwrap(),
        original
    );
    let reply = host
        .call(
            "rest",
            echo(&server, "{{:HOST}}/{{var:FROM_DICT}}/{{shop:FROM_VAR}}"),
        )
        .await;
    assert_eq!(
        reply.ok()["body"],
        "local.invalid/fixture.invalid/local",
        "{reply}"
    );
    let dictionary_path = host.path().join(".dictionary-profiles.json");
    let dictionary_before = std::fs::read(&dictionary_path).unwrap();
    host.op(
        "vars",
        "save",
        json!({"name":"var","variables":{"BASE":"changed"}}),
    )
    .await
    .ok();
    assert_eq!(std::fs::read(&dictionary_path).unwrap(), dictionary_before);
    let variables_before = std::fs::read(&path).unwrap();
    host.op(
        "dictionary",
        "save",
        json!({"name":"new","variables":{"REF":"{{var:BASE}}"}}),
    )
    .await
    .ok();
    assert_eq!(std::fs::read(&path).unwrap(), variables_before);
    assert_eq!(
        list(&host.reopen(), "dictionary").await["resolvedProfiles"]["new"]["REF"],
        "changed"
    );
    for (op, fields) in [
        ("save", json!({"name":"other","variables":{}})),
        ("delete", json!({"name":"var"})),
        ("rename", json!({"name":"var","newName":"other"})),
        ("reorder", json!({"order":["var"]})),
    ] {
        let reply = host.op("vars", op, fields).await;
        assert!(!reply.is_ok(), "vars {op} must be rejected");
    }
    for name in ["var", "env"] {
        let saved = host
            .op("dictionary", "save", json!({"name":name,"variables":{}}))
            .await;
        assert!(
            !saved.is_ok(),
            "a dictionary profile cannot be named {name}"
        );
        let renamed = host
            .op("dictionary", "rename", json!({"name":"new","newName":name}))
            .await;
        assert!(
            !renamed.is_ok(),
            "a dictionary profile cannot be renamed to {name}"
        );
    }
    let variable_disk: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        variable_disk["profiles"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec!["var"]
    );
}

#[tokio::test]
async fn migration_rejects_bad_data_and_allows_an_empty_dictionary() {
    let data = DataDir::new();
    let path = data.path().join(".variable-profiles.json");
    std::fs::write(&path, b"{invalid").unwrap();
    let host = Host::on(data);
    let reply = host.op("vars", "list", json!({})).await;
    assert!(!reply.is_ok(), "corrupt variables must not load");
    assert_eq!(std::fs::read(&path).unwrap(), b"{invalid");
    std::fs::write(&path, br#"{"profiles":{"default":{"X":"kept"}}}"#).unwrap();
    let host = host.reopen();
    assert_eq!(list(&host, "vars").await["profiles"]["var"], json!({}));
    assert_eq!(
        list(&host, "dictionary").await["profiles"]["default"]["X"],
        "kept"
    );
    host.op("dictionary", "delete", json!({"name":"default"}))
        .await
        .ok();
    assert_eq!(list(&host, "dictionary").await["order"], json!([]));
    assert_eq!(
        list(&host.reopen(), "dictionary").await["profiles"],
        json!({})
    );
    assert_eq!(list(&host, "vars").await["order"], json!(["var"]));
}

#[tokio::test]
async fn migration_resumes_partial_writes_and_rejects_conflicts() {
    for conflict in [false, true] {
        let data = DataDir::new();
        let path = data.path().join(".variable-profiles.json");
        let legacy =
            serde_json::to_vec(&json!({"order":["shop"],"profiles":{"shop":{"HOST":"original"}}}))
                .unwrap();
        std::fs::write(&path, &legacy).unwrap();
        std::fs::write(
            data.path().join(".variable-profiles.json.pre-split.bak"),
            &legacy,
        )
        .unwrap();
        let dictionary_path = data.path().join(".dictionary-profiles.json");
        let partial=serde_json::to_vec(&json!({"version":2,"order":["shop"],"profiles":{"shop":{"HOST":if conflict {"different"} else {"original"}}}})).unwrap();
        std::fs::write(&dictionary_path, &partial).unwrap();
        let host = Host::on(data);
        let reply = host.op("vars", "list", json!({})).await;
        if conflict {
            assert!(
                !reply.is_ok(),
                "a conflicting partial migration must not load"
            );
            assert_eq!(std::fs::read(&path).unwrap(), legacy);
            assert_eq!(std::fs::read(&dictionary_path).unwrap(), partial);
        } else {
            reply.ok();
            let variables: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(variables["version"], 2);
            assert_eq!(
                list(&host.reopen(), "dictionary").await["profiles"]["shop"]["HOST"],
                "original"
            );
        }
    }
    let data = DataDir::new();
    let path = data.path().join(".variable-profiles.json");
    let original = br#"{"profiles":{"default":{}}}"#;
    std::fs::write(&path, original).unwrap();
    std::fs::write(
        data.path().join(".variable-profiles.json.pre-split.bak"),
        b"partial backup",
    )
    .unwrap();
    let host = Host::on(data);
    let reply = host.op("vars", "list", json!({})).await;
    assert!(!reply.is_ok(), "a partial backup must stop the migration");
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_hosts_serialize_migration_and_mutations() {
    let host = Host::new();
    let mut saves = tokio::task::JoinSet::new();
    for index in 0..8 {
        let window = host.reopen();
        saves.spawn(async move {
            window
                .op(
                    "dictionary",
                    "save",
                    json!({"name":format!("profile-{index}"),"variables":{"X":index}}),
                )
                .await
                .ok();
        });
    }
    while let Some(saved) = saves.join_next().await {
        saved.expect("every concurrent save must succeed");
    }
    assert_eq!(
        list(&host.reopen(), "dictionary").await["order"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
}

#[tokio::test]
async fn profiles_persist_resolve_by_priority_and_keep_env_read_only() {
    let host = Host::new();
    let (server, _) = http::services().await;
    host.op("dictionary", "save", json!({"name":"default","variables":{"name":"first","url":"https://{{second:host}}","same":"{{default:name}}"}})).await.ok();
    host.op(
        "dictionary",
        "save",
        json!({"name":"second","variables":{"name":"second","host":"fixture.invalid"}}),
    )
    .await
    .ok();
    let mut params = echo(&server, "{{:url}}/{{default:same}}");
    params["headers"] = json!({"X":"{{second:name}}"});
    let reply = host.call("rest", params).await;
    assert_eq!(
        reply.ok()["body"],
        "https://fixture.invalid/first",
        "{reply}"
    );
    assert_eq!(
        reply.ok()["debug"]["request"]["headers"]["X"],
        "second",
        "{reply}"
    );
    assert_eq!(
        list(&host.reopen(), "dictionary").await["resolvedProfiles"]["default"]["url"],
        "https://fixture.invalid"
    );
    host.op(
        "dictionary",
        "reorder",
        json!({"order":["second","default"]}),
    )
    .await
    .ok();
    let reply = host.call("rest", echo(&server, "{{:name}}")).await;
    assert_eq!(reply.ok()["body"], "second", "{reply}");
    let reply = host
        .op(
            "dictionary",
            "reorder",
            json!({"order":["second","second"]}),
        )
        .await;
    assert!(!reply.is_ok(), "a duplicate order must be rejected");
    for op in ["save", "delete", "rename"] {
        let reply = host
            .op(
                "dictionary",
                op,
                json!({"name":"env","variables":{},"newName":"x"}),
            )
            .await;
        assert!(!reply.is_ok(), "dictionary {op} on env must be rejected");
    }
    assert_eq!(
        list(&host, "dictionary").await["context"]["profiles"]["env"]["PATH"],
        std::env::var("PATH").unwrap()
    );
    let renamed = host
        .op(
            "dictionary",
            "rename",
            json!({"name":"second","newName":"renamed"}),
        )
        .await
        .ok()
        .clone();
    assert!(
        renamed["validation"]["default"]
            .as_str()
            .unwrap()
            .contains("second")
    );
    assert_eq!(renamed["order"], json!(["renamed", "default"]));
}

#[tokio::test]
async fn mutations_return_full_state_with_env_and_resolved_profiles() {
    let host = Host::new();
    for (op, fields) in [
        (
            "save",
            json!({"name":"default","variables":{"BASE":"fixture","REF":"{{default:BASE}}"}}),
        ),
        ("save", json!({"name":"second","variables":{}})),
        ("reorder", json!({"order":["second","default"]})),
        ("rename", json!({"name":"second","newName":"renamed"})),
        ("delete", json!({"name":"renamed"})),
    ] {
        let reply = host.op("dictionary", op, fields).await.ok().clone();
        assert!(reply["context"]["profiles"]["env"].is_object());
        assert!(reply["profiles"].get("env").is_none());
        assert!(reply["profiles"].get("var").is_none());
        assert_eq!(reply["resolvedProfiles"]["default"]["REF"], "fixture");
        // Do not include real environment values in assertion failures.
        assert!(
            reply == list(&host, "dictionary").await,
            "Mutation must return the complete Variables state"
        );
    }
}
