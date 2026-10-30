use crate::core::text::SharedStr;
use crate::{Error, Result};
use serde_json::{Value, json};
use services_contracts::native_messaging::{
    MAX_HOST_MESSAGE_BYTES, MAX_RESPONSE_BYTES, RESPONSE_CHUNK_BYTES,
};
use std::borrow::Cow;
const MAX_FRAME: usize = 64 * 1024 * 1024;
pub fn encode(value: &Value) -> Result<Vec<u8>> {
    encode_bytes(&serde_json::to_vec(value)?)
}
fn encode_bytes(bytes: &[u8]) -> Result<Vec<u8>> {
    encode_parts(&[bytes])
}
fn encode_parts(parts: &[&[u8]]) -> Result<Vec<u8>> {
    let len = parts.iter().map(|part| part.len()).sum::<usize>();
    if len > MAX_HOST_MESSAGE_BYTES {
        return Err(Error::new(
            "Native host message exceeds Chrome's 1 MiB limit",
        ));
    }
    let mut frame = Vec::with_capacity(len + 4);
    frame.extend_from_slice(&(len as u32).to_le_bytes());
    for part in parts {
        frame.extend_from_slice(part);
    }
    Ok(frame)
}

/// The frames of one reply whose text is the concatenation of `parts`.
pub struct ResponseFrames {
    parts: Vec<SharedStr>,
    /// Where each part starts in the text.
    starts: Vec<usize>,
    len: usize,
    id: Value,
    offset: usize,
    chunked: bool,
}
pub fn response_frames(value: &Value, chunked: bool) -> Result<ResponseFrames> {
    response_frames_parts(
        vec![serde_json::to_string(value)?.into()],
        value["id"].clone(),
        chunked,
    )
}
/// `parts` concatenate to a serialized reply; `id` labels its chunks.
pub fn response_frames_parts(
    mut parts: Vec<SharedStr>,
    id: Value,
    chunked: bool,
) -> Result<ResponseFrames> {
    let len = parts.iter().map(|part| part.len()).sum::<usize>();
    if len > MAX_HOST_MESSAGE_BYTES && (!chunked || len > MAX_RESPONSE_BYTES) {
        let error = if chunked {
            "Native response exceeds the 64 MiB reassembly limit; reduce the result size."
        } else {
            "Native response exceeds Chrome's 1 MiB message limit. Update the extension to enable chunked responses."
        };
        parts = vec![serde_json::to_string(&json!({"id":id,"ok":false,"error":error}))?.into()];
    }
    let starts = parts
        .iter()
        .scan(0, |at, part| {
            let start = *at;
            *at += part.len();
            Some(start)
        })
        .collect();
    let len = parts.iter().map(|part| part.len()).sum::<usize>();
    Ok(ResponseFrames {
        parts,
        starts,
        chunked: len > MAX_HOST_MESSAGE_BYTES,
        len,
        id,
        offset: 0,
    })
}
impl ResponseFrames {
    fn spans(&self) -> impl Iterator<Item = (&SharedStr, usize)> {
        self.parts.iter().zip(self.starts.iter().copied())
    }

    /// `end` moved back to a char boundary; part edges are boundaries.
    fn boundary(&self, end: usize) -> usize {
        match self
            .spans()
            .find(|(part, start)| end > *start && end < start + part.len())
        {
            Some((part, start)) => {
                let mut local = end - start;
                while !part.is_char_boundary(local) {
                    local -= 1;
                }
                start + local
            }
            None => end,
        }
    }

    /// The text in `from..to`, borrowed when it lies within one part.
    fn text(&self, from: usize, to: usize) -> Cow<'_, str> {
        let mut slices = self.spans().filter_map(|(part, start)| {
            let (a, b) = (from.max(start), to.min(start + part.len()));
            (a < b).then(|| &part[a - start..b - start])
        });
        let first = slices.next().unwrap_or("");
        match slices.next() {
            None => Cow::Borrowed(first),
            Some(second) => {
                let mut text = String::with_capacity(to - from);
                text.push_str(first);
                text.push_str(second);
                slices.for_each(|slice| text.push_str(slice));
                Cow::Owned(text)
            }
        }
    }
}
impl Iterator for ResponseFrames {
    type Item = Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset == self.len {
            return None;
        }
        if !self.chunked {
            self.offset = self.len;
            let parts: Vec<&[u8]> = self.parts.iter().map(|part| part.as_bytes()).collect();
            return Some(encode_parts(&parts));
        }
        let end = self.boundary((self.offset + RESPONSE_CHUNK_BYTES).min(self.len));
        let frame = chunk_frame(&Chunk {
            kind: "nativeChunk",
            id: &self.id,
            offset_bytes: self.offset,
            total_bytes: self.len,
            data: &self.text(self.offset, end),
        });
        self.offset = end;
        Some(frame)
    }
}

/// Field order matches `json!({"type","id","offsetBytes","totalBytes","data"})`.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Chunk<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    id: &'a Value,
    offset_bytes: usize,
    total_bytes: usize,
    data: &'a str,
}

/// Serializes straight into the frame: no intermediate value or buffer per chunk.
fn chunk_frame(chunk: &Chunk) -> Result<Vec<u8>> {
    let mut frame = Vec::with_capacity(4 + chunk.data.len() * 3 / 2 + 128);
    frame.extend_from_slice(&[0; 4]);
    serde_json::to_writer(&mut frame, chunk)?;
    let size = frame.len() - 4;
    if size > MAX_HOST_MESSAGE_BYTES {
        return Err(Error::new(
            "Native host message exceeds Chrome's 1 MiB limit",
        ));
    }
    frame[..4].copy_from_slice(&(size as u32).to_le_bytes());
    Ok(frame)
}
#[derive(Default)]
pub struct FrameReader {
    buffer: Vec<u8>,
}
impl FrameReader {
    pub fn feed(&mut self, input: &[u8]) -> Result<Vec<Value>> {
        self.buffer.extend_from_slice(input);
        let mut consumed = 0;
        let mut messages = vec![];
        while self.buffer.len() - consumed >= 4 {
            let len = u32::from_le_bytes(self.buffer[consumed..consumed + 4].try_into().unwrap())
                as usize;
            if len > MAX_FRAME {
                return Err(Error::new("Native message exceeds 64 MiB limit"));
            }
            if self.buffer.len() - consumed < 4 + len {
                break;
            }
            if let Ok(value) =
                serde_json::from_slice(&self.buffer[consumed + 4..consumed + 4 + len])
            {
                messages.push(value);
            }
            consumed += 4 + len;
        }
        self.buffer.drain(..consumed);
        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use services_contracts::native_messaging::ResponseAssembler;

    #[test]
    fn host_limit_is_measured_after_json_serialization() {
        for (size, accepted) in [
            (MAX_HOST_MESSAGE_BYTES - 1, true),
            (MAX_HOST_MESSAGE_BYTES, true),
            (MAX_HOST_MESSAGE_BYTES + 1, false),
        ] {
            let value = json!("x".repeat(size - 2));
            assert_eq!(serde_json::to_vec(&value).unwrap().len(), size);
            assert_eq!(encode(&value).is_ok(), accepted);
        }
        assert!(encode(&json!("\0".repeat(MAX_HOST_MESSAGE_BYTES / 6 + 1))).is_err());
    }

    #[test]
    fn large_responses_roundtrip_in_individually_bounded_frames() {
        for value in [
            json!({"id":9,"ok":true,"result":{"rows":[{"text":"🦀\\\"\n\0".repeat(220_000)}]}}),
            json!({"id":9,"type":"data","event":{"data":"x".repeat(2*MAX_HOST_MESSAGE_BYTES)}}),
            json!({"id":9,"type":"exit","info":{"stdout":"x".repeat(2*MAX_HOST_MESSAGE_BYTES)}}),
        ] {
            let mut reader = FrameReader::default();
            let mut assembler = ResponseAssembler::default();
            let mut result = None;
            let frames = response_frames(&value, true).unwrap();
            for frame in frames {
                let frame = frame.unwrap();
                let size = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
                assert_eq!(frame.len(), size + 4);
                assert!(size <= MAX_HOST_MESSAGE_BYTES);
                for message in reader.feed(&frame).unwrap() {
                    if let Some(complete) = assembler.push(&message).unwrap() {
                        assert!(result.replace(complete).is_none());
                    }
                }
            }
            assert_eq!(result, Some(value));
        }
    }

    #[test]
    fn old_clients_get_an_explicit_error_and_small_replies_stay_unchanged() {
        let value = json!({"id":9,"ok":true,"result":{"body":"x".repeat(MAX_HOST_MESSAGE_BYTES)}});
        let frames: Vec<_> = response_frames(&value, false).unwrap().collect();
        assert_eq!(frames.len(), 1);
        let reply = FrameReader::default()
            .feed(frames[0].as_ref().unwrap())
            .unwrap()
            .remove(0);
        assert_eq!(reply["id"], 9);
        assert_eq!(reply["ok"], false);
        assert!(
            reply["error"]
                .as_str()
                .unwrap()
                .contains("Update the extension")
        );
        let small = json!({"id":10,"ok":true,"result":{}});
        assert_eq!(
            response_frames(&small, true)
                .unwrap()
                .next()
                .unwrap()
                .unwrap(),
            encode(&small).unwrap()
        );
    }

    #[test]
    fn chunk_frames_match_the_value_encoding() {
        for id in [json!("req-🦀"), json!(7), Value::Null] {
            let data = "🦀\\\"\n\0x\u{1f}";
            let chunk = Chunk {
                kind: "nativeChunk",
                id: &id,
                offset_bytes: 3,
                total_bytes: 99,
                data,
            };
            let value =
                json!({"type":"nativeChunk","id":id,"offsetBytes":3,"totalBytes":99,"data":data});
            assert_eq!(chunk_frame(&chunk).unwrap(), encode(&value).unwrap());
        }
    }

    #[test]
    fn segmented_replies_frame_like_one_text() {
        let text = format!(
            "{{\"id\":1,\"result\":\"{}\"}}",
            "a🦀\\\"é".repeat(MAX_HOST_MESSAGE_BYTES / 4)
        );
        let id = json!(1);
        let whole: Vec<_> = response_frames_parts(vec![text.clone().into()], id.clone(), true)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(whole.len() > 3);
        let cuts = [
            0,
            1,
            RESPONSE_CHUNK_BYTES - 2,
            RESPONSE_CHUNK_BYTES + 5,
            2 * RESPONSE_CHUNK_BYTES,
            2 * RESPONSE_CHUNK_BYTES + 40,
            text.len() - 1,
            text.len(),
        ]
        .map(|cut| {
            (0..=cut)
                .rev()
                .find(|at| text.is_char_boundary(*at))
                .unwrap()
        });
        for pair in cuts.windows(2) {
            for third in &cuts {
                let mut edges = vec![pair[0], pair[1], *third];
                edges.sort();
                let parts = [0, edges[0], edges[1], edges[2], text.len()]
                    .windows(2)
                    .map(|span| text[span[0]..span[1]].to_string().into())
                    .collect();
                let split: Vec<_> = response_frames_parts(parts, id.clone(), true)
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                assert_eq!(split, whole, "{edges:?}");
            }
        }
        let small: Vec<SharedStr> = vec!["{\"id\":".into(), "1".into(), "}".into()];
        assert_eq!(
            response_frames_parts(small, id, false)
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap(),
            [encode(&json!({"id":1})).unwrap()]
        );
    }

    #[test]
    fn inbound_messages_still_allow_more_than_one_mib() {
        let value = json!({"id":9,"params":{"body":"x".repeat(MAX_HOST_MESSAGE_BYTES)}});
        let bytes = serde_json::to_vec(&value).unwrap();
        let mut frame = (bytes.len() as u32).to_le_bytes().to_vec();
        frame.extend(bytes);
        assert_eq!(FrameReader::default().feed(&frame).unwrap(), [value]);
        assert!(
            FrameReader::default()
                .feed(&((MAX_FRAME + 1) as u32).to_le_bytes())
                .is_err()
        );
    }
}
