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
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn regions(&self) -> &[ResponseRegion] {
        &self.regions
    }
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
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn regions(&self) -> &[ResponseRegion] {
        &self.regions
    }
    pub fn checks(&self) -> &[crate::checks::Identity] {
        &self.checks
    }
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
