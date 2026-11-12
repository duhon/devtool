use crate::config::RESPONSE_LIMIT;
use serde_json::{Value, json};
use services_sdk::driver;

#[derive(Clone, Debug, PartialEq)]
pub enum Resp {
    Bytes(Vec<u8>),
    Int(i64),
    Null,
    Array(Vec<Resp>),
    Error(String),
}

impl Resp {
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Bytes(bytes) => std::str::from_utf8(bytes).ok(),
            _ => None,
        }
    }

    pub fn array(&self) -> Option<&[Resp]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn json(&self) -> Value {
        match self {
            Self::Bytes(bytes) => match std::str::from_utf8(bytes) {
                Ok(text) => json!(text),
                Err(_) => json!({"encoding": "base64", "data": driver::encode(bytes)}),
            },
            Self::Int(value) => json!(value),
            Self::Null => Value::Null,
            Self::Array(items) => Value::Array(items.iter().map(Self::json).collect()),
            Self::Error(error) => json!({"error": error}),
        }
    }

    pub fn display(&self) -> String {
        self.text()
            .map(str::to_owned)
            .unwrap_or_else(|| serde_json::to_string_pretty(&self.json()).unwrap())
    }
}

pub fn encode(args: &[String]) -> Vec<u8> {
    let mut bytes = format!("*{}\r\n", args.len()).into_bytes();
    for arg in args {
        bytes.extend_from_slice(format!("${}\r\n", arg.len()).as_bytes());
        bytes.extend_from_slice(arg.as_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    bytes
}

#[derive(Default)]
pub struct Decoder {
    bytes: Vec<u8>,
    arrays: Vec<(usize, Vec<Resp>)>,
    frame_bytes: usize,
    nodes: usize,
}

impl Decoder {
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<Resp>, String> {
        if self.bytes.len().saturating_add(chunk.len()) > RESPONSE_LIMIT * 2 {
            return Err("RESP response exceeds the 16 MiB limit".into());
        }
        self.bytes.extend_from_slice(chunk);
        let mut used = 0;
        let mut frames = Vec::new();
        while let Some((part, end)) = parse(&self.bytes, used)? {
            self.frame_bytes += end - used;
            self.nodes += 1;
            used = end;
            if self.frame_bytes > RESPONSE_LIMIT || self.nodes > 100_000 {
                return Err("RESP response exceeds the 16 MiB / 100000 element limit".into());
            }
            let mut item = match part {
                Part::Array(len) if len > 0 => {
                    if self.arrays.len() >= 64 {
                        return Err("RESP nesting exceeds 64 levels".into());
                    }
                    self.arrays.push((len, Vec::new()));
                    continue;
                }
                Part::Array(_) => Resp::Array(Vec::new()),
                Part::Value(value) => value,
            };
            loop {
                let Some((remaining, items)) = self.arrays.last_mut() else {
                    frames.push(item);
                    self.frame_bytes = 0;
                    self.nodes = 0;
                    break;
                };
                items.push(item);
                *remaining -= 1;
                if *remaining != 0 {
                    break;
                }
                item = Resp::Array(self.arrays.pop().unwrap().1);
            }
        }
        if self.frame_bytes + self.bytes.len() - used > RESPONSE_LIMIT {
            return Err("RESP response exceeds the 16 MiB limit".into());
        }
        if used > 0 {
            self.bytes.drain(..used);
        }
        Ok(frames)
    }
}

enum Part {
    Array(usize),
    Value(Resp),
}

fn parse(bytes: &[u8], start: usize) -> Result<Option<(Part, usize)>, String> {
    let Some(&prefix) = bytes.get(start) else {
        return Ok(None);
    };
    if !matches!(prefix, b'+' | b'-' | b':' | b'$' | b'*') {
        return Err("Unsupported RESP frame (the module uses RESP2)".into());
    }
    let Some(end) = bytes[start..]
        .windows(2)
        .position(|p| p == b"\r\n")
        .map(|n| start + n)
    else {
        if bytes.len() - start > 64 * 1024 {
            return Err("RESP header exceeds 64 KiB".into());
        }
        return Ok(None);
    };
    let header = &bytes[start + 1..end];
    let after = end + 2;
    let number = || -> Result<i64, String> {
        std::str::from_utf8(header)
            .ok()
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| "Invalid RESP length or integer".into())
    };
    let result = match prefix {
        b'+' => (Part::Value(Resp::Bytes(header.to_vec())), after),
        b'-' => (
            Part::Value(Resp::Error(String::from_utf8_lossy(header).into_owned())),
            after,
        ),
        b':' => (Part::Value(Resp::Int(number()?)), after),
        b'$' | b'*' => {
            let len = number()?;
            if len == -1 {
                return Ok(Some((Part::Value(Resp::Null), after)));
            }
            if len < 0 || len as u64 > RESPONSE_LIMIT as u64 {
                return Err("Invalid or oversized RESP length".into());
            }
            let len = len as usize;
            if prefix == b'$' {
                let end = after + len;
                if bytes.len() < end + 2 {
                    return Ok(None);
                }
                if &bytes[end..end + 2] != b"\r\n" {
                    return Err("Invalid RESP string terminator".into());
                }
                (
                    Part::Value(Resp::Bytes(bytes[after..end].to_vec())),
                    end + 2,
                )
            } else {
                if len > 100_000 {
                    return Err("RESP array exceeds the response limit".into());
                }
                (Part::Array(len), after)
            }
        }
        _ => return Err("Unsupported RESP frame (the module uses RESP2)".into()),
    };
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragmented_nested_binary_and_multiple_replies_are_lossless() {
        let wire = b"*4\r\n$2\r\n\xff\0\r\n$-1\r\n:-2\r\n*2\r\n+OK\r\n-ERR test\r\n";
        for size in 1..=wire.len() {
            let mut decoder = Decoder::default();
            let frames: Vec<_> = wire
                .chunks(size)
                .flat_map(|chunk| decoder.feed(chunk).unwrap())
                .collect();
            assert_eq!(frames.len(), 1);
            assert_eq!(
                frames[0].json(),
                json!([
                    {"encoding":"base64","data":"/wA="}, null, -2, ["OK", {"error":"ERR test"}]
                ])
            );
        }
        assert_eq!(Decoder::default().feed(b"+OK\r\n:1\r\n").unwrap().len(), 2);
    }

    #[test]
    fn malformed_and_oversized_frames_fail_without_allocating_the_declared_size() {
        for frame in [
            b"$999999999\r\n".as_slice(),
            b"*-3\r\n",
            b"$1\r\nx!!",
            b":x\r\n",
            b"\r\n",
            b"*100001\r\n",
        ] {
            assert!(Decoder::default().feed(frame).is_err());
        }
        let nested = format!("{}+OK\r\n", "*1\r\n".repeat(66));
        assert!(Decoder::default().feed(nested.as_bytes()).is_err());
    }
}
