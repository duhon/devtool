//! The module through the real host driver against a throwaway k3s (`host/scripts/kubernetes-integration.sh`).
//! `KUBERNETES_TEST_KUBECONFIG` lists its kubeconfig files (the context `default` is the admin);
//! the tests pass without running when it is not set, and never read the user's kubeconfig.
#![allow(dead_code)]
mod common;

use common::*;
use serde_json::Value;
use services_native::drivers::Drivers;
use services_sdk::Cmd;
use services_sdk::wire::{self, event};
use std::collections::VecDeque;
use tokio_util::sync::CancellationToken;

struct Runner {
    module: Harness,
    drivers: Drivers,
    result: Option<Value>,
    relit: bool,
}

impl Runner {
    fn new(kubeconfig: &str) -> Self {
        Self {
            module: Harness::default(),
            drivers: Drivers::new().unwrap().with_kubeconfig(kubeconfig),
            result: None,
            relit: false,
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
                "editor.relight" => self.relit = true,
                _ => {}
            }
        }
    }

    /// The result of the check: its `body`, `error` and `outcome`.
    async fn run(
        &mut self,
        namespace: &str,
        action: &str,
        metadata: &str,
        options: &str,
        payload: &str,
    ) -> Value {
        self.result = None;
        let text = format!(
            "#target\nk8s://default/{namespace}\n#action\n{action}\n#metadata\n{metadata}\n#options\n{options}\n#payload\n{payload}"
        );
        let cmds = self.module.run(&text);
        self.apply(cmds).await;
        let result = self.result.take().expect("the run did not finish");
        eprintln!(
            "--- {namespace}: {action} | {metadata} | {options}\n{}",
            brief(&result)
        );
        result
    }

    async fn ok(
        &mut self,
        namespace: &str,
        action: &str,
        metadata: &str,
        options: &str,
        payload: &str,
    ) -> String {
        let result = self
            .run(namespace, action, metadata, options, payload)
            .await;
        assert_eq!(result["outcome"], "OK", "{action}: {result}");
        result["body"].as_str().unwrap_or_default().to_owned()
    }

    /// The labels completion offers at the end of `text`, once discovery has answered.
    async fn offered(&mut self, text: &str) -> Vec<String> {
        let check = Some(check(text));
        let pos = text.len() as u32;
        let cmds = self.module.send(event::Kind::Hints(wire::CompleteRequest {
            check: check.clone(),
            pos,
        }));
        self.apply(cmds).await;
        let labels: Vec<String> = self
            .module
            .complete(text, text.len())
            .items
            .into_iter()
            .map(|item| item.label)
            .collect();
        eprintln!("--- complete {text:?}\n{labels:?}");
        labels
    }
}

fn brief(result: &Value) -> String {
    let text = result["body"]
        .as_str()
        .or_else(|| result["error"].as_str())
        .unwrap_or_default();
    let lines: Vec<&str> = text.lines().take(6).collect();
    format!("[{}] {}", result["outcome"], lines.join("\n"))
}

fn kubeconfig() -> Option<String> {
    let value = std::env::var("KUBERNETES_TEST_KUBECONFIG")
        .ok()
        .filter(|value| !value.is_empty());
    if value.is_none() {
        eprintln!("KUBERNETES_TEST_KUBECONFIG is not set; skipping");
    }
    value
}

#[tokio::test(flavor = "multi_thread")]
async fn kubernetes_k3s_module_reads_through_the_host_driver() {
    let Some(config) = kubeconfig() else { return };
    let mut runner = Runner::new(&config);

    let raw = runner.ok("kube-system", "get pods", "", "", "").await;
    assert!(raw.starts_with("NAME") && raw.contains("coredns"), "{raw}");
    let table = runner
        .ok("kube-system", "get pods", "", "Format: table", "")
        .await;
    assert!(
        table.contains("coredns") && table.contains("NAME"),
        "{table}"
    );

    let pod = raw
        .lines()
        .find(|line| line.starts_with("coredns"))
        .and_then(|line| line.split_whitespace().next())
        .unwrap()
        .to_owned();
    let described = runner
        .ok("kube-system", &format!("describe pods/{pod}"), "", "", "")
        .await;
    assert!(
        described.contains(&pod) && described.contains("Events"),
        "{described}"
    );
    let logs = runner
        .ok(
            "kube-system",
            &format!("logs pods/{pod}"),
            "Tail: 3",
            "",
            "",
        )
        .await;
    assert!(logs.lines().count() <= 3 && !logs.is_empty(), "{logs}");
    let events = runner.ok("kube-system", "events", "", "", "").await;
    assert!(
        events.contains("LAST SEEN") || events.contains("REASON"),
        "{events}"
    );
    let resources = runner.ok("default", "api-resources", "", "", "").await;
    assert!(
        resources.contains("configmaps") && resources.contains("deployments"),
        "{resources}"
    );

    let missing = runner.run("default", "get pods/nope", "", "", "").await;
    assert_eq!(missing["outcome"], "ERROR", "{missing}");
    assert!(
        missing["error"]
            .as_str()
            .unwrap()
            .starts_with("Error from server (NotFound)"),
        "{missing}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn kubernetes_k3s_module_writes_redacts_and_completes() {
    let Some(config) = kubeconfig() else { return };
    let mut runner = Runner::new(&config);
    let ns = format!("sdt-e2e-{}", std::process::id());

    runner
        .ok(
            &ns,
            "apply",
            "",
            "",
            &format!("apiVersion: v1\nkind: Namespace\nmetadata:\n  name: {ns}"),
        )
        .await;
    let map = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cm\ndata:\n  a: \"1\"";
    let applied = runner.ok(&ns, "apply", "", "", map).await;
    assert!(applied.contains("configmap/cm"), "{applied}");
    let patched = runner
        .ok(
            &ns,
            "patch configmaps/cm",
            "Patch-Type: merge",
            "",
            r#"{"data": {"a": "2"}}"#,
        )
        .await;
    assert!(patched.contains("configmap/cm patched"), "{patched}");
    let json = runner
        .ok(&ns, "get configmaps/cm", "", "Format: json", "")
        .await;
    assert!(json.contains("\"a\": \"2\""), "{json}");

    let secret =
        "apiVersion: v1\nkind: Secret\nmetadata:\n  name: s\nstringData:\n  password: hunter2";
    runner.ok(&ns, "create", "", "", secret).await;
    let secrets = runner
        .ok(&ns, "get secrets/s", "", "Format: yaml", "")
        .await;
    assert!(
        secrets.contains("<redacted>") && !secrets.contains("hunter2"),
        "{secrets}"
    );
    let listed = runner.ok(&ns, "get secrets", "", "Format: json", "").await;
    assert!(
        !listed.contains("aHVudGVyMg") && !listed.contains("hunter2"),
        "{listed}"
    );

    let offered = runner.offered("#target\nk8s://").await;
    assert!(offered.iter().any(|l| l == "default/"), "{offered:?}");
    let offered = runner.offered("#target\nk8s://default/").await;
    assert!(
        offered.iter().any(|l| l == "kube-system") && offered.contains(&ns),
        "{offered:?}"
    );
    let offered = runner
        .offered(&format!("#target\nk8s://default/{ns}\n#action\nget "))
        .await;
    assert!(offered.iter().any(|l| l == "configmaps"), "{offered:?}");
    let offered = runner
        .offered(&format!(
            "#target\nk8s://default/{ns}\n#action\nget configmaps/"
        ))
        .await;
    assert!(offered.iter().any(|l| l == "cm"), "{offered:?}");

    let deleted = runner.ok(&ns, "delete configmaps/cm", "", "", "").await;
    assert!(deleted.contains("configmap \"cm\" deleted"), "{deleted}");
    runner
        .ok(&ns, &format!("delete namespaces/{ns}"), "", "", "")
        .await;
}
