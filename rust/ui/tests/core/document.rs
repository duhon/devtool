//! Console document editing: response ownership, check identity, clipboard,
//! UTF-16 edits, navigation and legacy markers. UI-behaviour tests.

use services_ui::console;
use services_ui::document::{
    CheckViewport, ClipboardFragment, DocumentEdit, EditHistory, Position, ResponseRegion,
    TextChange, active_check, check_lines, mask_output, migrate_check_markers, remap,
    replace_response, request_at,
};
use services_ui::tables;

#[test]
fn response_touches_request_but_next_check_has_one_empty_line() {
    let source = "#check first\n#query\nselect 1;\n\n\n#check next\n#query\nselect 2;";
    let mut regions = vec![];
    let result = replace_response(source, 0, "OK", "first result", &mut regions);
    assert!(result.contains("select 1;\n#responce OK\nfirst result\n\n#check next"));
    assert!(!result.contains("\n\n#responce"));
    assert_eq!(regions[0].header, 3);
    assert_eq!(regions[0].end, 6);
    let refreshed = replace_response(&result, 0, "OK", "second result", &mut regions);
    assert!(refreshed.contains("select 1;\n#responce OK\nsecond result\n\n#check next"));
    assert!(!refreshed.contains("first result"));
}

#[test]
fn check_identity_survives_rename_cut_move_reload_and_undo_but_not_copy() {
    let mut history = EditHistory::default();
    history.scope = "workspace:rest".into();
    let source = "#check first\n#body\n{}\n#check second\n#body\n{}";
    history.synchronize(source, &[], 0);
    let first = history.checks()[0].id.clone();
    let second = history.checks()[1].id.clone();
    let mut clipboard = None;
    let renamed = edit(
        &mut history,
        (0, 7),
        (0, 12),
        "renamed",
        "+input",
        1,
        &mut clipboard,
    );
    assert_eq!(renamed.checks[0].id, first);
    edit(&mut history, (0, 0), (3, 0), "", "cut", 2, &mut clipboard);
    let fragment = clipboard.as_ref().unwrap().text().to_owned();
    edit(
        &mut history,
        (2, 2),
        (2, 2),
        "\n",
        "+input",
        3,
        &mut clipboard,
    );
    let moved = edit(
        &mut history,
        (3, 0),
        (3, 0),
        &fragment,
        "paste",
        4,
        &mut clipboard,
    );
    assert_eq!(moved.checks[0].id, second);
    assert_eq!(moved.checks[1].id, first);
    let mut reloaded = EditHistory::default();
    reloaded.synchronize(&moved.text, &[], 4);
    reloaded.synchronize_checks(&moved.checks, 4);
    assert_eq!(reloaded.checks(), moved.checks.as_slice());
    let mut copy = Some(
        history
            .clipboard(&Position { line: 0, ch: 0 }, &Position { line: 3, ch: 0 })
            .unwrap(),
    );
    let copied = edit(
        &mut history,
        (6, 0),
        (6, 0),
        "#check second\n#body\n{}\n",
        "paste",
        5,
        &mut copy,
    );
    assert_eq!(copied.checks.len(), 3);
    assert_ne!(copied.checks[2].id, second);
    let undo = edit(&mut history, (6, 0), (9, 0), "", "undo", 4, &mut copy);
    assert_eq!(undo.checks, moved.checks);
}

fn edit(
    history: &mut EditHistory,
    from: (usize, usize),
    to: (usize, usize),
    inserted: &str,
    origin: &str,
    generation: u64,
    clipboard: &mut Option<ClipboardFragment>,
) -> DocumentEdit {
    let from = Position {
        line: from.0,
        ch: from.1,
    };
    let to = Position {
        line: to.0,
        ch: to.1,
    };
    let start = offset(history.text(), &from).unwrap();
    let end = offset(history.text(), &to).unwrap();
    let removed = history.text()[start..end]
        .split('\n')
        .map(str::to_owned)
        .collect();
    let text = format!(
        "{}{}{}",
        &history.text()[..start],
        inserted,
        &history.text()[end..]
    );
    history
        .apply(
            text,
            &[TextChange {
                from,
                to,
                text: inserted.split('\n').map(str::to_owned).collect(),
                removed,
                origin: Some(origin.into()),
            }],
            generation,
            clipboard,
        )
        .unwrap()
}

/// Byte offset of a UTF-16 editor position, as the browser editor reports it.
fn offset(source: &str, position: &Position) -> Result<usize, String> {
    let mut start = 0;
    for (line, text) in source.split('\n').enumerate() {
        if line == position.line {
            let mut utf16 = 0;
            for (byte, ch) in text.char_indices() {
                if utf16 == position.ch {
                    return Ok(start + byte);
                }
                utf16 += ch.len_utf16();
            }
            return if utf16 == position.ch {
                Ok(start + text.len())
            } else {
                Err("Invalid UTF-16 position in Console edit".into())
            };
        }
        start += text.len() + 1;
    }
    Err("Invalid line in Console edit".into())
}
fn editing_fixture() -> EditHistory {
    let mut regions = vec![];
    let source = replace_response(
        "#check first\n#endpoint\nGET https://fixture.invalid/first\n\n#check second\n#endpoint\nGET https://fixture.invalid/second\n\n#check moved\n#endpoint\nGET https://fixture.invalid/moved",
        0,
        "OK",
        "literal output\n#check literal-output",
        &mut regions,
    );
    let mut history = EditHistory::default();
    history.synchronize(&source, &regions, 1);
    history
}

#[test]
fn separate_newlines_preserve_check_identity_and_exact_clipboard_matching() {
    let mut history = editing_fixture();
    let ids = history.checks().to_vec();
    let fragment = history
        .clipboard(
            &Position { line: 0, ch: 0 },
            &Position {
                line: ids[1].line,
                ch: 0,
            },
        )
        .unwrap();
    assert_eq!(
        fragment.pasted_regions(fragment.text()),
        Some(fragment.regions())
    );
    assert!(
        fragment
            .pasted_regions(&format!("\n{}", fragment.text()))
            .is_none()
    );
    let mut clipboard = Some(fragment);
    let line = ids[1].line;
    let separated = edit(
        &mut history,
        (line, 0),
        (line, 0),
        "\n",
        "paste",
        2,
        &mut clipboard,
    );
    assert_eq!(separated.checks.len(), ids.len());
    for (index, before) in ids.iter().enumerate() {
        assert_eq!(separated.checks[index].id, before.id);
        assert_eq!(
            separated.checks[index].line,
            before.line + usize::from(index > 0)
        );
    }
    assert_eq!(separated.regions.len(), 1);
    assert_eq!(
        separated.regions[0].table,
        clipboard.unwrap().regions()[0].table
    );
}

#[test]
fn exact_cut_and_paste_does_not_expand_the_previous_response() {
    let mut history = editing_fixture();
    let original = history.text().to_owned();
    let mut clipboard = None;
    let moved_line = check_lines(history.text(), history.regions())[2];
    let last = history.text().split('\n').count() - 1;
    let last_ch = history
        .text()
        .split('\n')
        .next_back()
        .unwrap()
        .encode_utf16()
        .count();
    let moved = history.text()[offset(
        history.text(),
        &Position {
            line: moved_line,
            ch: 0,
        },
    )
    .unwrap()..]
        .to_owned();
    edit(
        &mut history,
        (moved_line, 0),
        (last, last_ch),
        "",
        "cut",
        2,
        &mut clipboard,
    );
    let boundary = history.regions()[0].end;
    let pasted = edit(
        &mut history,
        (boundary, 0),
        (boundary, 0),
        &format!("{moved}\n\n"),
        "paste",
        3,
        &mut clipboard,
    );
    assert_eq!(check_lines(&pasted.text, &pasted.regions).len(), 3);
    assert_eq!(pasted.regions[0].end, boundary);
    let block = request_at(&pasted.text, boundary, &pasted.regions).unwrap();
    assert_eq!(block.start, boundary);
    assert_eq!(
        console::parse("rest", &block.text).unwrap()["url"],
        "https://fixture.invalid/moved"
    );
    let source = history.text().to_owned();
    let source_last = source.split('\n').count() - 1;
    let source_ch = source
        .split('\n')
        .next_back()
        .unwrap()
        .encode_utf16()
        .count();
    let restored = edit(
        &mut history,
        (0, 0),
        (source_last, source_ch),
        &original,
        "undo",
        1,
        &mut clipboard,
    );
    assert_eq!(restored.text, original);
    assert_eq!(check_lines(&restored.text, &restored.regions).len(), 3);
    assert!(restored.regions[0].end > boundary - 1);
}

#[test]
fn inserting_a_check_inside_output_promotes_only_the_user_inserted_marker() {
    let mut history = editing_fixture();
    let mut clipboard = None;
    let header = history.regions()[0].header;
    let before = history.regions().to_vec();
    let original = history.text().to_owned();
    let inserted = "#check pasted\n#endpoint\nGET https://fixture.invalid/pasted\n\n";
    let pasted = edit(
        &mut history,
        (header + 2, 0),
        (header + 2, 0),
        inserted,
        "paste",
        2,
        &mut clipboard,
    );
    assert_eq!(pasted.regions[0].end, header + 2);
    assert_eq!(check_lines(&pasted.text, &pasted.regions).len(), 4);
    assert_eq!(pasted.regions.len(), 2);
    assert_eq!(
        request_at(&pasted.text, header + 2, &pasted.regions)
            .unwrap()
            .text,
        "#endpoint\nGET https://fixture.invalid/pasted"
    );
    assert_eq!(
        request_at(&pasted.text, header + 2, &pasted.regions)
            .unwrap()
            .start,
        header + 2
    );
    let undone = edit(
        &mut history,
        (header + 2, 0),
        (header + 6, 0),
        "",
        "undo",
        1,
        &mut clipboard,
    );
    assert_eq!(undone.text, original);
    assert_eq!(undone.regions, before);
    let redone = edit(
        &mut history,
        (header + 2, 0),
        (header + 2, 0),
        inserted,
        "redo",
        2,
        &mut clipboard,
    );
    assert_eq!(redone.regions, pasted.regions);
    let mut regions = redone.regions.clone();
    let first_updated = replace_response(&redone.text, 0, "OK", "new first output", &mut regions);
    assert!(!first_updated.contains("#check literal-output"));
    assert_eq!(check_lines(&first_updated, &regions).len(), 4);
    let mut regions = redone.regions;
    let pasted_updated = replace_response(
        &redone.text,
        header + 2,
        "OK",
        "new pasted output",
        &mut regions,
    );
    assert_eq!(regions.len(), 2);
    let response = regions
        .iter()
        .find(|region| region.request == header + 2)
        .unwrap();
    assert_eq!(
        request_at(&pasted_updated, response.header + 1, &regions)
            .unwrap()
            .start,
        header + 2
    );
}

#[test]
fn copying_or_cutting_complete_checks_preserves_literal_output_ownership() {
    for cut in [false, true] {
        let mut history = editing_fixture();
        let mut clipboard = Some(
            history
                .clipboard(
                    &Position { line: 0, ch: 0 },
                    &Position {
                        line: history.regions()[0].end,
                        ch: 0,
                    },
                )
                .unwrap(),
        );
        let fragment = clipboard.as_ref().unwrap().text().to_owned();
        let end = history.regions()[0].end;
        if cut {
            edit(&mut history, (0, 0), (end, 0), "", "cut", 2, &mut clipboard);
        }
        let boundary = check_lines(history.text(), history.regions())[if cut { 1 } else { 2 }];
        let pasted = edit(
            &mut history,
            (boundary, 0),
            (boundary, 0),
            &fragment,
            "paste",
            3,
            &mut clipboard,
        );
        assert_eq!(pasted.regions.len(), if cut { 1 } else { 2 });
        assert_eq!(
            check_lines(&pasted.text, &pasted.regions).len(),
            if cut { 3 } else { 4 }
        );
        let protected = pasted
            .regions
            .iter()
            .find(|r| r.request == boundary)
            .unwrap();
        assert_eq!(
            request_at(&pasted.text, protected.header + 2, &pasted.regions)
                .unwrap()
                .start,
            boundary
        );
    }
}

#[test]
fn edits_use_exact_utf16_positions_and_reject_inconsistent_metadata() {
    let mut history = editing_fixture();
    let mut clipboard = None;
    let result = edit(
        &mut history,
        (0, 6),
        (0, 6),
        "😀 ",
        "+input",
        2,
        &mut clipboard,
    );
    assert!(result.text.starts_with("#check😀  first"));
    let result = edit(&mut history, (0, 8), (0, 9), "", "cut", 3, &mut clipboard);
    assert!(result.text.starts_with("#check😀 first"));
    let previous = history.text().to_owned();
    assert!(
        history
            .apply(
                "invalid".into(),
                &[TextChange {
                    from: Position { line: 0, ch: 7 },
                    to: Position { line: 0, ch: 8 },
                    text: vec!["x".into()],
                    removed: vec!["".into()],
                    origin: Some("paste".into()),
                }],
                4,
                &mut clipboard
            )
            .is_err()
    );
    assert_eq!(history.text(), previous);
}

#[test]
fn check_navigation_follows_document_order_and_excludes_output() {
    let source = "intro\n#check first\n#command\ntrue\n#responce OK\n#check output\n#check second\n#command\ntrue\n#check third";
    let regions = [ResponseRegion {
        request: 1,
        header: 4,
        end: 6,
        table: None,
    }];
    let lines = check_lines(source, &regions);
    assert_eq!(lines, [1, 6, 9]);
    for (top, active) in [(0, 0), (1, 0), (5, 0), (6, 1), (8, 1), (9, 2), (100, 2)] {
        assert_eq!(active_check(&lines, top), Some(active));
    }
    assert_eq!(active_check(&[], 0), None);
    let inserted = source.replace("#check second", "#check inserted\n#check second");
    assert_eq!(check_lines(&inserted, &regions), [1, 6, 7, 10]);
    assert_eq!(
        check_lines(&inserted.replace("#check inserted\n", ""), &regions),
        lines
    );
}

#[test]
fn active_check_follows_visible_cursor_then_viewport() {
    let checks = [0, 5, 10];
    for (top, visible_cursor, active) in [
        (0, Some(7), 1),
        (0, Some(12), 2),
        (5, Some(12), 2),
        (5, None, 1),
        (0, None, 0),
        (0, Some(7), 1),
    ] {
        assert_eq!(
            CheckViewport {
                top,
                visible_cursor
            }
            .active_check(&checks),
            Some(active)
        );
    }
    assert_eq!(CheckViewport::default().active_check(&[]), None);
}

#[test]
fn legacy_headers_migrate_without_rewriting_output_or_line_boundaries() {
    let source = "  #REQUEST first\r\n#endpoint\r\nbash -c\r\n#command\r\necho '#request inline'\r\n#responce OK\r\n#request output\r\n#check output\r\n\r\n#request second\r\n#command\r\necho next\r\n#requestSuffix";
    let regions = vec![ResponseRegion {
        request: 0,
        header: 5,
        end: 9,
        table: None,
    }];
    let expected = "  #check first\r\n#endpoint\r\nbash -c\r\n#command\r\necho '#request inline'\r\n#responce OK\r\n#request output\r\n#check output\r\n\r\n#check second\r\n#command\r\necho next\r\n#requestSuffix";
    let migrated = migrate_check_markers(source, &regions);
    assert_eq!(migrated, expected);
    assert_eq!(migrated.split('\n').count(), source.split('\n').count());
    assert_eq!(migrate_check_markers(&migrated, &regions), migrated);
    assert_eq!(request_at(&migrated, 6, &regions).unwrap().start, 0);
    assert_eq!(request_at(&migrated, 11, &regions).unwrap().start, 9);
    let mut remapped = regions.clone();
    remap(source, &migrated, &mut remapped);
    assert_eq!(remapped, regions);
}

#[test]
fn old_and_new_headers_can_be_pasted_together_without_merging_blocks() {
    let source = "#request old\n#command\nfirst\n#check new\n#command\nsecond\n#request last\n#command\nthird";
    for (line, start, command) in [(2, 0, "first"), (5, 3, "second"), (8, 6, "third")] {
        let block = request_at(source, line, &[]).unwrap();
        assert_eq!(block.start, start);
        assert_eq!(block.text, format!("#command\n{command}"));
        assert_eq!(console::request_at(source, line).unwrap(), block);
    }
    for line in ["#check", " #CHECK name", "#request", "\t#REQUEST name"] {
        assert!(console::is_check(line));
    }
    for line in ["#checker", "#requestSuffix", "echo #check", "\"#request\""] {
        assert!(!console::is_check(line));
    }
}

#[test]
fn literal_markers_are_output_not_requests() {
    let source = "#check\n#endpoint\nbash -c\n#command\necho hello\n\n#check\n#endpoint\nbash -c\n#command\necho next";
    let mut regions = vec![];
    let text = replace_response(
        source,
        0,
        "OK",
        "#check\n#responce fake\n#endpoint\nnot a real request",
        &mut regions,
    );
    let owner = request_at(&text, regions[0].header + 2, &regions).unwrap();
    assert_eq!(owner.start, 0);
    assert!(owner.text.contains("echo hello"));
    let next = request_at(&text, text.lines().count() - 1, &regions).unwrap();
    assert!(next.text.contains("echo next"));
    let replaced = replace_response(&text, 0, "OK", "replacement", &mut regions);
    assert!(!replaced.contains("not a real request"));
    assert!(replaced.contains("echo next"));
    assert_eq!(regions.len(), 1);
}

#[test]
fn output_ranges_follow_edits_and_restore() {
    let mut regions = vec![];
    let text = replace_response("#check\n#command\necho", 0, "OK", "#check", &mut regions);
    regions[0].table = Some(tables::Metadata::from_result(
        &serde_json::json!({"columns":["feed"],"columnTypes":{"feed":"json"}}),
    ));
    let new = format!("\n{text}");
    remap(&text, &new, &mut regions);
    assert_eq!(regions[0].request, 1);
    let json = serde_json::to_string(&regions).unwrap();
    let loaded: Vec<ResponseRegion> = serde_json::from_str(&json).unwrap();
    assert_eq!(loaded[0].table, regions[0].table);
    let mut copied = EditHistory::default();
    copied.synchronize(&new, &loaded, 0);
    let fragment = copied
        .clipboard(
            &Position { line: 1, ch: 0 },
            &Position {
                line: new.split('\n').count() - 1,
                ch: 0,
            },
        )
        .unwrap();
    assert_eq!(fragment.regions()[0].table, loaded[0].table);
    let legacy: ResponseRegion =
        serde_json::from_str(r#"{"request":0,"header":2,"end":4}"#).unwrap();
    assert!(legacy.table.is_none());
    assert_eq!(
        request_at(&new, loaded[0].header + 1, &loaded)
            .unwrap()
            .start,
        1
    );
    assert!(
        !mask_output(&new, &loaded)
            .lines()
            .skip(2)
            .any(|l| l == "#check")
    );
}

#[test]
fn invalid_restored_ranges_never_hide_or_slice_requests() {
    let source = "#check\n#command\necho";
    for region in [
        ResponseRegion {
            request: 0,
            header: 999,
            end: 1000,
            table: None,
        },
        ResponseRegion {
            request: 0,
            header: 0,
            end: 2,
            table: None,
        },
        ResponseRegion {
            request: 0,
            header: 1,
            end: 3,
            table: None,
        },
    ] {
        let block = request_at(source, 0, &[region]).unwrap();
        assert_eq!(block.text, "#command\necho");
    }
}

#[test]
fn other_responses_follow_stream_updates_and_request_edits() {
    let mut regions = vec![];
    let source = "#check\n#command\nfirst\n\n#check\n#command\nsecond";
    let text = replace_response(source, 4, "OK", "second output\n#check", &mut regions);
    let text = replace_response(&text, 0, "Running", "first output", &mut regions);
    let next = request_at(&text, text.lines().count() - 1, &regions).unwrap();
    assert!(next.text.contains("second"));
    let text = replace_response(
        &text,
        0,
        "OK",
        "first output\n#check\n#endpoint\nliteral",
        &mut regions,
    );
    assert_eq!(regions.len(), 2);
    let second = regions.iter().find(|r| r.request > 0).unwrap().request;
    assert!(
        request_at(&text, second, &regions)
            .unwrap()
            .text
            .contains("second")
    );
    let edited = text.replacen("first\n", "first\nanother command\n", 1);
    remap(&text, &edited, &mut regions);
    assert_eq!(regions.len(), 2);
    assert!(
        request_at(&edited, regions[1].header + 1, &regions)
            .unwrap()
            .text
            .contains("second")
    );
}
