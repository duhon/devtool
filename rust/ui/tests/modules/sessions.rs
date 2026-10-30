//! Sessions panel: layout persistence, copies, temporary session and
//! concurrent merges. UI-behaviour tests.

use serde_json::json;
use services_ui::sessions::{Layout, Library, Snapshot, merge_snapshot};

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
