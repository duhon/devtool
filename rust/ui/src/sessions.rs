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
    pub legacy: BTreeMap<String, crate::storage::State>,
    pub variables: Value,
    pub variable_drafts: BTreeMap<String, String>,
    pub hints: BTreeMap<String, String>,
    pub hints_drafts: BTreeMap<String, String>,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            layout: Layout::default(),
            console: json!({"version":1,"service":"rest","documents":{},"responses":{}}),
            legacy: BTreeMap::new(),
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout_is_session_scoped_copied_persisted_and_backward_compatible() {
        let mut library = Library::default();
        let a = library.copy(0, "A").unwrap();
        library.select(a).unwrap();
        library.snapshot_mut().layout.check_menu_width = 88;
        library
            .snapshot_mut()
            .layout
            .panel_widths
            .insert("history".into(), 410);
        let b = library.copy(a, "B").unwrap();
        assert_eq!(
            library.get(a).unwrap().layout,
            library.get(b).unwrap().layout
        );
        library.select(b).unwrap();
        library
            .snapshot_mut()
            .layout
            .panel_widths
            .insert("history".into(), 610);
        let restored = Library::restore(&serde_json::to_string(&library).unwrap()).unwrap();
        assert_eq!(restored.get(a).unwrap().layout.panel_widths["history"], 410);
        assert_eq!(restored.get(b).unwrap().layout.panel_widths["history"], 610);
        assert_eq!(restored.snapshot().layout, Layout::default());
        let mut legacy = serde_json::to_value(&library).unwrap();
        legacy["sessions"][0]["snapshot"]
            .as_object_mut()
            .unwrap()
            .remove("layout");
        assert_eq!(
            Library::restore(&legacy.to_string())
                .unwrap()
                .get(a)
                .unwrap()
                .layout,
            Layout::default()
        );
        legacy["sessions"][1]["snapshot"]["layout"]["check_menu_width"] = json!(0);
        assert!(Library::restore(&legacy.to_string()).is_err());
    }

    #[test]
    fn concurrent_layout_changes_merge_independently_of_documents_and_other_panels() {
        let base = Snapshot::default();
        let mut local = base.clone();
        local.layout.panel_widths.insert("history".into(), 410);
        let mut remote = base.clone();
        remote.layout.panel_widths.insert("variables".into(), 520);
        remote.console["documents"]["rest"] = json!("#check remote");
        let merged = merge_snapshot(&base, &local, &remote).unwrap();
        assert_eq!(merged.layout.panel_widths["history"], 410);
        assert_eq!(merged.layout.panel_widths["variables"], 520);
        assert_eq!(merged.console, remote.console);
    }
    #[test]
    fn session_copies_do_not_inherit_execution_history_identities() {
        let mut library = Library::default();
        let original = library.copy(0, "Original").unwrap();
        library.select(original).unwrap();
        library.snapshot_mut().console["workspaceId"] = json!("original-workspace");
        library.snapshot_mut().console["checkIdentities"] = json!({"rest":[{"id":"old-check"}]});
        library.snapshot_mut().console["documents"] =
            json!({"rest":"#check copied\n#endpoint\nGET /books"});
        let copy = library.copy(original, "Copy").unwrap();
        let snapshot = library.get(copy).unwrap();
        assert!(snapshot.console.get("workspaceId").is_none());
        assert!(snapshot.console.get("checkIdentities").is_none());
        assert_eq!(
            snapshot.console["documents"]["rest"],
            "#check copied\n#endpoint\nGET /books"
        );
        assert_eq!(
            library.get(original).unwrap().console["workspaceId"],
            "original-workspace"
        );
    }
    #[test]
    fn concurrent_changes_merge_only_when_their_fields_do_not_conflict() {
        let base = Snapshot::default();
        let mut local = base.clone();
        local.hints.insert("rest".into(), "local".into());
        let mut remote = base.clone();
        remote.console["documents"]["grpc"] = json!("#check remote");
        let merged = merge_snapshot(&base, &local, &remote).unwrap();
        assert_eq!(merged.hints["rest"], "local");
        assert_eq!(merged.console["documents"]["grpc"], "#check remote");
        remote.hints.insert("rest".into(), "remote".into());
        assert!(
            merge_snapshot(&base, &local, &remote)
                .unwrap_err()
                .contains("hints.rest")
        );
        assert_eq!(merge_snapshot(&base, &local, &local).unwrap(), local);
    }
    #[test]
    fn temporary_starts_empty_and_is_never_a_copy_template() {
        let mut library = Library::default();
        library.temporary.variables = json!({"secret":"temporary"});
        let id = library.copy(0, "Empty").unwrap();
        assert_eq!(library.get(id), Some(&Snapshot::default()));
        library.select(id).unwrap();
        let restored = Library::restore(&serde_json::to_string(&library).unwrap()).unwrap();
        assert_eq!(restored.current, 0);
        assert_eq!(restored.snapshot(), &Snapshot::default());
        assert!(library.delete(0).is_err());
    }
    #[test]
    fn copies_are_independent_and_include_checks_variables_hints_and_drafts() {
        let mut library = Library::default();
        let a = library.copy(0, "A").unwrap();
        library.select(a).unwrap();
        library.snapshot_mut().console["documents"] = json!({"rest":"#check A"});
        library.snapshot_mut().variables = json!({"token":"A"});
        library
            .snapshot_mut()
            .hints
            .insert("rest".into(), "A".into());
        library
            .snapshot_mut()
            .variable_drafts
            .insert("var".into(), "draft".into());
        let b = library.copy(a, "B").unwrap();
        assert_eq!(library.get(a), library.get(b));
        library.select(b).unwrap();
        library.snapshot_mut().variables["token"] = json!("B");
        assert_eq!(library.get(a).unwrap().variables["token"], "A");
        library.delete(b).unwrap();
        assert_eq!(library.current, 0);
        assert!(library.select(b).is_err());
        assert!(library.copy(a, " ").is_err());
        assert!(Library::restore(r#"{"version":2}"#).is_err());
    }
}
