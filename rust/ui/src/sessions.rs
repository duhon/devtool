use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Layout {
    pub check_menu_width: u32,
    pub panel_widths: BTreeMap<String, u32>,
}
impl Default for Layout {
    fn default() -> Self {
        Self {
            check_menu_width: 28,
            panel_widths: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Snapshot {
    pub layout: Layout,
    pub console: Value,
    pub variables: Value,
    pub variable_drafts: BTreeMap<String, String>,
    pub hints: BTreeMap<String, String>,
    pub hints_drafts: BTreeMap<String, String>,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            layout: Layout::default(),
            console: json!({"version":1,"service":crate::services::default_service(),"documents":{},"responses":{}}),
            variables: json!({}),
            variable_drafts: BTreeMap::new(),
            hints: BTreeMap::new(),
            hints_drafts: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub id: u64,
    pub name: String,
    pub snapshot: Snapshot,
}
/// Drops everything the snapshot keeps for `service`: its console with checks and
/// responses, its tab position and its hints. Used when a module is removed with its data.
pub fn forget_service(snapshot: &mut Snapshot, service: &str) {
    for key in ["documents", "responses", "checkIdentities"] {
        if let Some(map) = snapshot.console.get_mut(key).and_then(Value::as_object_mut) {
            map.remove(service);
        }
    }
    if let Some(order) = snapshot
        .console
        .get_mut("order")
        .and_then(Value::as_array_mut)
    {
        order.retain(|id| id != service);
    }
    snapshot.hints.remove(service);
    snapshot.hints_drafts.remove(service);
}
pub fn merge_snapshot(
    base: &Snapshot,
    local: &Snapshot,
    remote: &Snapshot,
) -> Result<Snapshot, String> {
    fn merge(
        base: Option<&Value>,
        local: Option<&Value>,
        remote: Option<&Value>,
        path: &str,
    ) -> Result<Option<Value>, String> {
        if local == base {
            return Ok(remote.cloned());
        }
        if remote == base || remote == local {
            return Ok(local.cloned());
        }
        if let (Some(Value::Object(local)), Some(Value::Object(remote))) = (local, remote)
            && (base.is_none() || base.is_some_and(Value::is_object))
        {
            let empty = serde_json::Map::new();
            let base = base.and_then(Value::as_object).unwrap_or(&empty);
            let keys: std::collections::BTreeSet<_> = base
                .keys()
                .chain(local.keys())
                .chain(remote.keys())
                .collect();
            let mut out = serde_json::Map::new();
            for key in keys {
                if let Some(value) = merge(
                    base.get(key),
                    local.get(key),
                    remote.get(key),
                    &format!("{path}.{key}"),
                )? {
                    out.insert(key.clone(), value);
                }
            }
            Ok(Some(Value::Object(out)))
        } else {
            Err(format!(
                "Session {path} changed in another window. Saved data was not overwritten; switch away and back to reload before editing it."
            ))
        }
    }
    let base = serde_json::to_value(base).map_err(|e| e.to_string())?;
    let local = serde_json::to_value(local).map_err(|e| e.to_string())?;
    let remote = serde_json::to_value(remote).map_err(|e| e.to_string())?;
    serde_json::from_value(
        merge(Some(&base), Some(&local), Some(&remote), "workspace")?
            .ok_or("Session data disappeared")?,
    )
    .map_err(|e| e.to_string())
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Library {
    pub version: u32,
    pub revision: u64,
    pub next_id: u64,
    pub sessions: Vec<Session>,
    #[serde(skip)]
    pub current: u64,
    #[serde(skip)]
    pub temporary: Snapshot,
}
impl Default for Library {
    fn default() -> Self {
        Self {
            version: 1,
            revision: 0,
            next_id: 1,
            sessions: Vec::new(),
            current: 0,
            temporary: Snapshot::default(),
        }
    }
}
impl Library {
    pub fn restore(raw: &str) -> Result<Self, String> {
        let value: Self =
            serde_json::from_str(raw).map_err(|e| format!("Invalid sessions: {e}"))?;
        if value.version != 1
            || value.next_id == 0
            || value.sessions.iter().any(|s| {
                s.id == 0
                    || !(28..=320).contains(&s.snapshot.layout.check_menu_width)
                    || s.snapshot
                        .layout
                        .panel_widths
                        .values()
                        .any(|width| !(1..=16_384).contains(width))
                    || s.id >= value.next_id
                    || s.name.trim().is_empty()
                    || !s.snapshot.variables.is_object()
                    || s.snapshot.console["version"] != 1
                    || !s.snapshot.console["documents"]
                        .as_object()
                        .is_some_and(|documents| documents.values().all(Value::is_string))
                    || !s.snapshot.console["responses"]
                        .as_object()
                        .is_some_and(|responses| {
                            responses.values().all(|regions| {
                                serde_json::from_value::<Vec<crate::document::ResponseRegion>>(
                                    regions.clone(),
                                )
                                .is_ok()
                            })
                        })
                    || value
                        .sessions
                        .iter()
                        .filter(|other| other.id == s.id)
                        .count()
                        != 1
            })
        {
            return Err(
                "Invalid or unsupported session library; saved data was not overwritten".into(),
            );
        }
        Ok(value)
    }
    pub fn snapshot(&self) -> &Snapshot {
        self.get(self.current).expect("selected session exists")
    }
    pub fn snapshot_mut(&mut self) -> &mut Snapshot {
        if self.current == 0 {
            &mut self.temporary
        } else {
            &mut self
                .sessions
                .iter_mut()
                .find(|s| s.id == self.current)
                .expect("selected session exists")
                .snapshot
        }
    }
    pub fn get(&self, id: u64) -> Option<&Snapshot> {
        if id == 0 {
            Some(&self.temporary)
        } else {
            self.sessions
                .iter()
                .find(|s| s.id == id)
                .map(|s| &s.snapshot)
        }
    }
    pub fn name(&self) -> &str {
        self.sessions
            .iter()
            .find(|s| s.id == self.current)
            .map(|s| s.name.as_str())
            .unwrap_or("Temporary")
    }
    pub fn copy(&mut self, source: u64, name: &str) -> Result<u64, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Session name cannot be empty".into());
        }
        let mut snapshot = if source == 0 {
            Snapshot::default()
        } else {
            self.get(source).ok_or("Session no longer exists")?.clone()
        };
        if let Some(console) = snapshot.console.as_object_mut() {
            console.remove("checkIdentities");
            console.remove("workspaceId");
        }
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or("Session numbering exhausted")?;
        self.sessions.push(Session {
            id,
            name: name.into(),
            snapshot,
        });
        Ok(id)
    }
    pub fn select(&mut self, id: u64) -> Result<(), String> {
        self.get(id).ok_or("Session no longer exists")?;
        self.current = id;
        Ok(())
    }
    pub fn delete(&mut self, id: u64) -> Result<(), String> {
        if id == 0 {
            return Err("The temporary session cannot be deleted".into());
        }
        self.get(id).ok_or("Session no longer exists")?;
        self.sessions.retain(|s| s.id != id);
        if self.current == id {
            self.current = 0;
        }
        Ok(())
    }
}
