//! Editor find bar: the RegExp a query compiles to, navigation with wrap,
//! response output kept out of replacement, and replacement templates.

use services_ui::document::ResponseRegion;
use services_ui::find::{
    Lines, Match, Options, counter, editable, escape, expand, following, pattern, preceding,
};
use std::collections::BTreeMap;

#[test]
fn queries_compile_to_literal_case_and_whole_word_patterns() {
    assert_eq!(pattern("", Options::default()), None);
    assert_eq!(
        pattern("a.b(c)", Options::default()),
        Some((r"a\.b\(c\)".into(), "gim".into()))
    );
    assert_eq!(
        escape(r"\^$.*+?()[]{}|/"),
        r"\\\^\$\.\*\+\?\(\)\[\]\{\}\|\/"
    );
    let (source, flags) = pattern(
        "id|name",
        Options {
            case: true,
            word: true,
            regex: true,
        },
    )
    .unwrap();
    assert_eq!(flags, "gm");
    assert!(source.starts_with("(?<![0-9A-Za-z_"));
    assert!(source.contains("(?:id|name)"));
}

#[test]
fn line_offsets_count_utf16_units() {
    let lines = Lines::new("ab\nпривет 😀x\n\nend");
    assert_eq!(lines.position(0), (0, 0));
    assert_eq!(lines.position(3), (1, 0));
    // The emoji is a surrogate pair: "x" sits at ch 9, not 8.
    assert_eq!(lines.position(3 + 9), (1, 9));
    assert_eq!(lines.offset(2, 0), 3 + 11);
    assert_eq!(lines.position(3 + 12), (3, 0));
    assert_eq!(lines.start(10), 3 + 12 + 3);
}

#[test]
fn navigation_wraps_around_the_document() {
    let matches = [Match { start: 2, end: 4 }, Match { start: 10, end: 12 }];
    assert_eq!(following(&matches, 0), Some(0));
    assert_eq!(following(&matches, 3), Some(1));
    assert_eq!(following(&matches, 11), Some(0));
    assert_eq!(preceding(&matches, 10), Some(0));
    assert_eq!(preceding(&matches, 2), Some(1));
    assert_eq!(following(&[], 0), None);
    assert_eq!(preceding(&[], 0), None);
}

#[test]
fn matches_inside_response_output_are_not_replaceable() {
    let text = "#check a\nGET /x\n#response 200\nGET /x\n\n#check b\nGET /x";
    let lines = Lines::new(text);
    let regions = [ResponseRegion {
        request: 0,
        header: 2,
        end: 4,
        columns: Vec::new(),
    }];
    let at = |line, ch, len| {
        let start = lines.offset(line, ch);
        Match {
            start,
            end: start + len,
        }
    };
    assert!(editable(&lines, &at(1, 0, 3), &regions));
    assert!(!editable(&lines, &at(3, 0, 3), &regions));
    assert!(!editable(&lines, &at(2, 1, 8), &regions));
    assert!(editable(&lines, &at(6, 0, 3), &regions));
    // A match ending with the line break of the line above a response stays outside.
    assert!(editable(&lines, &at(1, 0, 7), &regions));
    assert!(!editable(&lines, &at(1, 0, 8), &regions));
}

#[test]
fn replacement_templates_follow_javascript_replace() {
    let groups = [
        Some("key=value".to_owned()),
        Some("key".to_owned()),
        None,
        Some("value".to_owned()),
    ];
    assert_eq!(expand("$3=$1", &groups, None), "value=key");
    assert_eq!(
        expand("[$&] $$1 $2 $0 $9", &groups, None),
        "[key=value] $1  $0 $9"
    );
    assert_eq!(expand("$03$1x", &groups, None), "valuekeyx");
    assert_eq!(expand(r"a\nb\tc\\n", &groups, None), "a\nb\tc\\n");
    assert_eq!(expand("$<name>", &groups, None), "$<name>");
    let named = BTreeMap::from([
        ("name".to_owned(), Some("key".to_owned())),
        ("other".to_owned(), None),
    ]);
    assert_eq!(
        expand("<$<name>|$<other>|$<missing>|$<open", &groups, Some(&named)),
        "<key|||$<open"
    );
}

#[test]
fn the_counter_reads_position_total_and_cap() {
    assert_eq!(counter(None, 0, false), "No results");
    assert_eq!(counter(Some(2), 120, false), "3 of 120");
    assert_eq!(counter(Some(0), 10_000, true), "1 of 10000+");
    assert_eq!(counter(None, 4, false), "4 found");
}
