//! Agent panel workspace: check ids, revisions, edits and paging of the
//! Console the agent works on. UI-behaviour tests.

use serde_json::json;
use services_ui::agent_workspace::{Workspace, page};
use services_ui::document::{self, Position, ResponseRegion};

#[test]
fn ids_follow_unique_moved_checks_and_stale_writes_fail() {
    let mut workspace = Workspace::default();
    workspace.refresh("mysql", "#check A\nselect 1\n#check B\nselect 2", &[]);
    let id = workspace.context("mysql", 0).unwrap()["checks"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    workspace.refresh("mysql", "#check B\nselect 2\n#check A\nselect 1", &[]);
    assert_eq!(workspace.check_line("mysql", &id).unwrap(), 2);
    assert!(workspace.validate("mysql", "1").is_err());
    assert!(workspace.validate("mysql", "2").is_ok());
}

#[test]
fn reloading_a_workspace_invalidates_old_ids_and_revisions() {
    let mut first = Workspace::new(Ok("instance-a".into()));
    let mut second = Workspace::new(Ok("instance-b".into()));
    for workspace in [&mut first, &mut second] {
        workspace.refresh("mysql", "#check A\nselect 1", &[]);
    }
    let old = first.context("mysql", 0).unwrap();
    assert!(
        second
            .validate("mysql", old["revision"].as_str().unwrap())
            .is_err()
    );
    assert!(
        second
            .check_line("mysql", old["activeCheck"].as_str().unwrap())
            .is_err()
    );
    let mut unavailable = Workspace::new(Err("identity unavailable".into()));
    unavailable.refresh("mysql", "#check A\nselect 1", &[]);
    assert!(unavailable.context("mysql", 0).is_err());
}

#[test]
fn edits_preserve_connections_outputs_and_use_utf16_positions() {
    let mut workspace = Workspace::default();
    let source = "#check A\n#endpoint\nmysql://u:p@host/db\n\nselect 1\n#response\nok\n#check B\nselect '😀'";
    workspace.refresh(
        "mysql",
        source,
        &[ResponseRegion {
            request: 0,
            header: 5,
            end: 7,
            table: None,
        }],
    );
    let id = workspace.context("mysql", 0).unwrap()["checks"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let edit = workspace
        .edit(
            "mysql",
            &json!({"revision":"1","checkId":id,"request":"#query\nselect 2"}),
            1,
            false,
        )
        .unwrap();
    assert_eq!(edit.from, Position { line: 1, ch: 0 });
    assert_eq!(edit.to, Position { line: 5, ch: 0 });
    assert!(edit.text.starts_with("#endpoint\nmysql://"));
    assert!(edit.text.ends_with("\n\n#query\nselect 2\n"));
    let create = workspace
        .edit(
            "mysql",
            &json!({"revision":"1","checkId":id,"title":"C","request":"#query\nselect 3"}),
            2,
            true,
        )
        .unwrap();
    assert_eq!(create.from.ch, "select '😀'".encode_utf16().count());
    assert!(
        workspace
            .edit(
                "mysql",
                &json!({"revision":"1","checkId":id,"request":"[redacted]"}),
                1,
                false
            )
            .is_err()
    );
}

#[test]
fn titles_are_plain_names_and_prefixed_inputs_are_rejected() {
    let mut workspace = Workspace::default();
    workspace.refresh(
        "rest",
        "#check Products\n#endpoint\nGET https://example.invalid/products",
        &[],
    );
    let context = workspace.context("rest", 0).unwrap();
    assert_eq!(context["checks"][0]["title"], "Products");
    let id = context["activeCheck"].as_str().unwrap();
    for title in [
        "#check Products",
        "#CHECK Products",
        "#request Products",
        "#check",
        " \t#check Products",
    ] {
        let args =
            json!({"revision":"1","checkId":id,"title":title,"request":"#headers\nX-Test: value"});
        for create in [false, true] {
            assert!(
                workspace
                    .edit("rest", &args, 1, create)
                    .err()
                    .unwrap()
                    .contains("plain check name")
            );
        }
    }
    for payload in [
        json!({}),
        json!({"title":false}),
        json!({"request":false}),
        json!({"title":""}),
        json!({"title":"bad\ntitle"}),
        json!({"title":"я".repeat(101)}),
    ] {
        let mut args = payload;
        args["revision"] = json!("1");
        args["checkId"] = json!(id);
        assert!(workspace.edit("rest", &args, 1, false).is_err());
    }
    let create=workspace.edit("rest",&json!({"revision":"1","checkId":id,"title":"products overview","request":"#headers\nX-Test: value"}),1,true).unwrap();
    assert!(
        create
            .text
            .starts_with("\n\n#check products overview\n#endpoint\n")
    );
    assert!(!create.text.contains("#check #check"));
    let hashtag_title = workspace
        .edit(
            "rest",
            &json!({"revision":"1","checkId":id,"title":"#checklist review","request":"#headers\nX-Test: value"}),
            1,
            true,
        )
        .unwrap();
    assert!(hashtag_title.text.contains("#check #checklist review\n"));
    workspace.refresh("rest", "#request Legacy\ninvalid request", &[]);
    assert_eq!(
        workspace.context("rest", 0).unwrap()["checks"][0]["title"],
        "Legacy"
    );
    let context = workspace.context("rest", 0).unwrap();
    assert!(workspace.edit("rest",&json!({"revision":context["revision"],"checkId":context["activeCheck"],"title":"Renamed invalid draft"}),2,false).is_ok());
}

#[test]
fn renaming_preserves_body_response_ownership_and_check_identity() {
    let mut workspace = Workspace::default();
    let old_header = "#check #check products 😀";
    let source = format!(
        "{old_header}\n#endpoint\nGET https://example.invalid/products\n#body\n{{\"id\":123}}\n#response\ncaptured response\n#check Other\nunfinished draft"
    );
    let regions = vec![ResponseRegion {
        request: 0,
        header: 5,
        end: 7,
        table: None,
    }];
    workspace.refresh("rest", &source, &regions);
    let context = workspace.context("rest", 0).unwrap();
    let id = context["activeCheck"].as_str().unwrap().to_owned();
    let edit = workspace
        .edit(
            "rest",
            &json!({"revision":"1","checkId":id,"title":"products overview"}),
            1,
            false,
        )
        .unwrap();
    assert_eq!(edit.from, Position { line: 0, ch: 0 });
    assert_eq!(
        edit.to,
        Position {
            line: 0,
            ch: old_header.encode_utf16().count()
        }
    );
    assert_eq!(edit.text, "#check products overview");
    let renamed = source.replacen(old_header, &edit.text, 1);
    let mut history = document::EditHistory::default();
    history.synchronize(&source, &regions, 1);
    let changed = history
        .apply(
            renamed,
            &[document::TextChange {
                from: edit.from,
                to: edit.to,
                text: vec![edit.text],
                removed: vec![old_header.into()],
                origin: Some("+services-agent".into()),
            }],
            2,
            &mut None,
        )
        .unwrap();
    assert_eq!(changed.regions, regions);
    assert_eq!(
        changed.text.split_once('\n').unwrap().1,
        source.split_once('\n').unwrap().1
    );
    workspace.refresh("rest", &changed.text, &changed.regions);
    workspace.retain_edited_id("rest", &id, 0);
    assert_eq!(
        workspace.context("rest", 0).unwrap()["checks"][0]["title"],
        "products overview"
    );
    assert_eq!(workspace.check_line("rest", &id).unwrap(), 0);
    assert_eq!(
        workspace.response("rest", &id, 0, 16000).unwrap()["text"],
        "#response\ncaptured response"
    );
    assert!(
        workspace
            .edit(
                "rest",
                &json!({"revision":"1","checkId":id,"title":"Stale rename"}),
                2,
                false
            )
            .err()
            .unwrap()
            .contains("CONFLICT")
    );
}

#[test]
fn combined_title_and_request_edits_preserve_connection_and_response_boundary() {
    let mut workspace = Workspace::default();
    let source = "#check Products\n#endpoint\nGET https://example.invalid/products\n#headers\nX-Test: old\n#response\ncaptured";
    workspace.refresh(
        "rest",
        source,
        &[ResponseRegion {
            request: 0,
            header: 5,
            end: 7,
            table: None,
        }],
    );
    let context = workspace.context("rest", 0).unwrap();
    let edit=workspace.edit("rest",&json!({"revision":"1","checkId":context["activeCheck"],"title":"Updated products","request":"#headers\nX-Test: new"}),1,false).unwrap();
    assert_eq!(edit.from, Position { line: 0, ch: 0 });
    assert_eq!(edit.to, Position { line: 5, ch: 0 });
    assert_eq!(
        edit.text,
        "#check Updated products\n#endpoint\nGET https://example.invalid/products\n\n#headers\nX-Test: new\n"
    );
}

#[test]
fn unicode_pages_and_response_ownership_are_bounded() {
    assert_eq!(
        page("a😀б", 1, 1),
        json!({"text":"😀","offset":1,"total":3,"nextOffset":2})
    );
    let mut workspace = Workspace::default();
    workspace.refresh(
        "rest",
        "#check A\nGET /\n#response\n#check not-input\nok",
        &[ResponseRegion {
            request: 0,
            header: 2,
            end: 5,
            table: None,
        }],
    );
    let context = workspace.context("rest", 4).unwrap();
    assert_eq!(context["checks"].as_array().unwrap().len(), 1);
    let id = context["activeCheck"].as_str().unwrap();
    assert_eq!(
        workspace.response("rest", id, 0, 16000).unwrap()["text"],
        "#response\n#check not-input\nok"
    );
}
