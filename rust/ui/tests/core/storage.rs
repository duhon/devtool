//! Persisted Console tab order: restore, deduplication and reordering; consoles of
//! modules that aren't loaded.
//! UI-behaviour tests.

use serde_json::json;
use services_ui::storage::{console_order, keep_unloaded, reorder};

#[test]
fn console_order_restores_deduplicates_and_completes() {
    let mut order = console_order(&serde_json::json!(["terminal", "rest", "rest", "unknown"]));
    assert_eq!(order.len(), 7);
    assert_eq!(&order[..2], ["terminal", "rest"]);
    reorder(&mut order, "rest", "terminal");
    assert_eq!(&order[..2], ["rest", "terminal"]);
    let saved = serde_json::to_value(&order).unwrap();
    assert_eq!(console_order(&saved), order);
}

#[test]
fn a_save_keeps_the_console_of_a_module_that_is_not_loaded() {
    let saved = json!({"documents":{"rest":"#check\nold","redis":"#check\nPING"},
        "responses":{"redis":[{"start":1}]},"checkIdentities":{"redis":["a"]},
        "order":["redis","rest","terminal"]});
    let mut next = json!({"documents":{"rest":"#check\nnew"},"responses":{},
        "checkIdentities":{},"order":["rest","terminal"]});
    keep_unloaded(&saved, &mut next);
    assert_eq!(
        next,
        json!({"documents":{"rest":"#check\nnew","redis":"#check\nPING"},
            "responses":{"redis":[{"start":1}]},"checkIdentities":{"redis":["a"]},
            "order":["rest","terminal","redis"]})
    );
    let mut restored = console_order(&next["order"]);
    restored.truncate(2);
    assert_eq!(restored, ["rest", "terminal"]);
}
