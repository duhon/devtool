//! The API resource catalog of a cluster: what `kubectl api-resources` lists. Shared by the run
//! ([`action`](crate::action), which resolves the resource of an `#action` and builds its paths)
//! and by discovery (hints for resources, kinds, names and short names). Pure: it builds the
//! discovery requests and parses their answers, it makes no [`Cmd`](services_sdk::Cmd)s.
//!
//! Usage (discovery and run alike):
//! 1. [`requests`] gives the two aggregated discovery calls (`GET /api`, `GET /apis`) of a
//!    context. Send them in this order and keep it: the core group must come first, because
//!    [`resolve`] prefers the first match (`events` is the core `Event`, not `events.k8s.io`).
//! 2. Parse each answer with [`parse`]. A server without aggregated discovery answers with the
//!    legacy `APIVersions` / `APIGroupList`; [`parse`] then returns [`Parsed::Legacy`] with the
//!    paths (`/api/v1`, `/apis/apps/v1`…) to `GET` and read with [`parse_list`].
//! 3. [`resolve`] finds a [`Resource`] by plural, singular, short name or kind (any case), or by
//!    `plural.group` / `kind.group`; the [`Resource`] builds the paths.

use serde_json::Value;
use services_sdk::kubernetes::Request;

/// `Accept` of the aggregated discovery of the API server (Kubernetes 1.27+).
pub const AGGREGATED: &str = "application/json;g=apidiscovery.k8s.io;v=v2;as=APIGroupDiscoveryList,application/json;g=apidiscovery.k8s.io;v=v2beta1;as=APIGroupDiscoveryList,application/json";

/// One kind of object the API serves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resource {
    /// The API group, empty for the core group.
    pub group: String,
    /// The preferred version served, `v1`, `v1beta1`…
    pub version: String,
    pub kind: String,
    /// `pods`.
    pub plural: String,
    /// `pod`.
    pub singular: String,
    /// `po`.
    pub short_names: Vec<String>,
    pub namespaced: bool,
    /// `get`, `list`, `watch`, `create`, `delete`…
    pub verbs: Vec<String>,
    /// `log`, `status`, `scale`…
    pub subresources: Vec<String>,
}

impl Resource {
    /// `v1` of the core group, `apps/v1` otherwise.
    pub fn api_version(&self) -> String {
        if self.group.is_empty() {
            self.version.clone()
        } else {
            format!("{}/{}", self.group, self.version)
        }
    }

    fn base(&self) -> String {
        if self.group.is_empty() {
            format!("/api/{}", self.version)
        } else {
            format!("/apis/{}/{}", self.group, self.version)
        }
    }

    /// The list/create path; the namespace is ignored by cluster-scoped resources. An empty
    /// `namespace` of a namespaced resource gives the path across all namespaces.
    pub fn collection_path(&self, namespace: &str) -> String {
        if self.namespaced && !namespace.is_empty() {
            format!(
                "{}/namespaces/{}/{}",
                self.base(),
                segment(namespace),
                self.plural
            )
        } else {
            format!("{}/{}", self.base(), self.plural)
        }
    }

    /// The get/patch/delete path of one object.
    pub fn object_path(&self, namespace: &str, name: &str) -> String {
        format!("{}/{}", self.collection_path(namespace), segment(name))
    }

    /// The path of a subresource (`log`, `scale`, `status`) of one object.
    pub fn subresource_path(&self, namespace: &str, name: &str, subresource: &str) -> String {
        format!(
            "{}/{}",
            self.object_path(namespace, name),
            segment(subresource)
        )
    }

    pub fn has_verb(&self, verb: &str) -> bool {
        self.verbs.iter().any(|known| known == verb)
    }

    pub fn has_subresource(&self, name: &str) -> bool {
        self.subresources.iter().any(|known| known == name)
    }

    /// How kubectl names it in its lines: `deployment.apps`, `pod`.
    pub fn reference(&self) -> String {
        let singular = if self.singular.is_empty() {
            self.kind.to_lowercase()
        } else {
            self.singular.clone()
        };
        if self.group.is_empty() {
            singular
        } else {
            format!("{singular}.{}", self.group)
        }
    }

    /// `plural.group`, the name kubectl prints in `api-resources`.
    pub fn qualified(&self) -> String {
        if self.group.is_empty() {
            self.plural.clone()
        } else {
            format!("{}.{}", self.plural, self.group)
        }
    }

    fn named(&self, name: &str) -> bool {
        let is = |text: &str| text.eq_ignore_ascii_case(name);
        is(&self.plural)
            || is(&self.singular)
            || is(&self.kind)
            || self.short_names.iter().any(|short| is(short))
    }
}

/// `text` as one path segment: everything but letters, digits and `-._~:` is percent-encoded, so
/// a name cannot add a query, a fragment or another segment.
fn segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~:".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Checks an object name typed or resolved into an `#action` or `#payload`. Servers differ in
/// what they accept (`system:node`, `a.b`), so only what breaks a path is refused.
pub fn check_name(name: &str) -> Result<(), String> {
    let bad = |c: char| c.is_whitespace() || c.is_control() || matches!(c, '/' | '?' | '#' | '%');
    if name.is_empty() || name == "." || name == ".." || name.contains(bad) {
        return Err(format!(
            "Invalid object name {name:?}: it must not be empty, `.` or `..`, nor contain `/`, `?`, `#`, `%` or whitespace"
        ));
    }
    Ok(())
}

/// Checks a namespace: an RFC 1123 label.
pub fn check_namespace(namespace: &str) -> Result<(), String> {
    let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let valid = namespace.len() <= 63
        && edge(namespace.chars().next())
        && edge(namespace.chars().last())
        && namespace
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "Invalid namespace {namespace:?}: use lowercase letters, digits and `-` (at most 63), starting and ending with a letter or digit"
        ))
    }
}

/// The discovery calls of a context, in the order to send them: `/api` (core), then `/apis`.
pub fn requests(context: &str) -> Vec<Request> {
    ["/api", "/apis"]
        .into_iter()
        .map(|path| Request {
            accept: Some(AGGREGATED.into()),
            ..Request::get(context, path)
        })
        .collect()
}

/// A discovery answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Aggregated discovery: the resources of the answer.
    Resources(Vec<Resource>),
    /// A legacy index: the paths to `GET` (no `Accept` needed), read each with [`parse_list`].
    Legacy(Vec<String>),
}

/// Reads the body of `GET /api` or `GET /apis`.
pub fn parse(body: &Value) -> Result<Parsed, String> {
    match body.get("kind").and_then(Value::as_str) {
        Some("APIGroupDiscoveryList") => Ok(Parsed::Resources(parse_aggregated(body))),
        Some("APIVersions") => Ok(Parsed::Legacy(
            strings(&body["versions"])
                .into_iter()
                .map(|version| format!("/api/{version}"))
                .collect(),
        )),
        Some("APIGroupList") => {
            let groups = body["groups"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            let paths = groups
                .iter()
                .filter_map(|group| {
                    let preferred = group["preferredVersion"]["groupVersion"].as_str();
                    let first = group["versions"][0]["groupVersion"].as_str();
                    preferred
                        .or(first)
                        .map(|version| format!("/apis/{version}"))
                })
                .collect();
            Ok(Parsed::Legacy(paths))
        }
        other => Err(format!(
            "Unexpected API discovery answer{}",
            other.map(|kind| format!(" ({kind})")).unwrap_or_default()
        )),
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_aggregated(body: &Value) -> Vec<Resource> {
    let mut found = Vec::new();
    for item in body["items"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let group = item["metadata"]["name"].as_str().unwrap_or_default();
        // Only the preferred (first) version of a group.
        let Some(version) = item["versions"].as_array().and_then(|list| list.first()) else {
            continue;
        };
        let name = version["version"].as_str().unwrap_or_default();
        for entry in version["resources"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let kind = &entry["responseKind"];
            found.push(Resource {
                group: group.into(),
                version: name.into(),
                kind: kind["kind"].as_str().unwrap_or_default().into(),
                plural: entry["resource"].as_str().unwrap_or_default().into(),
                singular: entry["singularResource"]
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                short_names: strings(&entry["shortNames"]),
                namespaced: entry["scope"].as_str() == Some("Namespaced"),
                verbs: strings(&entry["verbs"]),
                subresources: entry["subresources"]
                    .as_array()
                    .map(|list| {
                        list.iter()
                            .filter_map(|sub| sub["subresource"].as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
            });
        }
    }
    found.retain(|resource| !resource.plural.is_empty());
    found
}

/// Reads an `APIResourceList` (`GET /api/v1`, `GET /apis/apps/v1`) of the legacy discovery.
pub fn parse_list(body: &Value) -> Vec<Resource> {
    let group_version = body["groupVersion"].as_str().unwrap_or_default();
    let (group, version) = group_version.split_once('/').unwrap_or(("", group_version));
    let entries = body["resources"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut found: Vec<Resource> = Vec::new();
    for entry in entries {
        let name = entry["name"].as_str().unwrap_or_default();
        if let Some((parent, sub)) = name.split_once('/') {
            if let Some(resource) = found.iter_mut().find(|resource| resource.plural == parent) {
                resource.subresources.push(sub.into());
            }
            continue;
        }
        found.push(Resource {
            group: group.into(),
            version: version.into(),
            kind: entry["kind"].as_str().unwrap_or_default().into(),
            plural: name.into(),
            singular: entry["singularName"].as_str().unwrap_or_default().into(),
            short_names: strings(&entry["shortNames"]),
            namespaced: entry["namespaced"].as_bool().unwrap_or(false),
            verbs: strings(&entry["verbs"]),
            subresources: vec![],
        });
    }
    found.retain(|resource| !resource.plural.is_empty());
    found
}

/// The resource `name` names: `plural`, `singular`, a short name or the `kind` in any case, or
/// `plural.group` / `kind.group` (also `singular.group`). The first match of the catalog wins.
pub fn resolve<'a>(resources: &'a [Resource], name: &str) -> Option<&'a Resource> {
    if let Some(found) = resources.iter().find(|resource| resource.named(name)) {
        return Some(found);
    }
    let (name, group) = name.split_once('.')?;
    resources
        .iter()
        .find(|resource| resource.group.eq_ignore_ascii_case(group) && resource.named(name))
}

/// The resource of an object's `apiVersion` and `kind`, served at that very version (the catalog
/// holds the preferred version of each group; the object may name another it serves).
pub fn by_kind(resources: &[Resource], api_version: &str, kind: &str) -> Option<Resource> {
    let (group, version) = api_version.split_once('/').unwrap_or(("", api_version));
    let found = resources
        .iter()
        .find(|resource| resource.group == group && resource.kind == kind)?;
    Some(Resource {
        version: version.into(),
        ..found.clone()
    })
}
