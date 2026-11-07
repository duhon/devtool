//! The export stream: newline-delimited JSON records of results, previews and messages,
//! read up to Count rows after Offset.

use serde_json::{Value, json};

/// The most bytes of results (and of one record) a search keeps.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

type Result<T> = std::result::Result<T, String>;

#[derive(Debug)]
pub struct Export {
    pub limit: Option<u64>,
    pub offset: u64,
    pub skip: u64,
    pub allow_preview: bool,
    pub preview: Option<bool>,
    pub rows_seen: u64,
    pub batch_ended: bool,
    pub preview_complete: bool,
    pub pending: Vec<u8>,
    pub columns: Vec<String>,
    pub rows: Vec<Value>,
    pub messages: Vec<Value>,
    pub retained: usize,
    pub message_bytes: usize,
    pub records: usize,
    pub previews: usize,
    pub final_seen: bool,
    pub limited: bool,
    pub complete: bool,
}

impl Export {
    pub fn new(limit: Option<u64>, offset: u64, allow_preview: bool) -> Self {
        Self {
            limit,
            offset,
            skip: offset,
            allow_preview,
            preview: None,
            rows_seen: 0,
            batch_ended: false,
            preview_complete: false,
            pending: vec![],
            columns: vec![],
            rows: vec![],
            messages: vec![],
            retained: 0,
            message_bytes: 0,
            records: 0,
            previews: 0,
            final_seen: false,
            limited: false,
            complete: false,
        }
    }

    pub fn push(&mut self, data: &[u8]) -> Result<bool> {
        for part in data.split_inclusive(|byte| *byte == b'\n') {
            if self.pending.len().saturating_add(part.len()) > MAX_RESPONSE_BYTES {
                return Err(String::from(
                    "Splunk export JSON record exceeds the 64 MiB response limit",
                ));
            }
            self.pending.extend_from_slice(part);
            if part.ends_with(b"\n") {
                let record = std::mem::take(&mut self.pending);
                self.record(&record)?;
                if self.limited || self.preview_complete {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn retain(&mut self, value: &Value) -> Result<()> {
        self.retained = self
            .retained
            .saturating_add(serde_json::to_vec(value).map_err(|e| e.to_string())?.len());
        if self.retained > MAX_RESPONSE_BYTES {
            return Err(String::from(
                "Splunk export results exceed the 64 MiB response limit; reduce Count or narrow the search",
            ));
        }
        Ok(())
    }

    fn column(&mut self, name: &str) -> Result<()> {
        if !self.columns.iter().any(|column| column == name) {
            self.retain(&json!(name))?;
            self.columns.push(name.to_string());
        }
        Ok(())
    }

    fn record(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|e| format!("Invalid or truncated Splunk export JSON: {e}"))?;
        if !value.is_object()
            || ![
                "result", "results", "messages", "fields", "preview", "lastrow",
            ]
            .iter()
            .any(|key| value.get(key).is_some())
        {
            return Err(String::from("Unrecognized Splunk export JSON record"));
        }
        self.records += 1;
        if let Some(messages) = value.get("messages") {
            for message in messages
                .as_array()
                .ok_or_else(|| String::from("Invalid Splunk export messages"))?
            {
                self.retain(message)?;
                self.message_bytes = self.message_bytes.saturating_add(
                    serde_json::to_vec(message)
                        .map_err(|e| e.to_string())?
                        .len(),
                );
                self.messages.push(message.clone());
                if message["type"].as_str().is_some_and(|kind| {
                    kind.eq_ignore_ascii_case("ERROR") || kind.eq_ignore_ascii_case("FATAL")
                }) {
                    return Err(String::from(
                        message["text"]
                            .as_str()
                            .unwrap_or("Splunk export search failed"),
                    ));
                }
            }
        }
        if value["preview"] == true {
            self.previews += 1;
            if !self.allow_preview {
                return Ok(());
            }
        }
        if value["preview"] == false {
            self.final_seen = true;
        } else if value.get("preview").is_some() && value["preview"] != true {
            return Err(String::from("Invalid Splunk export preview flag"));
        }
        let rows = if let Some(row) = value.get("result") {
            std::slice::from_ref(row)
        } else if let Some(rows) = value.get("results") {
            rows.as_array()
                .ok_or_else(|| String::from("Invalid Splunk export results"))?
                .as_slice()
        } else {
            &[]
        };
        if !rows.is_empty() && !value["preview"].is_boolean() {
            return Err(String::from(
                "Splunk export results are missing a boolean preview flag",
            ));
        }
        if let Some(preview) = value["preview"].as_bool() {
            if self.allow_preview
                && (self.preview != Some(preview)
                    || self.batch_ended
                    || (!rows.is_empty() && value["offset"] == 0 && self.rows_seen > 0))
            {
                self.rows.clear();
                self.columns.clear();
                self.retained = self.message_bytes;
                self.skip = self.offset;
                self.rows_seen = 0;
                self.batch_ended = false;
            }
            self.preview = Some(preview);
        }
        if let Some(fields) = value.get("fields") {
            for field in fields
                .as_array()
                .ok_or_else(|| String::from("Invalid Splunk export fields"))?
            {
                let name = field
                    .as_str()
                    .or_else(|| field["name"].as_str())
                    .ok_or_else(|| String::from("Invalid Splunk export field name"))?;
                self.column(name)?;
            }
        }
        for row in rows {
            let object = row
                .as_object()
                .ok_or_else(|| String::from("Invalid Splunk export result row"))?;
            self.rows_seen = self.rows_seen.saturating_add(1);
            if self.skip > 0 {
                self.skip -= 1;
                continue;
            }
            self.retain(row)?;
            for key in object.keys() {
                self.column(key)?;
            }
            self.rows.push(row.clone());
            if self
                .limit
                .is_some_and(|limit| self.rows.len() as u64 >= limit)
            {
                self.limited = true;
                break;
            }
        }
        if value["lastrow"] == true {
            self.batch_ended = true;
            self.preview_complete = self.preview == Some(true) && !self.rows.is_empty();
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<()> {
        let pending = std::mem::take(&mut self.pending);
        self.record(&pending)?;
        if self.records == 0 {
            return Err(String::from(
                "Splunk export returned an empty stream, not JSON",
            ));
        }
        if self.previews > 0
            && !self.final_seen
            && !(self.allow_preview && (self.limited || self.preview_complete))
        {
            return Err(String::from(
                "Splunk export ended with preview results only, without final results",
            ));
        }
        self.complete = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_offset_restarts_for_each_snapshot_and_zero_count_returns_one_snapshot() {
        for limit in [None, Some(30)] {
            let mut stream = Export::new(limit, 2, true);
            assert!(!stream.push(b"{\"preview\":true,\"results\":[{\"old\":0},{\"old\":1}],\"lastrow\":true}\n").unwrap());
            assert!(stream.push(b"{\"preview\":true,\"offset\":0,\"fields\":[\"n\"],\"results\":[{\"n\":0},{\"n\":1},{\"n\":2},{\"n\":3}],\"lastrow\":true}\n").unwrap());
            assert_eq!(stream.rows, [json!({"n":2}), json!({"n":3})]);
            assert_eq!(stream.columns, ["n"]);
            assert_eq!(stream.preview, Some(true));
            assert_eq!(stream.skip, 0);
            assert!(stream.preview_complete && !stream.complete && !stream.limited);
        }
    }

    #[test]
    fn a_single_preview_row_and_separate_completion_markers_return_without_eof() {
        for limit in [None, Some(30)] {
            let mut stream = Export::new(limit, 0, true);
            assert!(
                stream
                    .push(b"{\"preview\":true,\"result\":{\"n\":1},\"lastrow\":true}\n")
                    .unwrap()
            );
            assert_eq!(stream.rows, [json!({"n":1})]);
            assert!(stream.preview_complete && !stream.complete);
            let mut stream = Export::new(limit, 0, true);
            assert!(
                !stream
                    .push(b"{\"preview\":true,\"result\":{\"n\":1}}\n")
                    .unwrap()
            );
            assert!(
                stream
                    .push(b"{\"preview\":true,\"lastrow\":true}\n")
                    .unwrap()
            );
            assert_eq!(stream.rows, [json!({"n":1})]);
        }
        let mut stream = Export::new(None, 1, true);
        assert!(
            !stream
                .push(b"{\"preview\":true,\"result\":{\"old\":1},\"lastrow\":true}\n")
                .unwrap()
        );
        assert!(
            stream
                .push(b"{\"preview\":true,\"results\":[{\"n\":1},{\"n\":2}],\"lastrow\":true}\n")
                .unwrap()
        );
        assert_eq!(stream.rows, [json!({"n":2})]);
    }

    #[test]
    fn preview_count_and_new_snapshot_boundaries_never_mix_rows() {
        for final_batch in [false, true] {
            let mut stream = Export::new(Some(2), 0, true);
            assert!(
                !stream
                    .push(b"{\"preview\":true,\"offset\":0,\"result\":{\"old\":\"discard\"}}\n")
                    .unwrap()
            );
            let body = format!(
                "{}\n{}\n",
                json!({"preview":!final_batch,"offset":0,"result":{"new":0}}),
                json!({"preview":!final_batch,"offset":1,"result":{"new":1}})
            );
            assert!(stream.push(body.as_bytes()).unwrap());
            assert_eq!(stream.rows, [json!({"new":0}), json!({"new":1})]);
            assert_eq!(stream.columns, ["new"]);
            assert_eq!(stream.preview, Some(!final_batch));
            assert!(stream.limited && !stream.complete);
        }
    }

    #[test]
    fn final_offset_skips_only_final_rows_and_incomplete_preview_is_not_success() {
        let mut stream = Export::new(Some(1), 2, false);
        assert!(stream.push(concat!(
            "{\"preview\":true,\"results\":[{\"n\":100},{\"n\":101},{\"n\":102}],\"lastrow\":true}\n",
            "{\"preview\":false,\"results\":[{\"n\":0},{\"n\":1},{\"n\":2},{\"n\":3}]}\n"
        ).as_bytes()).unwrap());
        assert_eq!(stream.rows, [json!({"n":2})]);
        assert_eq!(stream.preview, Some(false));
        for (offset, body) in [
            (0, "{\"preview\":true,\"result\":{\"n\":1}}\n"),
            (
                2,
                "{\"preview\":true,\"result\":{\"n\":1},\"lastrow\":true}\n",
            ),
        ] {
            let mut stream = Export::new(Some(30), offset, true);
            assert!(!stream.push(body.as_bytes()).unwrap());
            assert!(stream.finish().is_err());
        }
    }

    #[test]
    fn export_handles_every_utf8_boundary_previews_fields_and_messages() {
        let body = concat!(
            "\n{\"preview\":true,\"result\":{\"temporary\":\"ignore\"},\"lastrow\":true}\n",
            "{\"preview\":false,\"fields\":[{\"name\":\"host\"}],\"result\":{\"host\":\"сервер🦀\"}}\n",
            "{\"preview\":false,\"results\":[{\"host\":\"two\",\"extra\":[\"a\",\"b\"]}],",
            "\"messages\":[{\"type\":\"WARN\",\"text\":\"fixture\"}],\"lastrow\":true}"
        );
        for boundary in 0..=body.len() {
            let mut stream = Export::new(None, 0, false);
            stream.push(&body.as_bytes()[..boundary]).unwrap();
            stream.push(&body.as_bytes()[boundary..]).unwrap();
            stream.finish().unwrap();
            assert_eq!(stream.rows.len(), 2);
            assert_eq!(stream.rows[0]["host"], "сервер🦀");
            assert_eq!(stream.columns, ["host", "extra"]);
            assert_eq!(stream.messages[0]["text"], "fixture");
            assert_eq!(stream.previews, 1);
            assert!(stream.complete);
        }
    }

    #[test]
    fn export_stops_at_count_and_never_treats_bad_streams_as_success() {
        let mut stream = Export::new(Some(1), 0, false);
        assert!(
            stream
                .push(b"{\"preview\":false,\"result\":{\"x\":1}}\nnot read")
                .unwrap()
        );
        assert_eq!(stream.rows, [json!({"x":1})]);
        assert!(stream.limited && !stream.complete);
        for body in [
            "",
            "\n ",
            "{\"preview\":false,\"result\":",
            "{\"unrecognized\":1}",
            "{\"preview\":true,\"result\":{\"x\":1}}\n",
            "{\"preview\":true,\"result\":{\"x\":1}}\n{\"messages\":[]}\n",
            "{\"preview\":\"false\",\"results\":[]}\n",
            "{\"result\":{\"x\":1}}\n",
            "{\"preview\":false,\"result\":[]}\n",
            "{\"messages\":[{\"type\":\"ERROR\",\"text\":\"bad search\"}]}\n",
            "{\"preview\":false,\"results\":{}}\n",
        ] {
            let mut stream = Export::new(None, 0, false);
            assert!(
                stream
                    .push(body.as_bytes())
                    .and_then(|_| stream.finish())
                    .is_err(),
                "{body}"
            );
        }
        let mut stream = Export::new(None, 0, false);
        stream
            .push(b"{\"preview\":false,\"results\":[]}\n")
            .unwrap();
        stream.finish().unwrap();
        assert!(stream.rows.is_empty());
    }

    #[test]
    fn export_bounds_partial_records_and_retained_results() {
        let mut stream = Export::new(None, 0, false);
        stream.pending = vec![b' '; MAX_RESPONSE_BYTES];
        assert!(stream.push(b"x").unwrap_err().contains("64 MiB"));
        stream.pending.clear();
        stream.retained = MAX_RESPONSE_BYTES;
        assert!(
            stream
                .push(b"{\"preview\":false,\"result\":{\"x\":1}}\n")
                .unwrap_err()
                .contains("reduce Count")
        );
    }
}
