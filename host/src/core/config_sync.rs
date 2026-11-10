//! The three-way comparison of a configuration file between the local source, a shared
//! source and the cache (the shared source as of the last sync, the common base). Files are
//! compared by record: a Dictionary by set and variable, Templates by service and section,
//! hints line by line. A record both sides changed differently is a conflict; the rest merges
//! by itself.
//!
//! Texts come out in a stable form (sorted keys, one record per line) so a difference shown
//! to the user is a record.

use super::sources::{DICTIONARY, HINTS, TEMPLATES};
use serde_json::{Map, Value};
use similar::{Algorithm, capture_diff_slices};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dictionary,
    Templates,
    Hints,
}

impl Kind {
    /// The kind of a source's configuration file path.
    pub fn of(path: &str) -> Option<Self> {
        match path.split('/').collect::<Vec<_>>().as_slice() {
            [DICTIONARY] => Some(Self::Dictionary),
            [TEMPLATES] => Some(Self::Templates),
            [HINTS, file] if file.ends_with(".md") => Some(Self::Hints),
            _ => None,
        }
    }
}

/// What differs between the local file, the shared one and their base.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    #[default]
    Same,
    /// Only the local file changed since the base.
    Local,
    /// Only the shared file changed since the base.
    Source,
    /// Both changed, in different records.
    Both,
    Conflict,
}

/// The merged file: the two sides are the same unless `conflicts` records differ; then
/// `left` has the local side of them and `right` the shared one.
#[derive(Debug, PartialEq, Eq)]
pub struct Merged {
    pub left: String,
    pub right: String,
    pub conflicts: usize,
}

type Groups = BTreeMap<String, BTreeMap<String, Value>>;

/// A parsed Dictionary or Templates file.
#[derive(Default)]
struct Document {
    order: Vec<String>,
    groups: Groups,
}

fn parse(kind: Kind, text: &str) -> Result<Document, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let object = value
        .as_object()
        .ok_or("The file must hold a JSON object")?;
    let mut document = Document::default();
    let sets = match kind {
        Kind::Dictionary => {
            document.order = object
                .get("order")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            // A dictionary written before sets were named so keeps its sets in "profiles".
            object
                .get("sets")
                .or_else(|| object.get("profiles"))
                .and_then(Value::as_object)
                .ok_or(
                    "The Dictionary must hold {\"sets\": {\"<set>\": {\"<VARIABLE>\": value}}}",
                )?
        }
        _ => object,
    };
    for (name, records) in sets {
        let records = records
            .as_object()
            .ok_or_else(|| format!("\"{name}\" must be an object"))?;
        document.groups.insert(
            name.clone(),
            records
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );
    }
    Ok(document)
}

fn render(kind: Kind, order: &[String], groups: &Groups) -> String {
    let sets: Map<String, Value> = groups
        .iter()
        .map(|(name, records)| {
            let records: Map<String, Value> = records
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            (name.clone(), Value::Object(records))
        })
        .collect();
    let value = match kind {
        Kind::Dictionary => {
            let mut object = Map::new();
            object.insert(
                "order".into(),
                Value::Array(
                    order
                        .iter()
                        .filter(|name| groups.contains_key(*name))
                        .map(|name| Value::String(name.clone()))
                        .collect(),
                ),
            );
            object.insert("sets".into(), Value::Object(sets));
            Value::Object(object)
        }
        _ => Value::Object(sets),
    };
    let mut text = serde_json::to_string_pretty(&value).unwrap_or_default();
    text.push('\n');
    text
}

/// The stable form of a file.
pub fn stable(kind: Kind, text: &str) -> Result<String, String> {
    match kind {
        Kind::Hints => Ok(text.to_owned()),
        _ => {
            let document = parse(kind, text)?;
            Ok(render(kind, &document.order, &document.groups))
        }
    }
}

/// One record in the three versions: what each side keeps and whether they conflict.
/// `base` is `None` without a base, `Some(None)` for a record the base lacks.
#[allow(clippy::type_complexity)]
fn pick<'a>(
    base: Option<Option<&'a Value>>,
    local: Option<&'a Value>,
    source: Option<&'a Value>,
) -> (Option<&'a Value>, Option<&'a Value>, bool) {
    if local == source {
        return (local, source, false);
    }
    match base {
        Some(base) if base == local => (source, source, false),
        Some(base) if base == source => (local, local, false),
        _ => (local, source, true),
    }
}

fn merge_documents(
    kind: Kind,
    base: Option<&Document>,
    local: &Document,
    source: &Document,
) -> Merged {
    let names: BTreeSet<&String> = local.groups.keys().chain(source.groups.keys()).collect();
    let none = BTreeMap::new();
    let (mut left, mut right) = (Groups::new(), Groups::new());
    let mut conflicts = 0;
    for name in names {
        let base_group = base.map(|base| base.groups.get(name).unwrap_or(&none));
        let (l, s) = (
            local.groups.get(name).unwrap_or(&none),
            source.groups.get(name).unwrap_or(&none),
        );
        let keys: BTreeSet<&String> = l
            .keys()
            .chain(s.keys())
            .chain(base_group.into_iter().flat_map(|group| group.keys()))
            .collect();
        let (mut left_group, mut right_group) = (BTreeMap::new(), BTreeMap::new());
        for key in keys {
            let (kept_left, kept_right, conflict) = pick(
                base_group.map(|group| group.get(key)),
                l.get(key),
                s.get(key),
            );
            conflicts += usize::from(conflict);
            if let Some(value) = kept_left {
                left_group.insert(key.clone(), value.clone());
            }
            if let Some(value) = kept_right {
                right_group.insert(key.clone(), value.clone());
            }
        }
        // A set without variables stays or goes like a record.
        let marker = Value::Bool(true);
        let has = |document: &Document| document.groups.contains_key(name).then_some(&marker);
        let (kept_left, kept_right, _) = pick(
            base.map(|base| base.groups.contains_key(name).then_some(&marker)),
            has(local),
            has(source),
        );
        if kept_left.is_some() || !left_group.is_empty() {
            left.insert(name.clone(), left_group);
        }
        if kept_right.is_some() || !right_group.is_empty() {
            right.insert(name.clone(), right_group);
        }
    }
    let order = merged_order(base, local, source);
    Merged {
        left: render(kind, &order, &left),
        right: render(kind, &order, &right),
        conflicts,
    }
}

/// The set order: a side's own change wins; both sides' changes keep the local order with
/// the shared side's new names after it.
fn merged_order(base: Option<&Document>, local: &Document, source: &Document) -> Vec<String> {
    let base = base.map(|base| &base.order);
    if local.order == source.order {
        return local.order.clone();
    }
    if base == Some(&local.order) {
        return source.order.clone();
    }
    if base == Some(&source.order) {
        return local.order.clone();
    }
    let mut order = local.order.clone();
    for name in &source.order {
        if !order.contains(name) {
            order.push(name.clone());
        }
    }
    order
}

/// A run of lines both sides agree on, or the two sides' versions of a piece they changed.
enum Chunk {
    Same(String),
    Conflict(String, String),
}

fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// A piece of the base a side replaced: base lines `start..end` became `lines`.
struct Hunk<'a> {
    start: usize,
    end: usize,
    lines: Vec<&'a str>,
}

fn hunks<'a>(base: &[&str], side: &[&'a str]) -> Vec<Hunk<'a>> {
    capture_diff_slices(Algorithm::Myers, base, side)
        .into_iter()
        .filter_map(|op| {
            let (tag, old, new) = op.as_tag_tuple();
            (tag != similar::DiffTag::Equal).then(|| Hunk {
                start: old.start,
                end: old.end,
                lines: side[new].to_vec(),
            })
        })
        .collect()
}

fn overlaps(hunk: &Hunk, start: usize, end: usize) -> bool {
    hunk.start < end || (hunk.start == end && hunk.end == hunk.start && start == end)
}

fn apply(base: &[&str], start: usize, end: usize, hunks: &[Hunk]) -> String {
    let mut text = String::new();
    let mut at = start;
    for hunk in hunks {
        text.extend(base[at..hunk.start].iter().copied());
        text.extend(hunk.lines.iter().copied());
        at = hunk.end;
    }
    text.extend(base[at..end].iter().copied());
    text
}

fn merge_lines(base: &[&str], local: &[&str], source: &[&str]) -> Vec<Chunk> {
    let (local_hunks, source_hunks) = (hunks(base, local), hunks(base, source));
    let (mut i, mut j, mut at) = (0, 0, 0);
    let mut chunks = Vec::new();
    while i < local_hunks.len() || j < source_hunks.len() {
        let local_first = j >= source_hunks.len()
            || (i < local_hunks.len() && local_hunks[i].start <= source_hunks[j].start);
        let (first, from) = if local_first {
            (&local_hunks[i], 0)
        } else {
            (&source_hunks[j], 1)
        };
        let (start, mut end) = (first.start, first.end);
        let (i0, j0) = (i, j);
        if from == 0 {
            i += 1;
        } else {
            j += 1;
        }
        loop {
            let mut grew = false;
            if i < local_hunks.len() && overlaps(&local_hunks[i], start, end) {
                end = end.max(local_hunks[i].end);
                i += 1;
                grew = true;
            }
            if j < source_hunks.len() && overlaps(&source_hunks[j], start, end) {
                end = end.max(source_hunks[j].end);
                j += 1;
                grew = true;
            }
            if !grew {
                break;
            }
        }
        chunks.push(Chunk::Same(base[at..start].concat()));
        let mine = apply(base, start, end, &local_hunks[i0..i]);
        let theirs = apply(base, start, end, &source_hunks[j0..j]);
        chunks.push(if mine == theirs {
            Chunk::Same(mine)
        } else if i0 == i {
            Chunk::Same(theirs)
        } else if j0 == j {
            Chunk::Same(mine)
        } else {
            Chunk::Conflict(mine, theirs)
        });
        at = end;
    }
    chunks.push(Chunk::Same(base[at..].concat()));
    chunks
}

/// Without a base every difference is a conflict.
fn compare_lines(local: &[&str], source: &[&str]) -> Vec<Chunk> {
    capture_diff_slices(Algorithm::Myers, local, source)
        .into_iter()
        .map(|op| {
            let (tag, old, new) = op.as_tag_tuple();
            if tag == similar::DiffTag::Equal {
                Chunk::Same(local[old].concat())
            } else {
                Chunk::Conflict(local[old].concat(), source[new].concat())
            }
        })
        .collect()
}

/// Merges `local` and `source` (of `kind`) given their `base`, if any.
pub fn merge(kind: Kind, base: Option<&str>, local: &str, source: &str) -> Result<Merged, String> {
    if kind == Kind::Hints {
        let (l, s) = (lines(local), lines(source));
        let chunks = match base {
            Some(base) => merge_lines(&lines(base), &l, &s),
            None => compare_lines(&l, &s),
        };
        let (mut left, mut right, mut conflicts) = (String::new(), String::new(), 0);
        for chunk in chunks {
            match chunk {
                Chunk::Same(text) => {
                    left.push_str(&text);
                    right.push_str(&text);
                }
                Chunk::Conflict(mine, theirs) => {
                    left.push_str(&mine);
                    right.push_str(&theirs);
                    conflicts += 1;
                }
            }
        }
        return Ok(Merged {
            left,
            right,
            conflicts,
        });
    }
    let local = parse(kind, local).map_err(|e| format!("the local file: {e}"))?;
    let source = parse(kind, source).map_err(|e| format!("the shared file: {e}"))?;
    let base = base.and_then(|base| parse(kind, base).ok());
    Ok(merge_documents(kind, base.as_ref(), &local, &source))
}

/// What differs between the three versions of a file.
pub fn state(kind: Kind, base: Option<&str>, local: &str, source: &str) -> Result<State, String> {
    let merged = merge(kind, base, local, source)?;
    let (stable_local, stable_source) = (stable(kind, local)?, stable(kind, source)?);
    if stable_local == stable_source {
        return Ok(State::Same);
    }
    let Some(base) = base.and_then(|base| stable(kind, base).ok()) else {
        return Ok(State::Conflict);
    };
    Ok(if base == stable_local {
        State::Source
    } else if base == stable_source {
        State::Local
    } else if merged.conflicts > 0 {
        State::Conflict
    } else {
        State::Both
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dictionary(sets: Value) -> String {
        serde_json::to_string(&json!({"sets": sets})).unwrap()
    }

    #[test]
    fn records_of_a_dictionary_merge_by_variable() {
        let base = dictionary(json!({"dev": {"HOST": "a", "PORT": "1"}}));
        let local = dictionary(json!({"dev": {"HOST": "b", "PORT": "1"}}));
        let source = dictionary(json!({"dev": {"HOST": "a", "PORT": "2"}, "qa": {"X": "1"}}));
        assert_eq!(
            state(Kind::Dictionary, Some(&base), &local, &source),
            Ok(State::Both)
        );
        let merged = merge(Kind::Dictionary, Some(&base), &local, &source).unwrap();
        assert_eq!(merged.conflicts, 0);
        assert_eq!(merged.left, merged.right);
        let value: Value = serde_json::from_str(&merged.left).unwrap();
        assert_eq!(
            value["sets"],
            json!({"dev": {"HOST": "b", "PORT": "2"}, "qa": {"X": "1"}})
        );
    }

    #[test]
    fn one_side_changing_is_not_a_conflict() {
        let base = dictionary(json!({"dev": {"HOST": "a"}}));
        let changed = dictionary(json!({"dev": {"HOST": "b"}}));
        assert_eq!(
            state(Kind::Dictionary, Some(&base), &changed, &base),
            Ok(State::Local)
        );
        assert_eq!(
            state(Kind::Dictionary, Some(&base), &base, &changed),
            Ok(State::Source)
        );
        assert_eq!(
            state(Kind::Dictionary, Some(&base), &changed, &changed),
            Ok(State::Same)
        );
    }

    #[test]
    fn the_same_record_changed_on_both_sides_conflicts_and_shows_both_versions() {
        let base = dictionary(json!({"dev": {"HOST": "a", "PORT": "1"}}));
        let local = dictionary(json!({"dev": {"HOST": "b", "PORT": "2"}}));
        let source = dictionary(json!({"dev": {"HOST": "c", "PORT": "1"}}));
        assert_eq!(
            state(Kind::Dictionary, Some(&base), &local, &source),
            Ok(State::Conflict)
        );
        let merged = merge(Kind::Dictionary, Some(&base), &local, &source).unwrap();
        assert_eq!(merged.conflicts, 1);
        let left: Value = serde_json::from_str(&merged.left).unwrap();
        let right: Value = serde_json::from_str(&merged.right).unwrap();
        // The record both changed differs; the one only the local side changed is merged in.
        assert_eq!(left["sets"]["dev"], json!({"HOST": "b", "PORT": "2"}));
        assert_eq!(right["sets"]["dev"], json!({"HOST": "c", "PORT": "2"}));
    }

    #[test]
    fn deleting_a_record_the_other_side_changed_conflicts() {
        let base = dictionary(json!({"dev": {"HOST": "a", "PORT": "1"}}));
        let local = dictionary(json!({"dev": {"PORT": "1"}}));
        let source = dictionary(json!({"dev": {"HOST": "b", "PORT": "1"}}));
        assert_eq!(
            state(Kind::Dictionary, Some(&base), &local, &source),
            Ok(State::Conflict)
        );
    }

    #[test]
    fn without_a_base_every_difference_conflicts() {
        let local = dictionary(json!({"dev": {"HOST": "a"}}));
        let source = dictionary(json!({"dev": {"HOST": "a", "PORT": "1"}}));
        assert_eq!(
            state(Kind::Dictionary, None, &local, &source),
            Ok(State::Conflict)
        );
        assert_eq!(
            state(Kind::Dictionary, None, &local, &local),
            Ok(State::Same)
        );
    }

    #[test]
    fn set_order_merges_without_a_conflict() {
        let with = |order: Value, sets: Value| {
            serde_json::to_string(&json!({"order": order, "sets": sets})).unwrap()
        };
        let base = with(json!(["a"]), json!({"a": {}}));
        let local = with(json!(["a", "b"]), json!({"a": {}, "b": {"X": "1"}}));
        let source = with(json!(["a", "c"]), json!({"a": {}, "c": {"Y": "1"}}));
        let merged = merge(Kind::Dictionary, Some(&base), &local, &source).unwrap();
        assert_eq!(merged.conflicts, 0);
        let value: Value = serde_json::from_str(&merged.left).unwrap();
        assert_eq!(value["order"], json!(["a", "b", "c"]));
    }

    #[test]
    fn templates_merge_by_section() {
        let library = |check: &str, send: &str| {
            serde_json::to_string(&json!({"rest": {"check": check, "send": send}})).unwrap()
        };
        let base = library("@a\nx\n", "@b\ny\n");
        let local = library("@a\nx2\n", "@b\ny\n");
        let source = library("@a\nx\n", "@b\ny2\n");
        let merged = merge(Kind::Templates, Some(&base), &local, &source).unwrap();
        assert_eq!(merged.conflicts, 0);
        let value: Value = serde_json::from_str(&merged.left).unwrap();
        assert_eq!(
            value["rest"],
            json!({"check": "@a\nx2\n", "send": "@b\ny2\n"})
        );
        let clash = library("@a\nx3\n", "@b\ny\n");
        let merged = merge(Kind::Templates, Some(&base), &local, &clash).unwrap();
        assert_eq!(merged.conflicts, 1);
    }

    #[test]
    fn hints_merge_by_line() {
        let base = "one\ntwo\nthree\nfour\n";
        let local = "ONE\ntwo\nthree\nfour\n";
        let source = "one\ntwo\nthree\nFOUR\n";
        let merged = merge(Kind::Hints, Some(base), local, source).unwrap();
        assert_eq!(merged.conflicts, 0);
        assert_eq!(merged.left, "ONE\ntwo\nthree\nFOUR\n");
        assert_eq!(
            state(Kind::Hints, Some(base), local, source),
            Ok(State::Both)
        );
        let clash = "one\nTWO!\nthree\nfour\n";
        let both = "one\nTWO\nthree\nfour\n";
        let merged = merge(Kind::Hints, Some(base), both, clash).unwrap();
        assert_eq!(merged.conflicts, 1);
        assert_eq!(merged.left, both);
        assert_eq!(merged.right, clash);
        assert_eq!(
            state(Kind::Hints, Some(base), both, clash),
            Ok(State::Conflict)
        );
    }

    #[test]
    fn hints_without_a_base_conflict_where_they_differ() {
        let merged = merge(Kind::Hints, None, "a\nb\nc\n", "a\nx\nc\n").unwrap();
        assert_eq!(merged.conflicts, 1);
        assert_eq!(merged.left, "a\nb\nc\n");
        assert_eq!(merged.right, "a\nx\nc\n");
    }
}
