//! Check identity: titles and ids follow `#check` blocks through edits,
//! response insertion and copies. UI-behaviour tests.

use services_ui::checks::{normalize, remap_text};

#[test]
fn titles_follow_check_comments_without_changing_identity() {
    let mut checks = vec![];
    normalize(
        "#check test remove catalog\n#body\n{}\n#check\n#body\n{}",
        &[],
        &mut checks,
    );
    assert_eq!(checks[0].title, "test remove catalog");
    assert!(checks[1].title.is_empty());
    let id = checks[0].id.clone();
    normalize(
        "#check renamed\n#body\n{}\n#check\n#body\n{}",
        &[],
        &mut checks,
    );
    assert_eq!(checks[0].id, id);
    assert_eq!(checks[0].title, "renamed");
}

#[test]
fn identities_survive_edits_and_response_insertions_but_copies_are_new() {
    let old = "#check same\n#body\n{}\n#check same\n#body\n{}";
    let mut checks = Vec::new();
    normalize(old, &[], &mut checks);
    assert_ne!(checks[0].id, checks[1].id);
    let ids = checks.clone();
    let new = old.replace("{}\n#check", "{\"id\":1}\n#check");
    remap_text(old, &new, &mut checks);
    normalize(&new, &[], &mut checks);
    assert_eq!(checks, ids);
    let copy = format!("{new}\n#check same\n#body\n{{}}");
    remap_text(&new, &copy, &mut checks);
    normalize(&copy, &[], &mut checks);
    assert_eq!(checks[0].id, ids[0].id);
    assert_eq!(checks[1].id, ids[1].id);
    assert_ne!(checks[2].id, ids[0].id);
    assert_ne!(checks[2].id, ids[1].id);
}
