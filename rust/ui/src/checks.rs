use crate::document::ResponseRegion;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub id: String,
    pub created_at: f64,
    pub line: usize,
    #[serde(default)]
    pub title: String,
}
impl Identity {
    pub fn new(line: usize) -> Self {
        let id = uuid::Uuid::now_v7();
        let (seconds, nanos) = id.get_timestamp().expect("UUIDv7 includes time").to_unix();
        Self {
            id: id.to_string(),
            created_at: seconds as f64 * 1000.0 + nanos as f64 / 1_000_000.0,
            line,
            title: String::new(),
        }
    }
}
pub fn normalize(source: &str, regions: &[ResponseRegion], checks: &mut Vec<Identity>) {
    let lines = crate::document::check_lines(source, regions);
    let mut ids = BTreeSet::new();
    let mut owned = BTreeSet::new();
    checks.retain(|check| {
        lines.contains(&check.line)
            && uuid::Uuid::parse_str(&check.id).is_ok()
            && ids.insert(check.id.clone())
            && owned.insert(check.line)
    });
    for line in lines {
        if owned.insert(line) {
            checks.push(Identity::new(line));
        }
    }
    checks.sort_by_key(|check| check.line);
    let mut source_lines = source.split('\n');
    let mut next = 0;
    for check in checks {
        let line = source_lines.nth(check.line - next).unwrap_or_default();
        check.title = line
            .trim_start()
            .split_once(char::is_whitespace)
            .map(|(_, title)| title.trim())
            .unwrap_or_default()
            .to_owned();
        next = check.line + 1;
    }
}
fn offsets(source: &str) -> Vec<usize> {
    let mut offset = 0;
    source
        .split('\n')
        .map(|line| {
            let start = offset;
            offset += line.len() + 1;
            start
        })
        .collect()
}
pub fn remap(
    old: &str,
    new: &str,
    from: usize,
    to: usize,
    inserted: usize,
    checks: &mut Vec<Identity>,
) {
    let starts = offsets(old);
    let next = offsets(new);
    let single = !old[from..to].contains('\n') && !new[from..from + inserted].contains('\n');
    checks.retain_mut(|check| {
        let Some(&offset) = starts.get(check.line) else {
            return false;
        };
        let offset = if offset < from {
            offset
        } else if offset >= to {
            offset - (to - from) + inserted
        } else if single {
            from
        } else {
            return false;
        };
        check.line = next
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        true
    });
}
pub fn remap_text(old: &str, new: &str, checks: &mut Vec<Identity>) {
    let from: usize = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum();
    let tail: usize = old[from..]
        .chars()
        .rev()
        .zip(new[from..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum();
    remap(
        old,
        new,
        from,
        old.len() - tail,
        new.len() - from - tail,
        checks,
    );
}
