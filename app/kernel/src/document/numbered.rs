//! Checks as variables: `{{.checks.<service>.<n>.<part>}}` reads part of the `n`th check (from 1)
//! of a console document: a section by name, `title` or `response`. `&n` reads the same check
//! and the editor keeps it pointing there when checks above it come and go ([`renumber`]);
//! `.checks.this` is the document being edited, with `prev`, the check above the current one.
use super::{ResponseRegion, check_lines, is_check, request_at, valid_regions};
use serde_json::{Map, Value};

/// The most of a response a check variable holds.
const RESPONSE_LIMIT: usize = 64 * 1024;

/// The checks of a document, `{"1": {title, response, <section>: text}, …}`, in one pass.
pub fn tree(source: &str, regions: &[ResponseRegion]) -> Map<String, Value> {
    let lines: Vec<&str> = source.split('\n').collect();
    let regions = valid_regions(&lines, regions);
    let output = |line: usize| regions.iter().any(|r| line >= r.header && line < r.end);
    let starts: Vec<usize> = (0..lines.len())
        .filter(|&at| !output(at) && is_check(lines[at]))
        .collect();
    let mut checks = Map::new();
    for (index, &start) in starts.iter().enumerate() {
        let next = starts.get(index + 1).copied().unwrap_or(lines.len());
        let own = regions
            .iter()
            .filter(|region| region.request == start)
            .min_by_key(|region| region.header);
        let end = own.map_or_else(
            || {
                (start + 1..next)
                    .find(|&at| {
                        super::marker(lines[at])
                            .is_some_and(|(name, _)| name == "responce" || name == "response")
                    })
                    .unwrap_or(next)
            },
            |region| region.header,
        );
        let mut entry = Map::new();
        // The title, without a template call after it.
        let title = lines[start]
            .trim_start()
            .split_once(char::is_whitespace)
            .map_or("", |(_, title)| title.trim());
        let title = crate::editor::templates::call_start(title)
            .filter(|&at| services_contracts::templates::parse_section_call(&title[at..]).is_ok())
            .map_or(title, |at| title[..at].trim_end());
        entry.insert("number".into(), Value::from(index + 1));
        entry.insert("title".into(), Value::String(title.into()));
        let body = lines[start + 1..end.max(start + 1)].join("\n");
        for section in super::check("", &body, &Value::Null).sections {
            if section.name != "leftover" && !entry.contains_key(&section.name) {
                entry.insert(section.name, Value::String(section.text));
            }
        }
        let mut response = own
            .map(|region| lines[region.header + 1..region.end].join("\n"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        if response.len() > RESPONSE_LIMIT {
            let mut end = RESPONSE_LIMIT;
            while !response.is_char_boundary(end) {
                end -= 1;
            }
            response.truncate(end);
        }
        entry.insert("response".into(), Value::String(response));
        checks.insert((index + 1).to_string(), Value::Object(entry));
    }
    checks
}

/// The number (from 1) of the check `line` belongs to.
pub fn number_at(source: &str, regions: &[ResponseRegion], line: usize) -> Option<usize> {
    let block = request_at(source, line, regions)?;
    check_lines(source, regions)
        .iter()
        .position(|&at| at == block.start)
        .map(|index| index + 1)
}

/// The text of check `number` of `service`'s document as a run reads it: a run keeps no
/// cursor, so `.checks.this` becomes `.checks.<service>` and `prev` the check above it.
pub fn with_prev(text: &str, service: &str, number: usize) -> String {
    if !text.contains("checks.") {
        return text.to_owned();
    }
    let text = replace_key(text, "checks", |key| {
        (key == "this").then(|| service.to_owned())
    });
    if number < 2 {
        return text;
    }
    let previous = (number - 1).to_string();
    replace_key(&text, &format!("checks.{service}"), |key| {
        (key == "prev").then(|| previous.clone())
    })
}

/// `text` with each key right after `prefix.` (an exact path segment) replaced by `rename`'s
/// answer; `None` keeps it.
fn replace_key(text: &str, prefix: &str, rename: impl Fn(&str) -> Option<String>) -> String {
    let needle = format!("{prefix}.");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(&needle) {
        let starts = at == 0 || rest[..at].ends_with(['.', ':']);
        let (head, tail) = rest.split_at(at + needle.len());
        out.push_str(head);
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '&')))
            .unwrap_or(tail.len());
        match starts.then(|| rename(&tail[..end])).flatten() {
            Some(key) => out.push_str(&key),
            None => out.push_str(&tail[..end]),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// `text` with its `&n` references to the checks of `service` following them from the
/// order `before` to `after` (check ids, first check first); `this` is `text`'s own document.
/// A reference to a check that is gone becomes `&gone`. `None` when nothing changes.
pub fn renumber(
    text: &str,
    service: &str,
    this: bool,
    before: &[String],
    after: &[String],
) -> Option<String> {
    if before == after || !text.contains(".&") {
        return None;
    }
    let rename = |key: &str| {
        let number: usize = key.strip_prefix('&')?.parse().ok()?;
        let id = before.get(number.checked_sub(1)?)?;
        Some(match after.iter().position(|other| other == id) {
            Some(index) => format!("&{}", index + 1),
            None => "&gone".to_owned(),
        })
    };
    let mut next = replace_key(text, &format!("checks.{service}"), rename);
    if this {
        next = replace_key(&next, "checks.this", rename);
    }
    (next != text).then_some(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_hold_their_title_sections_and_response() {
        let source = "#check one\n#target https://a/x\n#metadata\nAccept: */*\n#responce 200\n{\"a\":1}\n\n#check\n#target https://b";
        let regions = vec![ResponseRegion {
            request: 0,
            header: 4,
            end: 6,
            columns: Vec::new(),
        }];
        let checks = tree(source, &regions);
        assert_eq!(checks["1"]["title"], "one");
        assert_eq!(checks["1"]["target"], "https://a/x");
        assert_eq!(checks["1"]["metadata"], "Accept: */*");
        assert_eq!(checks["1"]["response"], "{\"a\":1}");
        assert_eq!(checks["2"]["target"], "https://b");
        assert_eq!(checks["2"]["response"], "");
        assert_eq!(number_at(source, &regions, 5), Some(1));
        assert_eq!(number_at(source, &regions, 8), Some(2));
    }

    #[test]
    fn prev_becomes_the_number_of_the_check_above() {
        let text = "{{.checks.rest.prev.target}} {{.checks.this.prev.title}} {{.checks.alpha.prev.x}} {{.xchecks.rest.prev}}";
        assert_eq!(
            with_prev(text, "rest", 3),
            "{{.checks.rest.2.target}} {{.checks.rest.2.title}} {{.checks.alpha.prev.x}} {{.xchecks.rest.prev}}"
        );
        assert_eq!(
            with_prev("{{:checks.this.&4.target}}", "rest", 1),
            "{{:checks.rest.&4.target}}"
        );
    }

    #[test]
    fn pinned_numbers_follow_their_checks() {
        let ids = |list: &[&str]| list.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let text = "{{.checks.rest.&2.target}} {{.checks.rest.2.target}} {{.checks.this.&3.title}} {{.checks.alpha.&2.x}}";
        assert_eq!(
            renumber(
                text,
                "rest",
                true,
                &ids(&["a", "b", "c"]),
                &ids(&["n", "a", "b"])
            )
            .unwrap(),
            "{{.checks.rest.&3.target}} {{.checks.rest.2.target}} {{.checks.this.&gone.title}} {{.checks.alpha.&2.x}}"
        );
        assert_eq!(
            renumber(
                text,
                "rest",
                false,
                &ids(&["a", "b", "c"]),
                &ids(&["b", "c"])
            )
            .unwrap(),
            "{{.checks.rest.&1.target}} {{.checks.rest.2.target}} {{.checks.this.&3.title}} {{.checks.alpha.&2.x}}"
        );
        assert_eq!(
            renumber(text, "rest", true, &ids(&["a"]), &ids(&["a"])),
            None
        );
    }
}
