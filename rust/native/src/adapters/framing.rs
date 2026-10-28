use crate::{Error, Result};
use serde_json::{Value, json};
use services_contracts::native_messaging::{
    MAX_HOST_MESSAGE_BYTES, MAX_RESPONSE_BYTES, RESPONSE_CHUNK_BYTES,
};
const MAX_FRAME: usize = 64 * 1024 * 1024;
pub fn encode(value: &Value) -> Result<Vec<u8>> {
    encode_bytes(&serde_json::to_vec(value)?)
}
fn encode_bytes(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() > MAX_HOST_MESSAGE_BYTES {
        return Err(Error::new(
            "Native host message exceeds Chrome's 1 MiB limit",
        ));
    }
    let mut frame = Vec::with_capacity(bytes.len() + 4);
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(bytes);
    Ok(frame)
}

pub struct ResponseFrames {
    text: String,
    id: Value,
    offset: usize,
    chunked: bool,
}
pub fn response_frames(value: &Value, chunked: bool) -> Result<ResponseFrames> {
    let mut text = serde_json::to_string(value)?;
    let id = value["id"].clone();
    let needs_chunks = text.len() > MAX_HOST_MESSAGE_BYTES;
    if needs_chunks && (!chunked || text.len() > MAX_RESPONSE_BYTES) {
        let error = if chunked {
            "Native response exceeds the 64 MiB reassembly limit; reduce the result size."
        } else {
            "Native response exceeds Chrome's 1 MiB message limit. Update the extension to enable chunked responses."
        };
        text = serde_json::to_string(&json!({"id":id,"ok":false,"error":error}))?;
    }
    Ok(ResponseFrames {
        chunked: text.len() > MAX_HOST_MESSAGE_BYTES,
        text,
        id,
        offset: 0,
    })
}
impl Iterator for ResponseFrames {
    type Item = Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset == self.text.len() {
            return None;
        }
        if !self.chunked {
            self.offset = self.text.len();
            return Some(encode_bytes(self.text.as_bytes()));
        }
        let mut end = (self.offset + RESPONSE_CHUNK_BYTES).min(self.text.len());
        while !self.text.is_char_boundary(end) {
            end -= 1;
        }
        let frame = encode(&json!({
            "type":"nativeChunk","id":self.id,"offsetBytes":self.offset,
            "totalBytes":self.text.len(),"data":&self.text[self.offset..end],
        }));
        self.offset = end;
        Some(frame)
    }
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
