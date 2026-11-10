//! Immutable Console snapshots. Offsets are UTF-8 bytes, not characters.
use super::error::{Error, Result};
use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const MAX_BYTES: usize = 100 * 1024 * 1024;
const CHUNK: usize = 128 * 1024;

#[derive(Clone)]
pub struct History {
    pub path: PathBuf,
    quota: usize,
    target: usize,
    /// The latest run of each repeating check, saved once its repeat ends.
    deferred: Arc<Mutex<HashMap<String, Snapshot>>>,
}
pub struct Snapshot {
    pub check_id: String,
    pub session_id: String,
    pub title: String,
    pub service: String,
    pub started: u64,
    pub ended: u64,
    pub duration: u64,
    pub status: String,
    pub text: String,
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn error(e: impl std::fmt::Display) -> Error {
    Error::new(format!("Execution history: {e}"))
}
pub fn response_text(response: &Value) -> Result<String> {
    if let Some(body) = response.get("body") {
        return match body {
            Value::String(body) => Ok(body.clone()),
            body => Ok(serde_json::to_string_pretty(body)?),
        };
    }
    let mut response = response.clone();
    if let Some(fields) = response.as_object_mut() {
        fields.remove("debug");
    }
    Ok(serde_json::to_string_pretty(&response)?)
}
fn clean_legacy_snapshot(text: &str) -> Result<Option<String>> {
    let Some((source, recorded)) = text.rsplit_once("\n\n#effective-parameters\n") else {
        return Ok(None);
    };
    let (params, response) = recorded
        .split_once("\n\n#response ")
        .ok_or_else(|| error("legacy snapshot has no response"))?;
    let _: Value = serde_json::from_str(params)?;
    let (status, response) = response
        .split_once('\n')
        .ok_or_else(|| error("legacy snapshot has no response body"))?;
    if !matches!(status, "success" | "error" | "stopped") {
        return Err(error("legacy snapshot has an invalid response status"));
    }
    let (output, response) = if let Some(output) = response.strip_prefix("#terminal-output\n") {
        let (output, response) = output
            .rsplit_once("\n{")
            .ok_or_else(|| error("legacy terminal snapshot has no response"))?;
        (
            format!("#terminal-output\n{output}\n"),
            format!("{{{response}"),
        )
    } else {
        (String::new(), response.to_owned())
    };
    let response = response_text(&serde_json::from_str(&response)?)?;
    Ok(Some(format!(
        "{source}\n\n#response {status}\n{output}{response}"
    )))
}
pub fn uuid(value: &Value, key: &str) -> Result<String> {
    let value = value[key]
        .as_str()
        .ok_or_else(|| error(format!("{key} must be a UUID")))?;
    if value.len() != 36
        || !value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
    {
        return Err(error(format!("{key} must be a UUID")));
    }
    Ok(value.to_ascii_lowercase())
}
fn positive(value: &Value, key: &str) -> Result<i64> {
    value[key]
        .as_u64()
        .filter(|v| *v > 0 && *v <= i64::MAX as u64)
        .map(|v| v as i64)
        .ok_or_else(|| error(format!("{key} must be a positive integer")))
}
impl History {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            quota: MAX_BYTES,
            target: MAX_BYTES / 2,
            deferred: Default::default(),
        }
    }
    /// Keeps a repeat's run until `commit`; a later run of the same check replaces it.
    pub fn defer(&self, snapshot: Snapshot) {
        self.deferred
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(snapshot.check_id.clone(), snapshot);
    }
    /// Saves the last run of a finished repeat.
    pub async fn commit(&self, message: &Value) -> Result<Value> {
        let check = uuid(message, "checkId")?;
        let snapshot = self
            .deferred
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&check);
        Ok(match snapshot {
            Some(snapshot) => match self.save(snapshot).await {
                Ok(id) => json!({"saved":true,"id":id}),
                Err(e) => json!({"saved":false,"error":e.to_string()}),
            },
            None => json!({"empty":true}),
        })
    }
    fn open(&self) -> Result<Connection> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(error)?;
        }
        if std::fs::symlink_metadata(&self.path).is_ok_and(|m| !m.is_file()) {
            return Err(error("database path is not a regular file"));
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&self.path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(error(e)),
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))
                .map_err(error)?;
        }
        let mut db = Connection::open(&self.path).map_err(error)?;
        db.busy_timeout(Duration::from_secs(10)).map_err(error)?;
        // DELETE journaling has no persistent WAL; FULL auto-vacuum reclaims deleted payload pages.
        db.execute_batch("PRAGMA auto_vacuum=FULL; PRAGMA journal_mode=DELETE; PRAGMA journal_size_limit=0;
            CREATE TABLE IF NOT EXISTS history (
                id INTEGER PRIMARY KEY AUTOINCREMENT, check_id TEXT NOT NULL, session_id TEXT NOT NULL,
                title TEXT NOT NULL, service TEXT NOT NULL, started INTEGER NOT NULL, ended INTEGER NOT NULL,
                duration INTEGER NOT NULL, status TEXT NOT NULL, text TEXT NOT NULL, bytes INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS history_check ON history(check_id, id);").map_err(error)?;
        if db
            .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .map_err(error)?
            == 0
        {
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(error)?;
            if tx
                .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .map_err(error)?
                == 0
            {
                let entries = tx
                    .prepare("SELECT id,text,bytes FROM history")
                    .map_err(error)?
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    })
                    .map_err(error)?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(error)?;
                for (id, text, bytes) in entries {
                    if let Some(cleaned) = clean_legacy_snapshot(&text)? {
                        let bytes = bytes - serde_json::to_vec(&text)?.len() as i64
                            + serde_json::to_vec(&cleaned)?.len() as i64;
                        tx.execute(
                            "UPDATE history SET text=?2,bytes=?3 WHERE id=?1",
                            params![id, cleaned, bytes],
                        )
                        .map_err(error)?;
                    }
                }
                tx.pragma_update(None, "user_version", 1).map_err(error)?;
            }
            tx.commit().map_err(error)?;
        }
        Ok(db)
    }
    pub async fn save(&self, snapshot: Snapshot) -> Result<i64> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.insert(snapshot))
            .await
            .map_err(error)?
    }
    /// Deletes every execution of `service` in all sessions.
    pub fn forget(&self, service: &str) -> Result<usize> {
        if !self.path.exists() {
            return Ok(0);
        }
        self.open()?
            .execute("DELETE FROM history WHERE service=?1", [service])
            .map_err(error)
    }
    fn insert(&self, s: Snapshot) -> Result<i64> {
        let bytes = serde_json::to_vec(&json!({
            "checkId":s.check_id,"sessionId":s.session_id,"title":s.title,"service":s.service,
            "startedAt":s.started,"endedAt":s.ended,"durationMs":s.duration,"status":s.status,"text":s.text
        }))?.len();
        if bytes > self.quota {
            return Err(error("snapshot exceeds the 100 MiB history quota"));
        }
        let mut db = self.open()?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        tx.execute("INSERT INTO history(check_id,session_id,title,service,started,ended,duration,status,text,bytes)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![s.check_id,s.session_id,s.title,s.service,s.started as i64,s.ended as i64,s.duration as i64,s.status,s.text,bytes as i64]).map_err(error)?;
        let id = tx.last_insert_rowid();
        let mut retained: i64 = tx
            .query_row("SELECT COALESCE(SUM(bytes),0) FROM history", [], |r| {
                r.get(0)
            })
            .map_err(error)?;
        if retained > self.quota as i64 {
            while retained > self.target as i64 {
                let oldest = tx.query_row(
                    "SELECT id,bytes FROM history WHERE id<>?1 ORDER BY id LIMIT 1",
                    [id],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
                );
                let (old, bytes) = match oldest {
                    Ok(row) => row,
                    Err(rusqlite::Error::QueryReturnedNoRows) => break,
                    Err(e) => return Err(error(e)),
                };
                tx.execute("DELETE FROM history WHERE id=?1", [old])
                    .map_err(error)?;
                retained -= bytes;
            }
        }
        tx.commit().map_err(error)?;
        Ok(id)
    }
    pub async fn dispatch(&self, message: &Value) -> Result<Value> {
        if message["op"] == "commit" {
            return self.commit(message).await;
        }
        let this = self.clone();
        let message = message.clone();
        tokio::task::spawn_blocking(move || this.read(&message))
            .await
            .map_err(error)?
    }
    fn read(&self, message: &Value) -> Result<Value> {
        match message["op"].as_str() {
            Some("list") => {
                let check = uuid(message, "checkId")?;
                let before = if message.get("before").is_some() {
                    positive(message, "before")?
                } else {
                    i64::MAX
                };
                let limit = if message.get("limit").is_some() {
                    positive(message, "limit")?.min(50)
                } else {
                    25
                };
                let mut db = self.open()?;
                let tx = db.transaction().map_err(error)?;
                let total: i64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM history WHERE check_id=?1",
                        [&check],
                        |r| r.get(0),
                    )
                    .map_err(error)?;
                let bytes: i64 = tx
                    .query_row("SELECT COALESCE(SUM(bytes),0) FROM history", [], |r| {
                        r.get(0)
                    })
                    .map_err(error)?;
                let mut stmt = tx
                    .prepare(
                        "SELECT id,started,ended,duration,status,substr(title,1,512),service FROM history
                    WHERE check_id=?1 AND id<?2 ORDER BY id DESC LIMIT ?3",
                    )
                    .map_err(error)?;
                let mut entries = stmt.query_map(params![check,before,limit+1], |r| Ok(json!({
                    "id":r.get::<_,i64>(0)?,"startedAt":r.get::<_,i64>(1)?,"endedAt":r.get::<_,i64>(2)?,
                    "durationMs":r.get::<_,i64>(3)?,"status":r.get::<_,String>(4)?,
                    "title":r.get::<_,String>(5)?,"service":r.get::<_,String>(6)?
                }))).map_err(error)?.collect::<std::result::Result<Vec<_>,_>>().map_err(error)?;
                let more = entries.len() > limit as usize;
                entries.truncate(limit as usize);
                let next = if more {
                    entries.last().unwrap()["id"].clone()
                } else {
                    Value::Null
                };
                Ok(json!({"entries":entries,"nextBefore":next,"total":total,"bytes":bytes}))
            }
            Some("get") => {
                // Native messaging reserves top-level id for correlation.
                let id = if message.get("entryId").is_some() {
                    positive(message, "entryId")?
                } else {
                    positive(message, "id")?
                };
                let offset = match message.get("offset") {
                    None => 0,
                    Some(v) => v
                        .as_u64()
                        .and_then(|v| usize::try_from(v).ok())
                        .ok_or_else(|| error("offset must be a nonnegative UTF-8 byte offset"))?,
                };
                let db = self.open()?;
                let len: i64 = db
                    .query_row(
                        "SELECT length(CAST(text AS BLOB)) FROM history WHERE id=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .map_err(|e| {
                        if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                            error("entry not found")
                        } else {
                            error(e)
                        }
                    })?;
                let len = len as usize;
                if offset > len {
                    return Err(error("offset exceeds snapshot length"));
                }
                let chunk: Vec<u8> = db
                    .query_row(
                        "SELECT substr(CAST(text AS BLOB),?2,?3) FROM history WHERE id=?1",
                        params![id, (offset + 1) as i64, CHUNK as i64],
                        |r| r.get(0),
                    )
                    .map_err(error)?;
                if chunk.first().is_some_and(|b| b & 0xc0 == 0x80) {
                    return Err(error("offset is not a UTF-8 boundary"));
                }
                let valid = match std::str::from_utf8(&chunk) {
                    Ok(_) => chunk.len(),
                    Err(e) if e.error_len().is_none() => e.valid_up_to(),
                    Err(_) => return Err(error("snapshot is not valid UTF-8")),
                };
                let next = offset + valid;
                Ok(
                    json!({"id":id,"text":std::str::from_utf8(&chunk[..valid]).map_err(error)?,
                    "nextOffset":if next<len {Some(next)} else {None}}),
                )
            }
            _ => Err(error("unknown operation")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    const CHECK: &str = "019a1234-5678-7890-abcd-012345678901";
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Data(PathBuf);
    impl Data {
        fn new() -> Self {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target/history-unit")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn history(&self) -> History {
            History::new(self.0.join(".console-history.sqlite3"))
        }
    }
    impl Drop for Data {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn snapshot(text: String) -> Snapshot {
        Snapshot {
            check_id: CHECK.into(),
            session_id: CHECK.into(),
            title: "title".into(),
            service: "rest".into(),
            started: 1,
            ended: 2,
            duration: 1,
            status: "success".into(),
            text,
        }
    }
    #[test]
    fn response_body_is_verbatim_including_payload_debug_fields_and_credentials() {
        let body = " \n{\"data\":{\"products\":[]},\"debug\":{\"keep\":true},\"token\":\"response-token\"}\r\n";
        assert_eq!(
            response_text(&json!({
                "status":200,"headers":{"x-transport":"hidden"},"body":body,
                "durationMs":404,"debug":{"request":"hidden"}
            }))
            .unwrap(),
            body
        );
        assert_eq!(
            serde_json::from_str::<Value>(
                &response_text(&json!({"error":"failed","debug":{"request":"hidden"}})).unwrap()
            )
            .unwrap(),
            json!({"error":"failed"})
        );
    }
    #[test]
    fn legacy_snapshot_cleanup_preserves_request_and_verbatim_response() {
        let data = Data::new();
        let store = data.history();
        let source = "#check example\n#target\nhttps://catalog.fixture/graphql\n#headers\nX-Api-Key: [redacted]\n#payload\nquery { products { name } }";
        let body = " {\"data\":{\"products\":[]},\"debug\":\"payload-owned\"}\r\n";
        let legacy = format!(
            "{source}\n\n#effective-parameters\n{{\"url\":\"https://catalog.fixture/graphql\"}}\n\n#response success\n{}",
            serde_json::to_string_pretty(&json!({
                "status":200,"body":body,"durationMs":404,"debug":{"request":"transport-only"}
            }))
            .unwrap()
        );
        let id = store.insert(snapshot(legacy)).unwrap();
        let db = store.open().unwrap();
        let bytes: i64 = db
            .query_row("SELECT SUM(bytes) FROM history", [], |r| r.get(0))
            .unwrap();
        db.pragma_update(None, "user_version", 0).unwrap();
        drop(db);
        let expected = format!("{source}\n\n#response success\n{body}");
        let read = store.read(&json!({"op":"get","entryId":id})).unwrap();
        assert_eq!(read["text"], expected);
        assert!(read["nextOffset"].is_null());
        let list = store.read(&json!({"op":"list","checkId":CHECK})).unwrap();
        assert_eq!(list["total"], 1);
        assert_eq!(list["entries"][0]["id"], id);
        assert!(list["bytes"].as_i64().unwrap() < bytes);
        assert_eq!(
            store.read(&json!({"op":"get","entryId":id})).unwrap()["text"],
            expected
        );
        let terminal = "#check terminal\n#payload\nprintf exact";
        let old = format!(
            "{terminal}\n\n#effective-parameters\n{{}}\n\n#response error\n#terminal-output\n{{\"type\":\"stdout\",\"data\":\"exact\"}}\n{{\n  \"code\": 7,\n  \"debug\": {{\"request\": \"hidden\"}}\n}}"
        );
        let cleaned = clean_legacy_snapshot(&old).unwrap().unwrap();
        assert!(cleaned.contains("\"data\":\"exact\""));
        assert!(!cleaned.contains("\"debug\""));
        assert!(cleaned.contains("\"code\": 7"));
    }
    /// The App's `history.insert` of a run started at 1000.
    fn insert(title: &str, text: String, defer: bool) -> Value {
        json!({"service":"history","op":"insert","checkId":CHECK,"sessionId":CHECK,
            "title":title,"module":"terminal","started":1000,"ended":crate::core::history::now(),
            "status":"success","text":text,"defer":defer})
    }
    #[tokio::test]
    async fn oversize_history_inserts_fail_and_parallel_writes_obey_quota() {
        let data = Data::new();
        let mut store = data.history();
        store.quota = 500;
        store.target = 250;
        let mut runtime = crate::Runtime::with_data_dir(&data.0).unwrap();
        runtime.history = store.clone();
        let reply = runtime
            .dispatch(&insert("large", "x".repeat(1000), false))
            .await;
        assert_eq!(reply["ok"], false);
        assert!(reply["error"].as_str().unwrap().contains("exceeds"));
        store.quota = 20_000;
        store.target = 10_000;
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..12 {
            let store = store.clone();
            tasks.spawn(async move { store.save(snapshot("parallel".repeat(700))).await.unwrap() });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
        let list = store
            .dispatch(&json!({"op":"list","checkId":CHECK}))
            .await
            .unwrap();
        assert!(list["bytes"].as_u64().unwrap() <= 20_000);
        assert!(list["total"].as_u64().unwrap() > 0);
    }
    #[tokio::test]
    async fn a_repeat_saves_only_its_last_run_timed_from_its_first_start() {
        let data = Data::new();
        let runtime = crate::Runtime::with_data_dir(&data.0).unwrap();
        let store = runtime.history.clone();
        for text in ["first", "second"] {
            let reply = runtime
                .dispatch(&insert(
                    "repeat",
                    format!("#check repeat\n\n#responce OK\n{text}"),
                    true,
                ))
                .await;
            assert_eq!(reply["result"], json!({"saved":false,"deferred":true}));
        }
        let list = json!({"op":"list","checkId":CHECK});
        assert_eq!(store.dispatch(&list).await.unwrap()["total"], 0);
        let commit = json!({"op":"commit","checkId":CHECK});
        let saved = store.dispatch(&commit).await.unwrap();
        assert_eq!(saved["saved"], true);
        let entries = store.dispatch(&list).await.unwrap();
        assert_eq!(entries["total"], 1);
        let entry = &entries["entries"][0];
        assert_eq!(entry["startedAt"], 1000);
        assert_eq!(
            entry["durationMs"].as_i64().unwrap(),
            entry["endedAt"].as_i64().unwrap() - 1000
        );
        let text = store
            .dispatch(&json!({"op":"get","entryId":saved["id"]}))
            .await
            .unwrap()["text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(text.contains("second") && !text.contains("first"), "{text}");
        assert_eq!(
            store.dispatch(&commit).await.unwrap(),
            json!({"empty":true})
        );
    }
    #[tokio::test]
    async fn retention_is_global_transactional_reclaims_pages_and_never_purges_new_entry() {
        let data = Data::new();
        let mut store = data.history();
        store.quota = 80_000;
        store.target = 40_000;
        for _ in 0..3 {
            store.save(snapshot("a".repeat(20_000))).await.unwrap();
        }
        let last = store.save(snapshot("b".repeat(20_000))).await.unwrap();
        let list = store
            .dispatch(&json!({"op":"list","checkId":CHECK}))
            .await
            .unwrap();
        assert_eq!(list["total"], 1);
        assert_eq!(list["entries"][0]["id"], last);
        let db = store.open().unwrap();
        assert_eq!(
            db.query_row("PRAGMA freelist_count", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(db);
        let large = store.save(snapshot("c".repeat(60_000))).await.unwrap();
        let list = store
            .dispatch(&json!({"op":"list","checkId":CHECK}))
            .await
            .unwrap();
        assert_eq!(list["total"], 1);
        assert_eq!(list["entries"][0]["id"], large);
        assert!(store.save(snapshot("d".repeat(81_000))).await.is_err());
        assert!(!store.path.with_extension("sqlite3-journal").exists());
        assert!(std::fs::metadata(&store.path).unwrap().len() < 110_000);
    }
    #[tokio::test]
    async fn chunks_use_validated_utf8_byte_offsets_and_reads_are_bounded() {
        let data = Data::new();
        let store = data.history();
        let original = format!("{}🦀{}", "a".repeat(CHUNK - 1), "é".repeat(CHUNK));
        let id = store.save(snapshot(original.clone())).await.unwrap();
        let mut offset = 0;
        let mut text = String::new();
        loop {
            let chunk = store
                .dispatch(&json!({"op":"get","id":id,"offset":offset}))
                .await
                .unwrap();
            let part = chunk["text"].as_str().unwrap();
            assert!(part.len() <= CHUNK);
            text.push_str(part);
            if let Some(next) = chunk["nextOffset"].as_u64() {
                offset = next;
            } else {
                break;
            }
        }
        assert_eq!(text, original);
        for message in [
            json!({"op":"get","id":id,"offset":CHUNK}),
            json!({"op":"get","id":id,"offset":-1}),
            json!({"op":"get","id":id,"offset":original.len()+1}),
            json!({"op":"get","id":0}),
            json!({"op":"get","id":99999}),
            json!({"op":"get"}),
            json!({"op":"list","checkId":"bad"}),
            json!({"op":"list","checkId":CHECK,"before":0}),
            json!({"op":"list","checkId":CHECK,"limit":0}),
        ] {
            assert!(store.dispatch(&message).await.is_err(), "{message}");
        }
        assert_eq!(
            store
                .dispatch(&json!({"op":"get","id":id,"offset":original.len()}))
                .await
                .unwrap()["text"],
            ""
        );
        let alias = store
            .dispatch(&json!({"op":"get","id":99999,"entryId":id}))
            .await
            .unwrap();
        assert_eq!(alias["id"], id);
    }
}
