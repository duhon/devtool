use crate::console::RequestBlock;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub ch: usize,
}
#[derive(Clone, Debug, Deserialize)]
pub struct TextChange {
    pub from: Position,
    pub to: Position,
    pub text: Vec<String>,
    pub removed: Vec<String>,
    pub origin: Option<String>,
}
#[derive(Clone)]
pub struct DocumentEdit {
    pub text: String,
    pub regions: Vec<ResponseRegion>,
    pub checks: Vec<crate::checks::Identity>,
}
#[derive(Clone)]
pub struct ClipboardFragment {
    text: String,
    regions: Vec<ResponseRegion>,
    checks: Vec<crate::checks::Identity>,
    scope: String,
    moved: bool,
}
impl ClipboardFragment {
    pub fn pasted_regions(&self, text: &str) -> Option<&[ResponseRegion]> {
        (self.text == text).then_some(self.regions.as_slice())
    }
}
#[derive(Default)]
pub struct EditHistory {
    text: String,
    regions: Vec<ResponseRegion>,
    snapshots: BTreeMap<u64, Vec<ResponseRegion>>,
    checks: Vec<crate::checks::Identity>,
    check_snapshots: BTreeMap<u64, Vec<crate::checks::Identity>>,
    pub scope: String,
}
impl EditHistory {
    pub fn synchronize(&mut self, text: &str, regions: &[ResponseRegion], generation: u64) {
        if self.text != text {
            crate::checks::remap_text(&self.text, text, &mut self.checks);
        }
        crate::checks::normalize(text, regions, &mut self.checks);
        self.text = text.into();
        self.regions = regions.into();
        self.snapshots.insert(generation, regions.into());
        self.check_snapshots.insert(generation, self.checks.clone());
    }
    pub fn synchronize_checks(&mut self, checks: &[crate::checks::Identity], generation: u64) {
        self.checks = checks.into();
        crate::checks::normalize(&self.text, &self.regions, &mut self.checks);
        self.check_snapshots.insert(generation, self.checks.clone());
    }
    pub fn retain_generations(&mut self, generations: &BTreeSet<u64>) {
        self.snapshots
            .retain(|generation, _| generations.contains(generation));
        self.check_snapshots
            .retain(|generation, _| generations.contains(generation));
    }
    pub fn clipboard(&self, from: &Position, to: &Position) -> Result<ClipboardFragment, String> {
        let mut fragment = clipboard_fragment(&self.text, &self.regions, from, to)?;
        fragment.checks = self
            .checks
            .iter()
            .filter(|check| {
                check.line >= from.line
                    && (check.line > from.line || from.ch == 0)
                    && check.line < to.line
            })
            .cloned()
            .map(|mut check| {
                check.line -= from.line;
                check
            })
            .collect();
        fragment.scope = self.scope.clone();
        Ok(fragment)
    }
    pub fn apply(
        &mut self,
        text: String,
        changes: &[TextChange],
        generation: u64,
        clipboard: &mut Option<ClipboardFragment>,
    ) -> Result<DocumentEdit, String> {
        let mut source = self.text.clone();
        let mut regions = self.regions.clone();
        let mut checks = self.checks.clone();
        normalize_regions(&source, &mut regions);
        let mut copied = clipboard.clone();
        for change in changes {
            let from = offset(&source, &change.from)?;
            let to = offset(&source, &change.to)?;
            if from > to || source[from..to] != change.removed.join("\n") {
                return Err("Editor change does not match the current Console document".into());
            }
            if change.origin.as_deref() == Some("cut") {
                let mut fragment = clipboard_fragment(&source, &regions, &change.from, &change.to)?;
                fragment.checks = checks
                    .iter()
                    .filter(|check| {
                        check.line >= change.from.line
                            && (check.line > change.from.line || change.from.ch == 0)
                            && check.line < change.to.line
                    })
                    .cloned()
                    .map(|mut check| {
                        check.line -= change.from.line;
                        check
                    })
                    .collect();
                fragment.scope = self.scope.clone();
                fragment.moved = true;
                copied = Some(fragment);
            }
            let inserted = change.text.join("\n");
            let next = format!("{}{}{}", &source[..from], inserted, &source[to..]);
            if change.origin.as_deref() == Some("setValue") {
                regions.clear();
                checks.clear();
            } else {
                crate::checks::remap(&source, &next, from, to, inserted.len(), &mut checks);
                remap_change(&source, &next, change, from, to, &mut regions);
                if change.origin.as_deref() == Some("paste")
                    && let Some(fragment) = &copied
                    && fragment.text == inserted
                {
                    regions.extend(fragment.regions.iter().map(|region| ResponseRegion {
                        request: region.request + change.from.line,
                        header: region.header + change.from.line,
                        end: region.end + change.from.line,
                        table: region.table.clone(),
                    }));
                    if fragment.moved && fragment.scope == self.scope {
                        checks.extend(fragment.checks.iter().cloned().map(|mut check| {
                            check.line += change.from.line;
                            check
                        }));
                    }
                }
                crate::checks::normalize(&next, &regions, &mut checks);
                if change.origin.as_deref() == Some("paste")
                    && let Some(fragment) = &mut copied
                {
                    fragment.moved = false;
                }
                normalize_regions(&next, &mut regions);
            }
            source = next;
        }
        if source != text {
            return Err("Editor changes did not produce the reported Console document".into());
        }
        if changes
            .iter()
            .all(|change| matches!(change.origin.as_deref(), Some("undo" | "redo")))
            && let Some(snapshot) = self.snapshots.get(&generation)
        {
            regions = snapshot.clone();
            if let Some(snapshot) = self.check_snapshots.get(&generation) {
                checks = snapshot.clone();
            }
        }
        self.text = text.clone();
        self.checks = checks.clone();
        self.synchronize(&text, &regions, generation);
        *clipboard = copied;
        Ok(DocumentEdit {
            text,
            regions,
            checks,
        })
    }
}

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
fn line_offsets(source: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(source.match_indices('\n').map(|(index, _)| index + 1))
        .collect()
}
fn clipboard_fragment(
    source: &str,
    regions: &[ResponseRegion],
    from: &Position,
    to: &Position,
) -> Result<ClipboardFragment, String> {
    let start = offset(source, from)?;
    let end = offset(source, to)?;
    if start > end {
        return Err("Invalid Console clipboard selection".into());
    }
    let starts = line_offsets(source);
    let regions = regions
        .iter()
        .filter(|region| {
            from.ch == 0
                && region.request >= from.line
                && starts.get(region.end).copied().unwrap_or(source.len()) <= end
        })
        .map(|region| ResponseRegion {
            request: region.request - from.line,
            header: region.header - from.line,
            end: region.end - from.line,
            table: region.table.clone(),
        })
        .collect();
    Ok(ClipboardFragment {
        text: source[start..end].into(),
        regions,
        checks: Vec::new(),
        scope: String::new(),
        moved: false,
    })
}
fn remap_change(
    old: &str,
    new: &str,
    change: &TextChange,
    from: usize,
    to: usize,
    regions: &mut Vec<ResponseRegion>,
) {
    let old_starts = line_offsets(old);
    let new_starts = line_offsets(new);
    let inserted = change.text.join("\n").len();
    let map = |position: usize, right: bool| {
        if position < from || position == from && !right {
            position
        } else if position >= to {
            position - (to - from) + inserted
        } else {
            from
        }
    };
    let line = |position| {
        new_starts
            .partition_point(|start| *start <= position)
            .saturating_sub(1)
    };
    for region in regions.iter_mut() {
        region.request = line(map(old_starts[region.request], true));
        region.header = line(map(old_starts[region.header], true));
        let end = map(
            old_starts.get(region.end).copied().unwrap_or(old.len()),
            false,
        );
        region.end = match new_starts.binary_search(&end) {
            Ok(line) => line,
            Err(line) => line,
        };
    }
    let old_lines: Vec<_> = old.split('\n').collect();
    let new_lines: Vec<_> = new.split('\n').collect();
    let inserted_end = change.from.line + change.text.len().saturating_sub(1);
    let mut fragments = Vec::new();
    let tail_offset = from + inserted;
    let tail_line = new_starts.partition_point(|start| *start < tail_offset);
    for (index, text) in new_lines
        .iter()
        .enumerate()
        .take(inserted_end + 1)
        .skip(change.from.line)
    {
        let was_check = index == change.from.line
            && old_lines
                .get(change.from.line)
                .is_some_and(|line| crate::console::is_check(line));
        if crate::console::is_check(text)
            && (!was_check || change.origin.as_deref() == Some("paste"))
        {
            for region in regions.iter_mut() {
                if index > region.header && index < region.end {
                    if tail_line < region.end {
                        fragments.push(ResponseRegion {
                            request: region.request,
                            header: tail_line,
                            end: region.end,
                            table: region.table.clone(),
                        });
                    }
                    region.end = index;
                }
            }
        }
    }
    regions.extend(fragments);
}
fn normalize_regions(source: &str, regions: &mut Vec<ResponseRegion>) {
    let lines: Vec<_> = source.split('\n').collect();
    *regions = valid_regions(&lines, regions);
    let checks = check_lines(source, regions);
    let mut owners = BTreeMap::new();
    for region in regions.iter() {
        owners
            .entry(region.request)
            .and_modify(|header: &mut usize| *header = (*header).min(region.header))
            .or_insert(region.header);
    }
    let owners: BTreeMap<_, _> = owners
        .into_iter()
        .filter_map(|(owner, header)| {
            checks
                .iter()
                .rev()
                .find(|line| **line < header)
                .map(|request| (owner, *request))
        })
        .collect();
    for region in regions.iter_mut() {
        if let Some(request) = owners.get(&region.request) {
            region.request = *request;
        }
    }
    regions.sort_by_key(|region| region.header);
    regions.dedup();
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseRegion {
    pub request: usize,
    /// First owned output line; split tails need not start with a response marker.
    pub header: usize,
    /// Exclusive line boundary. Output markers inside this region are never requests.
    pub end: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<crate::tables::Metadata>,
}
fn marker(line: &str, name: &str) -> bool {
    line.trim_start()
        .to_ascii_lowercase()
        .strip_prefix(&format!("#{name}"))
        .is_some_and(|s| s.is_empty() || s.starts_with(char::is_whitespace))
}
fn valid_regions(lines: &[&str], regions: &[ResponseRegion]) -> Vec<ResponseRegion> {
    let anchored = |region: &&ResponseRegion| {
        region.request < region.header
            && region.header < region.end
            && region.end <= lines.len()
            && lines
                .get(region.request)
                .is_some_and(|line| crate::console::is_check(line))
    };
    let primary: BTreeMap<usize, usize> = regions
        .iter()
        .filter(anchored)
        .filter(|region| {
            marker(lines[region.header], "responce") || marker(lines[region.header], "response")
        })
        .fold(BTreeMap::new(), |mut primary, region| {
            primary
                .entry(region.request)
                .and_modify(|header| *header = (*header).min(region.header))
                .or_insert(region.header);
            primary
        });
    regions
        .iter()
        .filter(anchored)
        .filter(|region| {
            primary
                .get(&region.request)
                .is_some_and(|header| *header <= region.header)
        })
        .cloned()
        .collect()
}
pub fn request_at(source: &str, cursor: usize, regions: &[ResponseRegion]) -> Option<RequestBlock> {
    let lines: Vec<_> = source.split('\n').collect();
    let regions = valid_regions(&lines, regions);
    let cursor = regions
        .iter()
        .find(|r| cursor >= r.header && cursor < r.end)
        .map(|r| r.request)
        .unwrap_or(cursor)
        .min(lines.len().saturating_sub(1));
    let output = |line| regions.iter().any(|r| line >= r.header && line < r.end);
    let start = (0..=cursor)
        .rev()
        .find(|i| !output(*i) && crate::console::is_check(lines[*i]))?;
    let next = (start + 1..lines.len())
        .find(|i| !output(*i) && crate::console::is_check(lines[*i]))
        .unwrap_or(lines.len());
    let own = regions.iter().find(|r| r.request == start);
    let end = own.map(|r| r.header).unwrap_or_else(|| {
        (start + 1..next)
            .find(|i| {
                regions.iter().any(|region| region.header == *i)
                    || !output(*i)
                        && (marker(lines[*i], "responce") || marker(lines[*i], "response"))
            })
            .unwrap_or(next)
    });
    Some(RequestBlock {
        start,
        end,
        response_end: next,
        text: lines[start + 1..end].join("\n").trim().into(),
    })
}
pub fn remap(old: &str, new: &str, regions: &mut Vec<ResponseRegion>) {
    let a: Vec<_> = old.split('\n').collect();
    let b: Vec<_> = new.split('\n').collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let old_end = a.len() - suffix;
    let new_end = b.len() - suffix;
    let map = |line: usize| {
        if line < prefix {
            line
        } else if line >= old_end {
            line.saturating_add_signed(new_end as isize - old_end as isize)
        } else {
            prefix + (line - prefix).min(new_end - prefix)
        }
    };
    for r in regions.iter_mut() {
        r.request = map(r.request);
        r.header = map(r.header);
        r.end = map(r.end);
    }
    *regions = valid_regions(&b, regions);
}
pub fn replace_response(
    source: &str,
    request: usize,
    status: &str,
    body: &str,
    regions: &mut Vec<ResponseRegion>,
) -> String {
    let Some(block) = request_at(source, request, regions) else {
        return source.into();
    };
    let tails: Vec<_> = regions
        .iter()
        .filter(|region| region.request == block.start && region.header >= block.response_end)
        .cloned()
        .collect();
    if !tails.is_empty() {
        let mut cleaned = source.to_owned();
        for tail in tails.into_iter().rev() {
            let starts = line_offsets(&cleaned);
            let from = starts[tail.header];
            let to = starts.get(tail.end).copied().unwrap_or(cleaned.len());
            let before = cleaned.clone();
            cleaned.replace_range(from..to, "");
            regions.retain(|region| region != &tail);
            remap(&before, &cleaned, regions);
        }
        return replace_response(&cleaned, request, status, body, regions);
    }
    let lines: Vec<_> = source.split('\n').collect();
    let before = lines[..block.end].join("\n");
    let before = before.trim_end();
    let after = lines[block.response_end..].join("\n");
    let prefix = format!(
        "{before}\n#responce {status}\n{}\n{}",
        body.trim_end(),
        if after.is_empty() { "" } else { "\n" }
    );
    let header = before.split('\n').count();
    let end = prefix.bytes().filter(|c| *c == b'\n').count();
    let result = format!("{prefix}{after}");
    regions.retain(|r| {
        r.request != block.start && !(r.header >= block.end && r.end <= block.response_end)
    });
    remap(source, &result, regions);
    regions.push(ResponseRegion {
        request: block.start,
        header,
        end,
        table: None,
    });
    regions.sort_by_key(|r| r.header);
    result
}
pub fn mask_output(source: &str, regions: &[ResponseRegion]) -> String {
    let lines: Vec<_> = source.split('\n').collect();
    let regions = valid_regions(&lines, regions);
    source
        .split('\n')
        .enumerate()
        .map(|(i, line)| {
            if regions.iter().any(|r| i >= r.header && i < r.end) {
                String::new()
            } else {
                line.into()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
pub fn check_lines(source: &str, regions: &[ResponseRegion]) -> Vec<usize> {
    let lines: Vec<_> = source.split('\n').collect();
    let regions = valid_regions(&lines, regions);
    source
        .split('\n')
        .enumerate()
        .filter(|(line, text)| {
            crate::console::is_check(text)
                && !regions.iter().any(|r| *line >= r.header && *line < r.end)
        })
        .map(|(line, _)| line)
        .collect()
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CheckViewport {
    pub top: usize,
    pub visible_cursor: Option<usize>,
}

impl CheckViewport {
    pub fn active_check(self, lines: &[usize]) -> Option<usize> {
        active_check(lines, self.visible_cursor.unwrap_or(self.top))
    }
}

pub fn active_check(lines: &[usize], top_line: usize) -> Option<usize> {
    if lines.is_empty() {
        None
    } else {
        Some(
            lines
                .partition_point(|line| *line <= top_line)
                .saturating_sub(1),
        )
    }
}
pub fn migrate_check_markers(source: &str, regions: &[ResponseRegion]) -> String {
    source
        .split_inclusive('\n')
        .enumerate()
        .map(|(index, line)| {
            if marker(line, "request")
                && !regions
                    .iter()
                    .any(|region| index >= region.header && index < region.end)
            {
                let start = line.len() - line.trim_start().len();
                format!(
                    "{}#check{}",
                    &line[..start],
                    &line[start + "#request".len()..]
                )
            } else {
                line.to_owned()
            }
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
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
        let mut history = EditHistory {
            scope: "workspace:rest".into(),
            ..Default::default()
        };
        let source = "#check first\n#body\n{}\n#check second\n#body\n{}";
        history.synchronize(source, &[], 0);
        let first = history.checks[0].id.clone();
        let second = history.checks[1].id.clone();
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
        let fragment = clipboard.as_ref().unwrap().text.clone();
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
        assert_eq!(reloaded.checks, moved.checks);
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
        let start = offset(&history.text, &from).unwrap();
        let end = offset(&history.text, &to).unwrap();
        let removed = history.text[start..end]
            .split('\n')
            .map(str::to_owned)
            .collect();
        let text = format!(
            "{}{}{}",
            &history.text[..start],
            inserted,
            &history.text[end..]
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
        let ids = history.checks.clone();
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
            fragment.pasted_regions(&fragment.text),
            Some(fragment.regions.as_slice())
        );
        assert!(
            fragment
                .pasted_regions(&format!("\n{}", fragment.text))
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
            clipboard.unwrap().regions[0].table
        );
    }
    #[test]
    fn exact_cut_and_paste_does_not_expand_the_previous_response() {
        let mut history = editing_fixture();
        let original = history.text.clone();
        let mut clipboard = None;
        let moved_line = check_lines(&history.text, &history.regions)[2];
        let last = history.text.split('\n').count() - 1;
        let last_ch = history
            .text
            .split('\n')
            .next_back()
            .unwrap()
            .encode_utf16()
            .count();
        let moved = history.text[offset(
            &history.text,
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
        let boundary = history.regions[0].end;
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
            crate::console::parse("rest", &block.text).unwrap()["url"],
            "https://fixture.invalid/moved"
        );
        let source = history.text.clone();
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
        let header = history.regions[0].header;
        let before = history.regions.clone();
        let original = history.text.clone();
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
        let first_updated =
            replace_response(&redone.text, 0, "OK", "new first output", &mut regions);
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
                            line: history.regions[0].end,
                            ch: 0,
                        },
                    )
                    .unwrap(),
            );
            let fragment = clipboard.as_ref().unwrap().text.clone();
            let end = history.regions[0].end;
            if cut {
                edit(&mut history, (0, 0), (end, 0), "", "cut", 2, &mut clipboard);
            }
            let boundary = check_lines(&history.text, &history.regions)[if cut { 1 } else { 2 }];
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
        let previous = history.text.clone();
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
        assert_eq!(history.text, previous);
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
            assert_eq!(crate::console::request_at(source, line).unwrap(), block);
        }
        for line in ["#check", " #CHECK name", "#request", "\t#REQUEST name"] {
            assert!(crate::console::is_check(line));
        }
        for line in ["#checker", "#requestSuffix", "echo #check", "\"#request\""] {
            assert!(!crate::console::is_check(line));
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
        regions[0].table = Some(crate::tables::Metadata::from_result(
            &serde_json::json!({"columns":["feed"],"columnTypes":{"feed":"json"}}),
        ));
        let new = format!("\n{text}");
        remap(&text, &new, &mut regions);
        assert_eq!(regions[0].request, 1);
        let json = serde_json::to_string(&regions).unwrap();
        let loaded: Vec<ResponseRegion> = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded[0].table, regions[0].table);
        let fragment = clipboard_fragment(
            &new,
            &loaded,
            &Position { line: 1, ch: 0 },
            &Position {
                line: new.split('\n').count() - 1,
                ch: 0,
            },
        )
        .unwrap();
        assert_eq!(fragment.regions[0].table, loaded[0].table);
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
}
