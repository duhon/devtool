//! Line text helpers. Positions count UTF-16 code units, as the browser and the
//! language code do.
use serde::{Deserialize, Serialize};

/// A place in the text: a line and a UTF-16 column in it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub ch: usize,
}

/// One change an edit made: `removed` lines between `from` and `to` became `text`.
#[derive(Clone, Debug, Deserialize)]
pub struct TextChange {
    pub from: Position,
    pub to: Position,
    pub text: Vec<String>,
    pub removed: Vec<String>,
    pub origin: Option<String>,
}

pub const TAB_SIZE: usize = 4;
pub const INDENT_UNIT: usize = 2;

pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The byte offset of UTF-16 column `ch`; inside a surrogate pair it rounds down.
pub fn byte_at(text: &str, ch: usize) -> usize {
    let mut units = 0;
    for (index, c) in text.char_indices() {
        if units >= ch {
            return index;
        }
        units += c.len_utf16();
        if units > ch {
            return index;
        }
    }
    text.len()
}

/// [`byte_at`] for increasing columns of one text, in a single pass over it.
pub struct Utf16Cursor<'a> {
    text: &'a str,
    byte: usize,
    ch: usize,
}

impl<'a> Utf16Cursor<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            byte: 0,
            ch: 0,
        }
    }

    /// As [`byte_at`]; a column before the previous one restarts from the line start.
    pub fn byte_at(&mut self, ch: usize) -> usize {
        if ch < self.ch {
            (self.byte, self.ch) = (0, 0);
        }
        for c in self.text[self.byte..].chars() {
            let width = c.len_utf16();
            if self.ch + width > ch {
                break;
            }
            self.ch += width;
            self.byte += c.len_utf8();
        }
        self.byte
    }
}

/// The UTF-16 column of byte offset `byte`.
pub fn ch_at(text: &str, byte: usize) -> usize {
    text.char_indices()
        .take_while(|(index, _)| *index < byte)
        .map(|(_, c)| c.len_utf16())
        .sum()
}

/// Splits inserted text into lines; `\r\n` and `\r` count as line breaks.
pub fn split_lines(text: &str) -> Vec<String> {
    if !text.contains('\r') {
        return text.split('\n').map(str::to_owned).collect();
    }
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(str::to_owned)
        .collect()
}

/// The display column of byte `end` (tabs expand to [`TAB_SIZE`]).
pub fn column(text: &str, end: usize) -> usize {
    let mut column = 0;
    for c in text[..end.min(text.len())].chars() {
        column += if c == '\t' {
            TAB_SIZE - column % TAB_SIZE
        } else {
            1
        };
    }
    column
}

/// The byte offset of display column `goal`, rounding to the nearest character.
pub fn byte_at_column(text: &str, goal: usize) -> usize {
    let mut column = 0;
    for (index, c) in text.char_indices() {
        let width = if c == '\t' {
            TAB_SIZE - column % TAB_SIZE
        } else {
            1
        };
        if column + width > goal {
            return if goal - column > width / 2 {
                index + c.len_utf8()
            } else {
                index
            };
        }
        column += width;
    }
    text.len()
}

/// The display column of the first non-whitespace character.
pub fn indentation(text: &str) -> usize {
    column(text, leading_whitespace(text))
}

pub fn leading_whitespace(text: &str) -> usize {
    text.char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map_or(text.len(), |(index, _)| index)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CharClass {
    Space,
    Word,
    Punctuation,
}

pub fn class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

pub fn min(a: Position, b: Position) -> Position {
    if (a.line, a.ch) <= (b.line, b.ch) {
        a
    } else {
        b
    }
}

pub fn max(a: Position, b: Position) -> Position {
    if (a.line, a.ch) >= (b.line, b.ch) {
        a
    } else {
        b
    }
}

pub fn before(a: Position, b: Position) -> bool {
    (a.line, a.ch) < (b.line, b.ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_columns_round_down_inside_surrogate_pairs() {
        let text = "a😀b";
        assert_eq!(utf16_len(text), 4);
        assert_eq!(byte_at(text, 1), 1);
        assert_eq!(byte_at(text, 2), 1);
        assert_eq!(byte_at(text, 3), 5);
        assert_eq!(byte_at(text, 9), text.len());
        assert_eq!(ch_at(text, 5), 3);
    }

    #[test]
    fn a_cursor_finds_the_bytes_byte_at_does() {
        let text = "a😀bé😀";
        let mut cursor = Utf16Cursor::new(text);
        for ch in [0, 1, 2, 2, 3, 4, 5, 6, 7, 9, 3, 0, 8] {
            assert_eq!(cursor.byte_at(ch), byte_at(text, ch), "column {ch}");
        }
    }

    #[test]
    fn columns_expand_tabs() {
        assert_eq!(column("\tab", 1), 4);
        assert_eq!(column("a\tb", 2), 4);
        assert_eq!(byte_at_column("\tab", 5), 2);
        assert_eq!(indentation("  \tx"), 4);
        assert_eq!(split_lines("a\r\nb\rc"), ["a", "b", "c"]);
    }
}
