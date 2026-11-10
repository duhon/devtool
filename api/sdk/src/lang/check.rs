//! Where a position in a check is: its line, the section it is in and the section's lines;
//! the completion of section markers.

use super::{finish, item, marker};
use crate::{Completions, Item};

/// A position in a check's text.
#[derive(Debug)]
pub struct At<'a> {
    pub text: &'a str,
    /// The byte position, on a char boundary.
    pub pos: usize,
    /// Every line: its byte start and text.
    pub lines: Vec<(usize, &'a str)>,
    /// The index of the position's line.
    pub line: usize,
    /// The line's text before the position.
    pub before: &'a str,
    /// The markers of the other lines: line index and lowercased name.
    pub markers: Vec<(usize, String)>,
    /// The marker line of the section the position is in, and its name.
    pub section: Option<(usize, String)>,
    /// The line after the section, the next marker or the end.
    pub section_end: usize,
}

impl<'a> At<'a> {
    pub fn new(text: &'a str, pos: usize) -> Self {
        let mut pos = pos.min(text.len());
        while !text.is_char_boundary(pos) {
            pos -= 1;
        }
        let mut start = 0;
        let lines: Vec<(usize, &str)> = text
            .split('\n')
            .map(|line| {
                let at = start;
                start += line.len() + 1;
                (at, line)
            })
            .collect();
        let line = lines.iter().rposition(|(at, _)| *at <= pos).unwrap_or(0);
        let (line_start, full) = lines[line];
        let markers: Vec<_> = lines
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != line)
            .filter_map(|(i, (_, text))| Some((i, marker(text)?.0)))
            .collect();
        let section = markers.iter().rev().find(|(i, _)| *i < line).cloned();
        let section_end = markers
            .iter()
            .find(|(i, _)| *i > line)
            .map_or(lines.len(), |(i, _)| *i);
        Self {
            text,
            pos,
            before: &full[..pos - line_start],
            lines,
            line,
            markers,
            section,
            section_end,
        }
    }

    /// The text of line `i`, empty past the end.
    pub fn line_text(&self, i: usize) -> &'a str {
        self.lines.get(i).map_or("", |(_, text)| text)
    }
    /// The byte start of line `i`, the text's end past the last line.
    pub fn line_start(&self, i: usize) -> usize {
        self.lines.get(i).map_or(self.text.len(), |(at, _)| *at)
    }
    /// The whole line of the position.
    pub fn full(&self) -> &'a str {
        self.line_text(self.line)
    }
    /// The section's name, if any.
    pub fn name(&self) -> Option<&str> {
        self.section.as_ref().map(|(_, name)| name.as_str())
    }
    /// The first line of the section's body.
    pub fn body_start(&self) -> usize {
        self.section.as_ref().map_or(0, |(line, _)| line + 1)
    }
    /// The section's body up to the position.
    pub fn body(&self) -> &'a str {
        let start = self.line_start(self.body_start()).min(self.pos);
        &self.text[start..self.pos]
    }
    /// The section's whole body.
    pub fn whole(&self) -> &'a str {
        let start = self.line_start(self.body_start()).min(self.pos);
        let end = self
            .lines
            .get(self.section_end)
            .map_or(self.text.len(), |(at, _)| at - 1)
            .max(start);
        &self.text[start..end]
    }
    /// The other lines of the section's body.
    pub fn other_lines(&self) -> impl Iterator<Item = &'a str> + '_ {
        (self.body_start()..self.section_end)
            .filter(|i| *i != self.line)
            .map(|i| self.line_text(i))
    }
    /// After a `#responce` marker: the response is not completed.
    pub fn in_response(&self) -> bool {
        self.markers
            .iter()
            .any(|(i, name)| *i < self.line && matches!(name.as_str(), "responce" | "response"))
    }
    /// Inside an open `{{…}}`: completion is the App's.
    pub fn in_placeholder(&self) -> bool {
        self.before
            .rfind("{{")
            .is_some_and(|start| !self.before[start..].contains("}}"))
    }
    /// Nothing to complete at the position.
    pub fn none(&self) -> Completions {
        finish(self.pos, self.pos, "", vec![])
    }
    /// Markers of `sections` the check does not have yet.
    pub fn unused(&self, sections: &[&str]) -> Vec<Item> {
        sections
            .iter()
            .filter(|name| !self.markers.iter().any(|(_, used)| used == *name))
            .map(|name| item(format!("#{name}\n"), "Section", "section"))
            .collect()
    }
    /// Marker completion when a marker is being typed, or on a blank line before any
    /// section.
    pub fn markers(&self, sections: &[&str]) -> Option<Completions> {
        let before = self.before;
        if !(before.trim_start().starts_with('#')
            || (before.trim().is_empty() && self.section.is_none()))
        {
            return None;
        }
        let full = self.full();
        if before.trim().is_empty() && !full.trim().is_empty() {
            return Some(self.none());
        }
        let line_start = self.line_start(self.line);
        let offset = before.len() - before.trim_start().len();
        let end = full[offset..]
            .find(char::is_whitespace)
            .map_or(full.len(), |length| offset + length);
        Some(finish(
            line_start + offset,
            line_start + end,
            before.trim(),
            self.unused(sections),
        ))
    }
    /// Unused markers when a section is over: a blank line after body lines.
    pub fn next_section(&self, sections: &[&str]) -> Option<Completions> {
        let filled = (self.body_start()..self.line).any(|i| !self.line_text(i).trim().is_empty());
        (self.before.trim().is_empty() && filled).then(|| {
            finish(
                self.line_start(self.line),
                self.pos,
                "",
                self.unused(sections),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_know_their_section_and_body() {
        let text = "#target\nmysql://db\n#payload\nSELECT 1\nFROM t\n#responce OK\nx";
        let at = At::new(text, text.find("FROM").unwrap() + 2);
        assert_eq!(at.name(), Some("payload"));
        assert_eq!(at.before, "FR");
        assert_eq!(at.body(), "SELECT 1\nFR");
        assert_eq!(at.whole(), "SELECT 1\nFROM t");
        assert!(!at.in_response());
        assert!(At::new(text, text.len()).in_response());
        let at = At::new("#target\nx\n#", 13);
        let markers = at.markers(&["target", "payload"]).unwrap();
        assert_eq!(markers.items.len(), 1);
        assert_eq!(markers.items[0].insert, "#payload\n");
    }
}
