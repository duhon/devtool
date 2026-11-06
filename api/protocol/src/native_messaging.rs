use serde_json::Value;

pub const MAX_HOST_MESSAGE_BYTES: usize = 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
pub const RESPONSE_CHUNK_BYTES: usize = 128 * 1024;

#[derive(Default)]
pub struct ResponseAssembler {
    total: Option<usize>,
    text: String,
}

impl ResponseAssembler {
    pub fn push(&mut self, message: &Value) -> Result<Option<Value>, String> {
        let invalid = || "Invalid native response chunk".to_owned();
        let total = message["totalBytes"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0 && *n <= MAX_RESPONSE_BYTES)
            .ok_or_else(invalid)?;
        let offset = message["offsetBytes"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(invalid)?;
        let data = message["data"].as_str().ok_or_else(invalid)?;
        if message["type"] != "nativeChunk"
            || data.is_empty()
            || data.len() > RESPONSE_CHUNK_BYTES
            || offset != self.text.len()
            || self.total.is_some_and(|expected| expected != total)
            || data.len() > total.saturating_sub(offset)
        {
            return Err(invalid());
        }
        self.total = Some(total);
        self.text.push_str(data);
        if self.text.len() != total {
            return Ok(None);
        }
        let text = std::mem::take(&mut self.text);
        self.total = None;
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| format!("Invalid native response JSON: {error}"))?;
        if value.get("id").is_none()
            || value["id"] != message["id"]
            || value["type"] == "nativeChunk"
        {
            return Err(invalid());
        }
        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chunk(data: &str, offset: usize, total: usize) -> Value {
        json!({"type":"nativeChunk","id":9,"offsetBytes":offset,"totalBytes":total,"data":data})
    }

    #[test]
    fn independent_responses_preserve_unicode_json_and_stream_events() {
        let original = json!({"id":9,"type":"data","event":{"data":"quote \" slash \\ \r\n 🦀 é"}});
        let text = serde_json::to_string(&original).unwrap();
        let split = text.find('🦀').unwrap() + '🦀'.len_utf8();
        let mut first = ResponseAssembler::default();
        let mut second = ResponseAssembler::default();
        assert_eq!(first.push(&chunk(&text[..split], 0, text.len())), Ok(None));
        assert_eq!(
            second.push(&chunk(&text, 0, text.len())),
            Ok(Some(original.clone()))
        );
        assert_eq!(
            first.push(&chunk(&text[split..], split, text.len())),
            Ok(Some(original))
        );
        assert_eq!(
            first.push(&chunk(r#"{"id":9,"ok":true}"#, 0, 18)),
            Ok(Some(json!({"id":9,"ok":true})))
        );
    }

    #[test]
    fn malformed_chunks_are_errors_not_partial_success() {
        for value in [
            chunk("x", 0, 0),
            chunk("x", 0, MAX_RESPONSE_BYTES + 1),
            chunk("x", 1, 2),
            chunk("", 0, 1),
            chunk("{}", 0, 1),
            chunk(&"x".repeat(RESPONSE_CHUNK_BYTES + 1), 0, MAX_RESPONSE_BYTES),
            chunk("{", 0, 1),
            chunk(r#"{"id":8}"#, 0, 8),
            chunk(r#"{"id":9,"type":"nativeChunk"}"#, 0, 29),
        ] {
            assert!(
                ResponseAssembler::default().push(&value).is_err(),
                "{value}"
            );
        }
        let mut assembler = ResponseAssembler::default();
        assert_eq!(assembler.push(&chunk("{", 0, 8)), Ok(None));
        assert!(assembler.push(&chunk("x", 0, 8)).is_err());
        assert!(assembler.push(&chunk("x", 1, 9)).is_err());
    }
}
