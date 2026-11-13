#![allow(dead_code)]
mod common;

use common::*;
use serde_json::Value;
use services_native::drivers::Drivers;
use services_sdk::Cmd;
use std::collections::VecDeque;
use tokio_util::sync::CancellationToken;

struct Runner {
    module: Harness,
    drivers: Drivers,
    result: Option<Value>,
    discovered: bool,
}

impl Runner {
    fn new() -> Self {
        Self {
            module: Harness::wasm(),
            drivers: Drivers::new().unwrap(),
            result: None,
            discovered: false,
        }
    }

    async fn apply(&mut self, cmds: Vec<Cmd>) {
        let mut cmds = VecDeque::from(cmds);
        while let Some(cmd) = cmds.pop_front() {
            let args = cmd.args();
            match cmd.name.as_str() {
                "vars.resolve" => {
                    let text = args["text"].clone();
                    cmds.extend(self.module.reply(&cmd.tag, text));
                }
                "driver.call" => {
                    let result = self.drivers.call(&args, CancellationToken::new()).await;
                    cmds.extend(match result {
                        Ok(value) => self.module.reply(&cmd.tag, value),
                        Err(error) => self.module.fail(&cmd.tag, &error.message),
                    });
                }
                "run.result" => self.result = Some(args),
                "editor.relight" => self.discovered = true,
                "status.set" | "log" => {}
                other => panic!("Unexpected module command {other}"),
            }
        }
    }

    async fn run(&mut self, uri: &str, action: &str, payload: &str) -> Value {
        self.run_with(uri, "", action, payload).await
    }

    async fn run_with(&mut self, uri: &str, headers: &str, action: &str, payload: &str) -> Value {
        self.result = None;
        let (collection, action) = action.split_once(' ').unwrap_or(("", action));
        let headers = if headers.is_empty() {
            String::new()
        } else {
            format!("#headers\n{headers}\n")
        };
        let cmds = self.module.run(&format!(
            "#target\n{uri}\n{headers}#collection\n{collection}\n#action\n{action}\n#payload\n{payload}"
        ));
        self.apply(cmds).await;
        self.result.take().expect("The run did not finish")
    }

    async fn ok(&mut self, uri: &str, action: &str, payload: &str) -> String {
        let result = self.run(uri, action, payload).await;
        assert_eq!(result["outcome"], "OK", "{action} {payload}: {result}");
        result["body"].as_str().unwrap().to_owned()
    }

    async fn hint(&mut self, uri: &str, tail: &str) -> Vec<String> {
        let text = format!("#target\n{uri}\n{tail}");
        self.discovered = false;
        let cmds = self.module.hints(&text, text.len());
        self.apply(cmds).await;
        assert!(self.discovered, "Discovery did not finish");
        self.module
            .complete(&text, text.len())
            .items
            .into_iter()
            .map(|item| item.label)
            .collect()
    }
}

#[tokio::test]
#[ignore = "requires scripts/test-integration.sh"]
async fn packaged_wasm_runs_statements_through_the_actual_host_driver() {
    let base = std::env::var("MONGODB_URL").expect("Use scripts/test-integration.sh");
    let uri = format!("{}/module_it", base.trim_end_matches('/'));
    let mut runner = Runner::new();
    runner.ok(&uri, "dropDatabase", "").await;
    let inserted = runner
        .ok(
            &uri,
            "games insert",
            r#"[{"title": "Portal", "year": 2007, "tags": ["puzzle"]}, {"title": "Witcher", "year": 2015, "tags": ["rpg"]}]"#,
        )
        .await;
    assert!(inserted.contains("\"n\": 2"), "{inserted}");
    let table = runner
        .ok(
            &uri,
            "games find",
            r#"{"filter": {"year": {"$gt": 2010}}, "projection": {"title": 1, "year": 1, "_id": 0}}"#,
        )
        .await;
    assert!(
        table.contains("Witcher") && !table.contains("Portal"),
        "{table}"
    );
    assert!(
        runner
            .ok(&uri, "games countDocuments", "{}")
            .await
            .contains('2')
    );
    let grouped = runner
        .ok(
            &uri,
            "games aggregate",
            r#"[{"$group": {"_id": null, "years": {"$sum": "$year"}}}]"#,
        )
        .await;
    assert!(grouped.contains("4022"), "{grouped}");
    let updated = runner
        .ok(
            &uri,
            "games update",
            r#"{"filter": {"title": "Portal"}, "update": {"$set": {"year": 2008}}}"#,
        )
        .await;
    assert!(updated.contains("\"nModified\": 1"), "{updated}");
    assert!(
        runner
            .ok(&uri, "games find", r#"{"filter": {"title": "Portal"}}"#)
            .await
            .contains("2008")
    );
    assert!(
        runner
            .ok(&uri, "listCollections", "")
            .await
            .contains("games")
    );
    assert!(
        runner
            .ok(&uri, "listDatabases", "")
            .await
            .contains("module_it")
    );
    assert!(runner.ok(&uri, "ping", "").await.contains("\"ok\": 1"));
    let failed = runner
        .run(&uri, "games aggregate", r#"[{"$bogus": {}}]"#)
        .await;
    assert_eq!(failed["outcome"], "ERROR", "{failed}");
    assert!(
        failed["body"].as_str().unwrap().contains("ERROR 40324"),
        "{failed}"
    );

    let collections = runner.hint(&uri, "#collection\n").await;
    assert!(collections.contains(&"games".into()), "{collections:?}");
    let fields = runner
        .hint(
            &uri,
            "#collection\ngames\n#action\nfind\n#payload\n{\"filter\": {\"",
        )
        .await;
    assert!(
        fields.contains(&"title".into()) && fields.contains(&"tags".into()),
        "{fields:?}"
    );
    runner
        .ok(&uri, "stores insert", r#"[{"name": "Main", "storeId": 7}]"#)
        .await;
    runner
        .ok(
            &uri,
            "inventory insert",
            r#"[{"sku": "a", "storeId": 7, "price": 3}]"#,
        )
        .await;
    let nested = Runner::new()
        .hint(
            &uri,
            concat!(
                "#collection\nstores\n#action\naggregate\n#payload\n",
                r#"{"pipeline": [{"$lookup": {"from": "inventory", "let": {"sid": "$storeId"}, "#,
                r#""pipeline": [{"$match": {"$expr": {"$eq": ["$storeId", "$$sid"]}}}, {"$match": {""#,
            ),
        )
        .await;
    assert!(
        nested.contains(&"sku".into()) && nested.contains(&"price".into()),
        "The inventory is sampled: {nested:?}"
    );
    assert!(!nested.contains(&"name".into()), "{nested:?}");
    runner.ok(&uri, "dropDatabase", "").await;
}

#[tokio::test]
#[ignore = "requires scripts/test-x509.sh"]
async fn x509_client_certificate_authenticates_against_a_real_tls_server() {
    let uri = std::env::var("MONGODB_X509_URL").expect("Use scripts/test-x509.sh");
    let certificate = std::env::var("MONGODB_X509_CERT").expect("Use scripts/test-x509.sh");
    let headers = format!("Auth: x509\nCertificate: {certificate}");
    let mut runner = Runner::new();

    let status = runner
        .run_with(&uri, &headers, "connectionStatus", "")
        .await;
    assert_eq!(status["outcome"], "OK", "{status}");
    assert!(
        status["body"].as_str().unwrap().contains("CN=client"),
        "The client is authenticated by its certificate: {status}"
    );
    let inserted = runner
        .run_with(&uri, &headers, "notes insert", r#"{"text": "hello"}"#)
        .await;
    assert_eq!(inserted["outcome"], "OK", "{inserted}");
    let found = runner
        .run_with(&uri, &headers, "notes find", r#"{"filter": {}}"#)
        .await;
    assert!(found["body"].as_str().unwrap().contains("hello"), "{found}");

    let anonymous = runner.run(&uri, "notes find", r#"{"filter": {}}"#).await;
    assert_eq!(
        anonymous["outcome"], "ERROR",
        "Access control is on: {anonymous}"
    );

    let rejected = runner
        .run_with(
            &uri,
            "Auth: x509\nCertificate: /nonexistent/client.pem",
            "ping",
            "",
        )
        .await;
    assert_eq!(rejected["outcome"], "ERROR", "{rejected}");
}
