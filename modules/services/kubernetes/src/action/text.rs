//! kubectl-like texts the run builds itself: `describe`, `rollout status`, quantities of `top` and
//! label selectors of workloads.

use crate::render::{aligned, time, yaml};
use serde_json::Value;

/// The label selector (`a=b,c in (d,e)`) of a `matchLabels` / `matchExpressions` selector, or of a
/// Service's plain `selector` map.
pub fn selector(selector: &Value) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let labels = selector.get("matchLabels").unwrap_or(selector);
    if let Some(map) = labels.as_object() {
        for (key, value) in map {
            if key == "matchExpressions" || key == "matchLabels" {
                continue;
            }
            parts.push(format!("{key}={}", value.as_str()?));
        }
    }
    for expression in selector["matchExpressions"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let key = expression["key"].as_str()?;
        let values = expression["values"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        parts.push(match expression["operator"].as_str()? {
            "In" => format!("{key} in ({values})"),
            "NotIn" => format!("{key} notin ({values})"),
            "Exists" => key.to_owned(),
            "DoesNotExist" => format!("!{key}"),
            _ => return None,
        });
    }
    (!parts.is_empty()).then(|| parts.join(","))
}

/// A quantity (`250m`, `123456n`, `64Mi`, `2`) in base units.
pub fn quantity(text: &str) -> Option<f64> {
    let split = text
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(text.len());
    let (digits, suffix) = text.split_at(split);
    let number: f64 = digits.parse().ok()?;
    let factor = match suffix {
        "" => 1.0,
        "n" => 1e-9,
        "u" => 1e-6,
        "m" => 1e-3,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        "Ki" => 1024.0,
        "Mi" => 1024.0 * 1024.0,
        "Gi" => 1024.0 * 1024.0 * 1024.0,
        "Ti" => 1024f64.powi(4),
        "Pi" => 1024f64.powi(5),
        _ => return None,
    };
    Some(number * factor)
}

/// Millicores of a CPU quantity, as `top` prints them.
pub fn millicores(cores: f64) -> u64 {
    (cores * 1000.0).round().max(0.0) as u64
}

/// Mebibytes of a memory quantity in bytes.
pub fn mebibytes(bytes: f64) -> u64 {
    (bytes / (1024.0 * 1024.0)).round().max(0.0) as u64
}

fn indent(text: &str, by: usize) -> String {
    text.lines()
        .map(|line| format!("{}{line}", " ".repeat(by)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn labelled(lines: &mut Vec<String>, name: &str, value: String) {
    let mut parts = value.lines();
    lines.push(format!(
        "{:<14}{}",
        format!("{name}:"),
        parts.next().unwrap_or("")
    ));
    lines.extend(parts.map(|part| format!("{:<14}{part}", "")));
}

fn map_lines(value: &Value) -> String {
    match value.as_object() {
        Some(map) if !map.is_empty() => map
            .iter()
            .map(|(key, value)| format!("{key}={}", value.as_str().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => "<none>".into(),
    }
}

fn event_time(event: &Value) -> String {
    ["lastTimestamp", "eventTime", "firstTimestamp"]
        .iter()
        .filter_map(|key| event[*key].as_str())
        .chain(event["metadata"]["creationTimestamp"].as_str())
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// `describe`: the object's metadata, the rest of the object as YAML, and its events.
pub fn describe(object: &Value, events: &[Value], now: f64) -> Vec<String> {
    let metadata = &object["metadata"];
    let mut lines = Vec::new();
    labelled(
        &mut lines,
        "Name",
        metadata["name"].as_str().unwrap_or_default().into(),
    );
    if let Some(namespace) = metadata["namespace"].as_str() {
        labelled(&mut lines, "Namespace", namespace.into());
    }
    labelled(
        &mut lines,
        "Kind",
        format!(
            "{} ({})",
            object["kind"].as_str().unwrap_or_default(),
            object["apiVersion"].as_str().unwrap_or_default()
        ),
    );
    labelled(&mut lines, "Labels", map_lines(&metadata["labels"]));
    let annotations = metadata["annotations"].as_object().map(|map| {
        map.iter()
            .filter(|(key, _)| *key != "kubectl.kubernetes.io/last-applied-configuration")
            .map(|(key, value)| format!("{key}: {}", value.as_str().unwrap_or_default()))
            .collect::<Vec<_>>()
    });
    labelled(
        &mut lines,
        "Annotations",
        annotations
            .filter(|list| !list.is_empty())
            .map_or_else(|| "<none>".into(), |list| list.join("\n")),
    );
    if let Some(created) = metadata["creationTimestamp"].as_str() {
        labelled(
            &mut lines,
            "Created",
            format!("{created} ({} ago)", time::age(now, created)),
        );
    }
    let owners: Vec<String> = metadata["ownerReferences"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|owner| {
            format!(
                "{}/{}",
                owner["kind"].as_str().unwrap_or_default(),
                owner["name"].as_str().unwrap_or_default()
            )
        })
        .collect();
    if !owners.is_empty() {
        labelled(&mut lines, "Controlled By", owners.join("\n"));
    }
    if let Some(map) = object.as_object() {
        for (key, value) in map {
            if matches!(key.as_str(), "apiVersion" | "kind" | "metadata") {
                continue;
            }
            let mut title = key.clone();
            title[..1].make_ascii_uppercase();
            lines.push(format!("{title}:"));
            lines.push(indent(&yaml(value), 2));
        }
    }
    let mut events: Vec<&Value> = events.iter().collect();
    events.sort_by_key(|event| event_time(event));
    if events.is_empty() {
        lines.push("Events:         <none>".into());
        return lines;
    }
    lines.push("Events:".into());
    let header: Vec<String> = ["TYPE", "REASON", "AGE", "FROM", "MESSAGE"]
        .map(String::from)
        .to_vec();
    let rows: Vec<Vec<String>> = events
        .iter()
        .map(|event| {
            let at = event_time(event);
            vec![
                event["type"].as_str().unwrap_or_default().into(),
                event["reason"].as_str().unwrap_or_default().into(),
                time::age(now, &at),
                event["source"]["component"]
                    .as_str()
                    .or(event["reportingComponent"].as_str())
                    .unwrap_or_default()
                    .into(),
                event["message"]
                    .as_str()
                    .unwrap_or_default()
                    .replace('\n', " "),
            ]
        })
        .collect();
    lines.extend(
        aligned(&header, &rows)
            .into_iter()
            .map(|line| format!("  {line}")),
    );
    lines
}

fn count(value: &Value) -> i64 {
    value.as_i64().unwrap_or(0)
}

/// `rollout status` of a Deployment, StatefulSet or DaemonSet: kubectl's message, and whether the
/// rollout is complete.
pub fn rollout_status(object: &Value) -> Result<(String, bool), String> {
    let name = object["metadata"]["name"].as_str().unwrap_or_default();
    let (spec, status) = (&object["spec"], &object["status"]);
    let observed = count(&status["observedGeneration"]);
    let waiting = observed < count(&object["metadata"]["generation"]);
    match object["kind"].as_str().unwrap_or_default() {
        "Deployment" => {
            let wanted = spec["replicas"].as_i64().unwrap_or(1);
            let (replicas, updated, available) = (
                count(&status["replicas"]),
                count(&status["updatedReplicas"]),
                count(&status["availableReplicas"]),
            );
            if waiting {
                return Ok((
                    "Waiting for deployment spec update to be observed...".into(),
                    false,
                ));
            }
            if spec["paused"] == true {
                return Ok((format!("Deployment {name:?} is paused"), false));
            }
            let pending = format!("Waiting for deployment {name:?} rollout to finish");
            Ok(if updated < wanted {
                (
                    format!(
                        "{pending}: {updated} out of {wanted} new replicas have been updated..."
                    ),
                    false,
                )
            } else if replicas > updated {
                (
                    format!(
                        "{pending}: {} old replicas are pending termination...",
                        replicas - updated
                    ),
                    false,
                )
            } else if available < updated {
                (
                    format!(
                        "{pending}: {available} of {updated} updated replicas are available..."
                    ),
                    false,
                )
            } else {
                (format!("deployment {name:?} successfully rolled out"), true)
            })
        }
        "StatefulSet" => {
            if waiting {
                return Ok((
                    "Waiting for statefulset spec update to be observed...".into(),
                    false,
                ));
            }
            let wanted = spec["replicas"].as_i64().unwrap_or(1);
            let ready = count(&status["readyReplicas"]);
            if ready < wanted {
                return Ok((
                    format!("Waiting for {} pods to be ready...", wanted - ready),
                    false,
                ));
            }
            let (current, update) = (
                status["currentRevision"].as_str().unwrap_or_default(),
                status["updateRevision"].as_str().unwrap_or_default(),
            );
            if current != update && count(&status["updatedReplicas"]) < wanted {
                return Ok((
                    format!(
                        "Waiting for {} pods to be updated...",
                        wanted - count(&status["updatedReplicas"])
                    ),
                    false,
                ));
            }
            Ok((
                format!("partitioned roll out complete: {wanted} new pods have been updated..."),
                true,
            ))
        }
        "DaemonSet" => {
            if waiting {
                return Ok((
                    "Waiting for daemon set spec update to be observed...".into(),
                    false,
                ));
            }
            let (desired, updated, available) = (
                count(&status["desiredNumberScheduled"]),
                count(&status["updatedNumberScheduled"]),
                count(&status["numberAvailable"]),
            );
            let pending = format!("Waiting for daemon set {name:?} rollout to finish");
            Ok(if updated < desired {
                (
                    format!("{pending}: {updated} out of {desired} new pods have been updated..."),
                    false,
                )
            } else if available < desired {
                (
                    format!("{pending}: {available} of {desired} updated pods are available..."),
                    false,
                )
            } else {
                (format!("daemon set {name:?} successfully rolled out"), true)
            })
        }
        other => Err(format!(
            "rollout status works on deployments, statefulsets and daemonsets, not {other}"
        )),
    }
}
