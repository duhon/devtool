//! STOMP frames: encoding and incremental decoding with 1.1/1.2 header escaping.

/// Frames larger than this are a broken stream, not a message to show.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub command: String,
    /// In wire order; a repeated header keeps its first value (STOMP 1.2).
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Frame {
    pub fn new(command: &str) -> Self {
        Self {
            command: command.into(),
            headers: vec![],
            body: vec![],
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// `CONNECT`/`CONNECTED` and STOMP 1.0 frames carry raw header values.
    pub fn encode(&self, escapes: bool) -> Vec<u8> {
        let escapes = escapes && !raw(&self.command);
        let mut out = Vec::with_capacity(self.body.len() + 64);
        out.extend_from_slice(self.command.as_bytes());
        out.push(b'\n');
        for (name, value) in &self.headers {
            let (name, value) = if escapes {
                (escape(name), escape(value))
            } else {
                (name.clone(), value.clone())
            };
            out.extend_from_slice(name.as_bytes());
            out.push(b':');
            out.extend_from_slice(value.as_bytes());
            out.push(b'\n');
        }
        out.push(b'\n');
        out.extend_from_slice(&self.body);
        out.push(0);
        out
    }
}

fn raw(command: &str) -> bool {
    command == "CONNECT" || command == "CONNECTED"
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            ':' => out.push_str("\\c"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(value: &str) -> Result<String, String> {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match chars.next() {
            Some('\\') => '\\',
            Some('n') => '\n',
            Some('r') => '\r',
            Some('c') => ':',
            other => {
                return Err(format!(
                    "Invalid STOMP header escape \\{}",
                    other.map(String::from).unwrap_or_default()
                ));
            }
        });
    }
    Ok(out)
}

/// Bytes read from the broker, cut into frames; heart-beat EOLs between frames are skipped.
pub struct Decoder {
    buffer: Vec<u8>,
    pub escapes: bool,
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            buffer: vec![],
            escapes: true,
        }
    }
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The next complete frame; `None` until it has fully arrived.
    pub fn frame(&mut self) -> Result<Option<Frame>, String> {
        let eols = self
            .buffer
            .iter()
            .take_while(|b| **b == b'\n' || **b == b'\r')
            .count();
        self.buffer.drain(..eols);
        if self.buffer.is_empty() {
            return Ok(None);
        }
        let mut lines = vec![];
        let mut pos = 0;
        let body_start = loop {
            let Some(end) = self.buffer[pos..].iter().position(|b| *b == b'\n') else {
                return self.incomplete();
            };
            let line = &self.buffer[pos..pos + end];
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            pos += end + 1;
            if line.is_empty() {
                break pos;
            }
            lines.push(
                String::from_utf8(line.to_vec())
                    .map_err(|_| "STOMP frame headers are not UTF-8".to_string())?,
            );
        };
        let command = lines.remove(0);
        let escapes = self.escapes && !raw(&command);
        let mut headers = Vec::with_capacity(lines.len());
        for line in lines {
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| format!("Invalid STOMP header line {line:?}"))?;
            headers.push(if escapes {
                (unescape(name)?, unescape(value)?)
            } else {
                (name.to_owned(), value.to_owned())
            });
        }
        let length = headers
            .iter()
            .find(|(name, _)| name == "content-length")
            .map(|(_, value)| {
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| format!("Invalid STOMP content-length {value:?}"))
            })
            .transpose()?;
        let (body_end, frame_end) = match length {
            Some(length) => {
                if length > MAX_FRAME_BYTES {
                    return Err(too_large());
                }
                if self.buffer.len() <= body_start + length {
                    return self.incomplete();
                }
                if self.buffer[body_start + length] != 0 {
                    return Err("STOMP frame body does not end with NUL".into());
                }
                (body_start + length, body_start + length + 1)
            }
            None => match self.buffer[body_start..].iter().position(|b| *b == 0) {
                Some(end) => (body_start + end, body_start + end + 1),
                None => return self.incomplete(),
            },
        };
        let body = self.buffer[body_start..body_end].to_vec();
        self.buffer.drain(..frame_end);
        Ok(Some(Frame {
            command,
            headers,
            body,
        }))
    }

    fn incomplete(&self) -> Result<Option<Frame>, String> {
        if self.buffer.len() > MAX_FRAME_BYTES {
            Err(too_large())
        } else {
            Ok(None)
        }
    }
}

fn too_large() -> String {
    format!("STOMP frame exceeds {} MiB", MAX_FRAME_BYTES / 1024 / 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(bytes: &[u8]) -> Vec<Frame> {
        let mut decoder = Decoder::default();
        let mut frames = vec![];
        for byte in bytes {
            decoder.push(&[*byte]);
            while let Some(frame) = decoder.frame().unwrap() {
                frames.push(frame);
            }
        }
        frames
    }

    #[test]
    fn escaped_headers_round_trip_byte_by_byte_between_heart_beats() {
        let sent = Frame::new("MESSAGE")
            .header("destination", "orders")
            .header("note", "a:b\\c\nd\re")
            .header("note", "second wins never");
        let mut wire = b"\n\r\n".to_vec();
        wire.extend(sent.encode(true));
        wire.extend(b"\n");
        wire.extend(Frame::new("RECEIPT").header("receipt-id", "1").encode(true));
        let frames = decode_all(&wire);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], sent);
        assert_eq!(frames[0].get("note"), Some("a:b\\c\nd\re"));
        assert_eq!(frames[1].get("receipt-id"), Some("1"));
    }

    #[test]
    fn content_length_bodies_may_contain_nul_and_crlf_lines_are_accepted() {
        let wire = b"MESSAGE\r\ncontent-length:3\r\n\r\na\0b\0";
        let frames = decode_all(wire);
        assert_eq!(frames[0].body, b"a\0b");
        let mut decoder = Decoder::default();
        decoder.push(b"MESSAGE\ncontent-length:1\n\nab\0");
        assert_eq!(
            decoder.frame().unwrap_err(),
            "STOMP frame body does not end with NUL"
        );
    }

    #[test]
    fn connect_frames_and_stomp_1_0_keep_raw_values() {
        let connect = Frame::new("CONNECT").header("passcode", "a:b\\");
        assert_eq!(connect.encode(true), b"CONNECT\npasscode:a:b\\\n\n\0");
        let send = Frame::new("SEND").header("x", "a:b");
        assert_eq!(send.encode(false), b"SEND\nx:a:b\n\n\0");
        let mut decoder = Decoder {
            escapes: false,
            ..Decoder::default()
        };
        decoder.push(b"MESSAGE\npath:C:\\temp\n\nx\0");
        assert_eq!(
            decoder.frame().unwrap().unwrap().get("path"),
            Some("C:\\temp")
        );
        let mut decoder = Decoder::default();
        decoder.push(b"MESSAGE\npath:C:\\temp\n\nx\0");
        assert_eq!(
            decoder.frame().unwrap_err(),
            "Invalid STOMP header escape \\t"
        );
    }
}
