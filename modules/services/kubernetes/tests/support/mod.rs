//! A fake cluster behind the driver and helpers for the editor and discovery tests.
#![allow(dead_code)]

use crate::common::{Harness, check};
use serde_json::{Value, json};
use services_sdk::Cmd;
use services_sdk::wire::{self, answer, event, request};

/// Splits a `|` cursor out of `text`.
pub fn cursor(text: &str) -> (String, usize) {
    let at = text.find('|').expect("a | cursor");
    (text.replacen('|', "", 1), at)
}

fn object(name: &str, labels: Value) -> Value {
    json!({"metadata": {"name": name, "labels": labels}})
}

fn resource(name: &str, singular: &str, kind: &str, short: &[&str], scope: &str) -> Value {
    json!({
        "resource": name,
        "singularResource": singular,
        "scope": scope,
        "responseKind": {"kind": kind},
        "shortNames": short,
        "verbs": ["get", "list", "watch", "delete", "patch", "create"],
        "subresources": if name == "deployments" { json!([{"subresource": "scale"}]) } else { json!([]) },
    })
}

fn schemas() -> Value {
    json!({"components": {"schemas": {
        "io.k8s.api.apps.v1.Deployment": {
            "type": "object",
            "x-kubernetes-group-version-kind": [{"group": "apps", "version": "v1", "kind": "Deployment"}],
            "properties": {
                "apiVersion": {"type": "string"},
                "kind": {"type": "string"},
                "metadata": {"type": "object", "properties": {
                    "name": {"type": "string"}, "namespace": {"type": "string"}}},
                "spec": {"$ref": "#/components/schemas/io.k8s.api.apps.v1.DeploymentSpec"},
            },
        },
        "io.k8s.api.apps.v1.DeploymentSpec": {
            "type": "object",
            "required": ["selector"],
            "properties": {
                "replicas": {"type": "integer", "description": "Number of replicas."},
                "selector": {"type": "object", "properties": {"matchLabels": {"type": "object"}}},
                "strategy": {"type": "object", "properties": {
                    "type": {"type": "string", "enum": ["Recreate", "RollingUpdate"]}}},
            },
        },
    }}})
}

/// What the cluster answers to a driver request; `None` for a path it does not serve.
pub fn serve(request: &Value) -> Value {
    if request["op"] == "contexts" {
        return json!({
            "contexts": [
                {"name": "dev", "cluster": "dev", "server": "https://dev"},
                {"name": "prod", "cluster": "prod", "server": "https://prod"},
            ],
            "current": "dev",
        });
    }
    let path = request["path"].as_str().unwrap_or("");
    let body = match path {
        "/api" => json!({"kind": "APIGroupDiscoveryList", "items": [{
            "metadata": {"name": ""},
            "versions": [{"version": "v1", "resources": [
                resource("pods", "pod", "Pod", &["po"], "Namespaced"),
                resource("services", "service", "Service", &["svc"], "Namespaced"),
            ]}],
        }]}),
        "/apis" => json!({"kind": "APIGroupDiscoveryList", "items": [{
            "metadata": {"name": "apps"},
            "versions": [{"version": "v1", "resources": [
                resource("deployments", "deployment", "Deployment", &["deploy"], "Namespaced"),
                resource("statefulsets", "statefulset", "StatefulSet", &["sts"], "Namespaced"),
            ]}],
        }]}),
        "/api/v1/namespaces" => json!({"items": [
            object("default", json!({})), object("kube-system", json!({}))]}),
        "/api/v1/namespaces/default/pods" => json!({"items": [
            object("web-1", json!({"app": "web", "tier": "front"})),
            object("web-2", json!({"app": "web"})),
            object("db-1", json!({"app": "db"})),
        ]}),
        "/api/v1/namespaces/default/pods/web-1" => json!({"kind": "Pod", "spec": {
            "containers": [{"name": "app"}, {"name": "sidecar"}]}}),
        "/openapi/v3" => json!({"paths": {
            "api/v1": {"serverRelativeURL": "/openapi/v3/api/v1?hash=A"},
            "apis/apps/v1": {"serverRelativeURL": "/openapi/v3/apis/apps/v1?hash=B"},
        }}),
        "/openapi/v3/apis/apps/v1" => schemas(),
        _ => {
            return json!({"status": 404, "body": {},
                "error": {"reason": "NotFound", "message": "not found", "code": 404}});
        }
    };
    json!({"status": 200, "body": body})
}

/// A harness whose discovery calls are answered by [`serve`].
#[derive(Default)]
pub struct Cluster {
    pub module: Harness,
    /// Every driver request made so far: its context, path and `Accept`.
    pub requests: Vec<Value>,
}

impl Cluster {
    /// Answers the driver calls in `cmds` and those their answers lead to; the other commands
    /// are returned.
    pub fn pump(&mut self, cmds: Vec<Cmd>) -> Vec<Cmd> {
        let mut rest = Vec::new();
        let mut queue = cmds;
        while !queue.is_empty() {
            let mut next = Vec::new();
            for cmd in queue {
                if cmd.name == "driver.call" {
                    let request = cmd.args()["request"].clone();
                    let answer = serve(&request);
                    self.requests.push(request);
                    next.extend(self.module.reply(&cmd.tag, answer));
                } else {
                    rest.push(cmd);
                }
            }
            queue = next;
        }
        rest
    }

    /// Hints at the `|` of `text`, with every answer delivered.
    pub fn hints(&mut self, text: &str) -> Vec<Cmd> {
        let (text, pos) = cursor(text);
        let cmds = self.module.send(event::Kind::Hints(wire::CompleteRequest {
            check: Some(check(&text)),
            pos: pos as u32,
        }));
        self.pump(cmds)
    }

    pub fn discover(&mut self, text: &str) -> Vec<Cmd> {
        let cmds = self.module.send(event::Kind::Discover(check(text)));
        self.pump(cmds)
    }

    /// The labels completion offers at the `|` of `text`.
    pub fn labels(&mut self, text: &str) -> Vec<String> {
        let (text, pos) = cursor(text);
        let found = self.module.complete(&text, pos);
        found.items.into_iter().map(|item| item.label).collect()
    }

    /// Hints, then the labels.
    pub fn offered(&mut self, text: &str) -> Vec<String> {
        self.hints(text);
        self.labels(text)
    }

    /// The marked texts of every line.
    pub fn marked(&mut self, text: &str) -> Vec<String> {
        let answer::Kind::Highlighted(lines) = self.module.call(request::Kind::Marks(check(text)))
        else {
            panic!("Expected marks")
        };
        let source: Vec<&str> = text.split('\n').collect();
        let mut found = Vec::new();
        for (line, marks) in source.iter().zip(&lines.lines) {
            for token in &marks.tokens {
                let start = token.start as usize;
                found.push(line[start..start + token.len as usize].to_owned());
            }
        }
        found
    }

    pub fn paths(&self) -> Vec<String> {
        self.requests
            .iter()
            .map(|request| request["path"].as_str().unwrap_or("contexts").to_owned())
            .collect()
    }
}
