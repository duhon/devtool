//! A tolerant parser of the `#payload` JSON into canonical Extended JSON v2: comments,
//! trailing commas, unquoted keys and single quotes are accepted; numbers become
//! `$numberInt`, `$numberLong` or `$numberDouble`; Extended JSON (`{"$oid": "…"}`) passes
//! through as written.

use serde_json::{Map, Value, json};
use std::fmt;

/// A parse error with its 1-based line and column in the parsed text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub message: String,
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (line {}, column {})",
            self.message, self.line, self.column
        )
    }
}

/// Keys whose objects hold plain JSON (Extended JSON passes through as written).
const EXTENDED: [&str; 17] = [
    "$oid",
    "$date",
    "$numberInt",
    "$numberLong",
    "$numberDouble",
    "$numberDecimal",
    "$binary",
    "$uuid",
    "$timestamp",
    "$regularExpression",
    "$minKey",
    "$maxKey",
    "$symbol",
    "$code",
    "$scope",
    "$dbPointer",
    "$undefined",
];

pub fn locate(source: &str, pos: usize) -> (usize, usize) {
    let before = &source[..pos.min(source.len())];
    let line = before.matches('\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |at| at + 1);
    (line, before[start..].chars().count() + 1)
}

pub fn error_at(source: &str, pos: usize, message: impl Into<String>) -> Error {
    let (line, column) = locate(source, pos);
    Error {
        message: message.into(),
        line,
        column,
    }
}

pub struct Parser<'a> {
    src: &'a str,
    pos: usize,
    /// Inside an Extended JSON object: numbers stay plain JSON.
    raw: u32,
}

/// Parses `source`, one value, into canonical Extended JSON.
pub fn parse(source: &str) -> Result<Value, Error> {
    let mut parser = Parser {
        src: source,
        pos: 0,
        raw: 0,
    };
    let value = parser.value()?;
    parser.finish()?;
    Ok(value)
}

impl<'a> Parser<'a> {
    fn error(&self, pos: usize, message: impl Into<String>) -> Error {
        error_at(self.src, pos, message)
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    /// Skips whitespace and comments.
    fn skip(&mut self) -> Result<(), Error> {
        loop {
            let rest = self.rest();
            let trimmed = rest.trim_start();
            self.pos += rest.len() - trimmed.len();
            if trimmed.starts_with("//") {
                self.pos += trimmed.find('\n').unwrap_or(trimmed.len());
            } else if trimmed.starts_with("/*") {
                let end = trimmed
                    .find("*/")
                    .ok_or_else(|| self.error(self.pos, "Unterminated comment"))?;
                self.pos += end + 2;
            } else {
                return Ok(());
            }
        }
    }

    /// The next character after whitespace and comments.
    fn peek(&mut self) -> Result<Option<char>, Error> {
        self.skip()?;
        Ok(self.rest().chars().next())
    }

    fn eat(&mut self, c: char) -> Result<bool, Error> {
        if self.peek()? == Some(c) {
            self.pos += c.len_utf8();
            return Ok(true);
        }
        Ok(false)
    }

    fn expect(&mut self, c: char, context: &str) -> Result<(), Error> {
        if self.eat(c)? {
            return Ok(());
        }
        Err(self.unexpected(&format!("Expected '{c}' {context}")))
    }

    fn unexpected(&mut self, expected: &str) -> Error {
        let found = match self.peek() {
            Ok(Some(c)) => format!("found '{c}'"),
            Ok(None) => "reached the end".into(),
            Err(error) => return error,
        };
        self.error(self.pos, format!("{expected}, {found}"))
    }

    fn ident(&mut self) -> Result<Option<(usize, &'a str)>, Error> {
        self.skip()?;
        let rest = self.rest();
        let mut chars = rest.char_indices();
        match chars.next() {
            Some((_, c)) if c.is_alphabetic() || c == '_' || c == '$' => {}
            _ => return Ok(None),
        }
        let len = chars
            .find(|(_, c)| !(c.is_alphanumeric() || *c == '_' || *c == '$'))
            .map_or(rest.len(), |(at, _)| at);
        let start = self.pos;
        self.pos += len;
        Ok(Some((start, &rest[..len])))
    }

    fn finish(&mut self) -> Result<(), Error> {
        match self.peek()? {
            None => Ok(()),
            Some(c) => Err(self.error(self.pos, format!("Unexpected '{c}' after the payload"))),
        }
    }

    fn value(&mut self) -> Result<Value, Error> {
        let Some(c) = self.peek()? else {
            return Err(self.error(self.pos, "A value is expected, found the end"));
        };
        match c {
            '{' => self.object(),
            '[' => self.array(),
            '"' | '\'' => self.string().map(Value::String),
            '-' | '+' | '.' | '0'..='9' => self.number(),
            c if c.is_alphabetic() || c == '_' => self.word(),
            c => Err(self.error(self.pos, format!("Unexpected '{c}'"))),
        }
    }

    fn object(&mut self) -> Result<Value, Error> {
        self.pos += 1;
        let mut map = Map::new();
        loop {
            if self.eat('}')? {
                return Ok(Value::Object(map));
            }
            let key = self.key()?;
            self.expect(':', "after a key")?;
            let extended = EXTENDED.contains(&key.as_str());
            self.raw += u32::from(extended);
            let value = self.value();
            self.raw -= u32::from(extended);
            let mut value = value?;
            if key == "$date"
                && let Some(ms) = value.as_i64()
            {
                value = json!({"$numberLong": ms.to_string()});
            }
            map.insert(key, value);
            if !self.eat(',')? && self.peek()? != Some('}') {
                return Err(self.unexpected("Expected ',' or '}'"));
            }
        }
    }

    fn key(&mut self) -> Result<String, Error> {
        match self.peek()? {
            Some('"' | '\'') => self.string(),
            Some('0'..='9') => {
                let rest = self.rest();
                let len = rest
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len());
                self.pos += len;
                Ok(rest[..len].into())
            }
            _ => match self.ident()? {
                Some((_, name)) => Ok(name.into()),
                None => Err(self.unexpected("Expected a key")),
            },
        }
    }

    fn array(&mut self) -> Result<Value, Error> {
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            if self.eat(']')? {
                return Ok(Value::Array(items));
            }
            items.push(self.value()?);
            if !self.eat(',')? && self.peek()? != Some(']') {
                return Err(self.unexpected("Expected ',' or ']'"));
            }
        }
    }

    /// A quoted string with escapes.
    fn string(&mut self) -> Result<String, Error> {
        self.skip()?;
        let start = self.pos;
        let quote = self.rest().chars().next().unwrap_or('"');
        self.pos += 1;
        let mut out = String::new();
        let mut chars = self.rest().char_indices();
        while let Some((at, c)) = chars.next() {
            if c == quote {
                self.pos += at + 1;
                return Ok(out);
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            let Some((_, escape)) = chars.next() else {
                break;
            };
            match escape {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'v' => out.push('\u{b}'),
                '0' => out.push('\0'),
                '\n' => {}
                'x' | 'u' => {
                    let width = if escape == 'x' { 2 } else { 4 };
                    let digits: String = chars.by_ref().take(width).map(|(_, c)| c).collect();
                    let code = u32::from_str_radix(&digits, 16)
                        .ok()
                        .filter(|_| digits.len() == width)
                        .ok_or_else(|| self.error(self.pos + at, "Invalid escape sequence"))?;
                    out.push(self.character(code, &mut chars, at)?);
                }
                other => out.push(other),
            }
        }
        Err(self.error(start, "Unterminated string"))
    }

    /// A `\u` code point; a high surrogate takes the low surrogate escape after it.
    fn character(
        &self,
        code: u32,
        chars: &mut std::str::CharIndices<'_>,
        at: usize,
    ) -> Result<char, Error> {
        let invalid = || self.error(self.pos + at, "Invalid escape sequence");
        if !(0xD800..0xDC00).contains(&code) {
            return char::from_u32(code).ok_or_else(invalid);
        }
        let mut ahead = chars.clone();
        let low = match (ahead.next(), ahead.next()) {
            (Some((_, '\\')), Some((_, 'u'))) => {
                let digits: String = ahead.by_ref().take(4).map(|(_, c)| c).collect();
                u32::from_str_radix(&digits, 16).ok()
            }
            _ => None,
        }
        .filter(|low| (0xDC00..0xE000).contains(low))
        .ok_or_else(invalid)?;
        *chars = ahead;
        char::from_u32(0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00)).ok_or_else(invalid)
    }

    fn number(&mut self) -> Result<Value, Error> {
        let start = self.pos;
        let rest = self.rest();
        let negative = rest.starts_with('-');
        let sign = usize::from(rest.starts_with(['-', '+']));
        let len = number_len(&rest[sign..]);
        let text = &rest[sign..sign + len];
        let end = start + sign + len;
        let integer = text
            .parse::<i128>()
            .ok()
            .map(|n| if negative { -n } else { n });
        let float = text
            .parse::<f64>()
            .ok()
            .map(|n| if negative { -n } else { n });
        if float.is_none() || self.src[end..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
        {
            return Err(self.error(start, "Invalid number"));
        }
        self.pos = end;
        let float = float.unwrap_or(0.0);
        if self.raw > 0 {
            return Ok(match integer.and_then(|n| i64::try_from(n).ok()) {
                Some(n) => json!(n),
                None => json!(float),
            });
        }
        match integer {
            Some(n) => int(n, start, self),
            None => Ok(double(float)),
        }
    }

    fn word(&mut self) -> Result<Value, Error> {
        let (start, name) = self.ident()?.expect("a word starts here");
        match name {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            "null" => Ok(Value::Null),
            _ => Err(self.error(
                start,
                format!("Unknown word '{name}'; values are strings, numbers, true, false, null, arrays and objects"),
            )),
        }
    }
}

fn int(n: i128, start: usize, parser: &Parser<'_>) -> Result<Value, Error> {
    if let Ok(n) = i32::try_from(n) {
        return Ok(json!({"$numberInt": n.to_string()}));
    }
    if let Ok(n) = i64::try_from(n) {
        return Ok(json!({"$numberLong": n.to_string()}));
    }
    Err(parser.error(start, "The number is out of the 64-bit integer range; write it as a decimal with a fraction or as {\"$numberDecimal\": \"…\"}"))
}

fn number_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    let digits = |from: usize| {
        from + bytes[from..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count()
    };
    let mut end = digits(0);
    if bytes.get(end) == Some(&b'.') {
        end = digits(end + 1);
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let sign = end + 1 + usize::from(matches!(bytes.get(end + 1), Some(b'+' | b'-')));
        if digits(sign) > sign {
            end = digits(sign);
        }
    }
    end
}

pub fn double(n: f64) -> Value {
    let text = if n.is_nan() {
        "NaN".to_owned()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
    } else if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{n:.1}")
    } else if n.abs() >= 1e15 || n.abs() < 1e-5 {
        format!("{n:e}")
    } else {
        n.to_string()
    };
    json!({"$numberDouble": text})
}

/// A number from a plain or Extended JSON value.
pub fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::Object(map) if map.len() == 1 => {
            let (key, inner) = map.iter().next()?;
            match key.as_str() {
                "$numberInt" | "$numberLong" | "$numberDouble" => inner.as_str()?.parse().ok(),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The integer of a whole-number Extended JSON value.
pub fn as_i64(value: &Value) -> Option<i64> {
    if let Value::Object(map) = value
        && let Some(Value::String(text)) = map.get("$numberInt").or_else(|| map.get("$numberLong"))
    {
        return text.parse().ok();
    }
    as_f64(value)
        .filter(|n| n.fract() == 0.0 && n.abs() < 9.3e18)
        .map(|n| n as i64)
}

/// The civil date `(year, month, day)` of days since 1970-01-01.
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let of_era = z - era * 146_097;
    let of_year = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let day_of_year = of_era - (365 * of_year + of_year / 4 - of_year / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = of_year + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `2024-03-01T10:20:30.123Z` of milliseconds since 1970.
pub fn iso_date(ms: i64) -> String {
    let (year, month, day) = civil_from_days(ms.div_euclid(86_400_000));
    let of_day = ms.rem_euclid(86_400_000);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        of_day / 3_600_000,
        of_day / 60_000 % 60,
        of_day / 1000 % 60,
        of_day % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fails(source: &str) -> Error {
        parse(source).unwrap_err()
    }

    #[test]
    fn plain_values_become_canonical_extended_json() {
        assert_eq!(
            parse(r#"{"a": 1, "b": 5000000000, "c": 1.5, "d": 1e3, "e": -2, "f": "x", "g": [true, null]}"#).unwrap(),
            json!({"a": {"$numberInt": "1"}, "b": {"$numberLong": "5000000000"},
                "c": {"$numberDouble": "1.5"}, "d": {"$numberDouble": "1000.0"},
                "e": {"$numberInt": "-2"}, "f": "x", "g": [true, null]})
        );
    }

    #[test]
    fn relaxed_syntax_is_tolerated() {
        let source =
            "{\n  // a comment\n  unquoted: 'single',\n  /* block */ \"list\": [1, 2,],\n}";
        assert_eq!(
            parse(source).unwrap(),
            json!({"unquoted": "single", "list": [{"$numberInt": "1"}, {"$numberInt": "2"}]})
        );
        assert_eq!(parse(r#""a\n\u00e9""#).unwrap(), json!("a\né"));
    }

    #[test]
    fn extended_json_passes_through() {
        let source = r#"{"_id": {"$oid": "65f0c0ffee0000000000000a"}, "at": {"$date": 1700000000000},
            "n": {"$numberLong": "7"}, "d": {"$date": "2024-01-01T00:00:00Z"}}"#;
        let value = parse(source).unwrap();
        assert_eq!(value["_id"], json!({"$oid": "65f0c0ffee0000000000000a"}));
        assert_eq!(
            value["at"],
            json!({"$date": {"$numberLong": "1700000000000"}})
        );
        assert_eq!(value["n"], json!({"$numberLong": "7"}));
        assert_eq!(value["d"], json!({"$date": "2024-01-01T00:00:00Z"}));
    }

    #[test]
    fn errors_have_lines_and_columns() {
        let error = fails("{\n  a: 1,\n  b: foo\n}");
        assert_eq!((error.line, error.column), (3, 6));
        assert!(error.message.starts_with("Unknown word 'foo'"));
        assert_eq!((fails("{a 1}").line, fails("{a 1}").column), (1, 4));
        assert_eq!(
            fails("{a: 1").message,
            "Expected ',' or '}', reached the end"
        );
        assert_eq!(fails("1 2").message, "Unexpected '2' after the payload");
        assert_eq!(fails("/* open").message, "Unterminated comment");
        assert_eq!(fails("\"open").message, "Unterminated string");
        assert!(fails("ObjectId(\"x\")").message.contains("Unknown word"));
        assert!(
            fails("12345678901234567890")
                .message
                .starts_with("The number is out of")
        );
    }
}
