//! The console part of a module (its `ui.wasm`): what a service does in the console beyond
//! its manifest data. The console calls it with JSON text through [`dispatch`]; a module
//! exports it with [`export!`].
//!
//! Wire: the console sends `{"op":"check","source"}`, `{"op":"request","sections"}`,
//! `{"op":"import","command"}` or `{"op":"export","params"}` and gets `{"ok":value}` or
//! `{"error":text}` back.

use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

pub mod command;

/// Version of the wasm exports [`export!`] generates; the console checks it first.
pub const ABI_VERSION: u32 = 1;

/// What a service does in the console beyond its manifest data.
pub trait ServiceUi: Sync {
    /// Rejects a `#check` before its sections are read.
    fn check(&self, _source: &str) -> Result<(), String> {
        Ok(())
    }
    /// Request params from the sections of a `#check`.
    fn request(&self, sections: &Sections) -> Result<Value, String>;
    /// A check from a command line (see [`command`]); only called with a manifest `command`.
    fn import(&self, _command: &str) -> Result<Imported, String> {
        Err("Import isn't supported".into())
    }
    /// A command line from request params; only called with a manifest `command`.
    fn export(&self, _params: &Value) -> Result<String, String> {
        Err("Export isn't supported".into())
    }
}

/// The text of each section of a `#check`; text before any marker is `leftover`.
pub struct Sections(pub BTreeMap<String, String>);

impl Sections {
    pub fn text(&self, name: &str) -> &str {
        self.0.get(name).map(String::as_str).unwrap_or("")
    }
    pub fn has(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }
    /// `Name: value` lines of a section as a JSON object.
    pub fn pairs(&self, name: &str) -> Map<String, Value> {
        match pairs(self.text(name)) {
            Value::Object(map) => map,
            _ => Map::new(),
        }
    }
}

/// An imported check: the `#endpoint` line and the sections after it, in order.
pub struct Imported {
    pub endpoint: String,
    pub sections: Vec<(String, String)>,
}

/// `Name: value` lines as a JSON object; `#` lines are skipped.
pub fn pairs(text: &str) -> Value {
    let mut out = Map::new();
    for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        if let Some((key, value)) = line.split_once(':')
            && !key.trim().is_empty()
        {
            out.insert(key.trim().into(), Value::String(value.trim().into()));
        }
    }
    Value::Object(out)
}

/// JSON text pretty-printed; anything else unchanged.
pub fn pretty(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| text.into())
}

/// The parsed command's headers as the `headers` section of an imported check.
pub fn imported_headers(command: &Value) -> Vec<(String, String)> {
    match command["headers"].as_object().filter(|map| !map.is_empty()) {
        Some(headers) => vec![(
            "headers".into(),
            headers
                .iter()
                .map(|(k, v)| format!("{k}: {}", v.as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join("\n"),
        )],
        None => Vec::new(),
    }
}

/// The parsed command's body as the `section` of an imported check, pretty-printed when JSON.
pub fn imported_body(command: &Value, section: &str) -> Vec<(String, String)> {
    let Some(body) = command.get("body") else {
        return Vec::new();
    };
    let b = body
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string_pretty(body).unwrap());
    if b.is_empty() {
        Vec::new()
    } else {
        vec![(section.to_owned(), pretty(&b))]
    }
}

/// A single-quoted shell word.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A `curl` command line; `GET` is implied.
pub fn curl(method: &str, url: &str, headers: Map<String, Value>, body: &str) -> String {
    let mut lines = vec!["curl".to_owned()];
    if method != "GET" {
        lines.push(format!("  --request {method}"));
    }
    lines.push(format!("  --url {}", quote(url)));
    for (k, v) in headers {
        lines.push(format!(
            "  --header {}",
            quote(&format!("{k}: {}", v.as_str().unwrap_or("")))
        ));
    }
    if !body.is_empty() {
        lines.push(format!("  --data {}", quote(body)));
    }
    lines.join(" \\\n")
}

/// Answers one wire request (see the crate docs).
pub fn dispatch(ui: &dyn ServiceUi, request: &str) -> String {
    let answer = (|| -> Result<Value, String> {
        let request: Value = serde_json::from_str(request).map_err(|e| e.to_string())?;
        match request["op"].as_str().unwrap_or("") {
            "check" => ui
                .check(request["source"].as_str().unwrap_or(""))
                .map(|()| Value::Null),
            "request" => {
                let sections = serde_json::from_value(request["sections"].clone())
                    .map_err(|e| e.to_string())?;
                ui.request(&Sections(sections))
            }
            "import" => ui.import(request["command"].as_str().unwrap_or("")).map(
                |imported| json!({"endpoint": imported.endpoint, "sections": imported.sections}),
            ),
            "export" => ui.export(&request["params"]).map(Value::String),
            op => Err(format!("Unknown console call \"{op}\"")),
        }
    })();
    match answer {
        Ok(value) => json!({"ok": value}),
        Err(error) => json!({"error": error}),
    }
    .to_string()
}

/// The console's side of the wire: a [`ServiceUi`] over a `call` that sends a request and
/// returns the module's answer text.
pub struct Remote<F>(pub F);

impl<F: Fn(String) -> Result<String, String> + Sync> Remote<F> {
    fn call(&self, request: Value) -> Result<Value, String> {
        let answer: Value = serde_json::from_str(&(self.0)(request.to_string())?)
            .map_err(|e| format!("Invalid console part answer: {e}"))?;
        match answer.get("error") {
            Some(error) => Err(error.as_str().unwrap_or("Console part failed").to_owned()),
            None => Ok(answer["ok"].clone()),
        }
    }
}

impl<F: Fn(String) -> Result<String, String> + Sync> ServiceUi for Remote<F> {
    fn check(&self, source: &str) -> Result<(), String> {
        self.call(json!({"op": "check", "source": source}))
            .map(drop)
    }
    fn request(&self, sections: &Sections) -> Result<Value, String> {
        self.call(json!({"op": "request", "sections": sections.0}))
    }
    fn import(&self, command: &str) -> Result<Imported, String> {
        let answer = self.call(json!({"op": "import", "command": command}))?;
        let imported: (String, Vec<(String, String)>) =
            serde_json::from_value(json!([answer["endpoint"], answer["sections"]]))
                .map_err(|e| format!("Invalid import: {e}"))?;
        Ok(Imported {
            endpoint: imported.0,
            sections: imported.1,
        })
    }
    fn export(&self, params: &Value) -> Result<String, String> {
        match self.call(json!({"op": "export", "params": params}))? {
            Value::String(command) => Ok(command),
            _ => Err("Invalid export".into()),
        }
    }
}

/// Exports a module's console part from its `ui.wasm`: `export!(MyService)` where the
/// value implements [`ServiceUi`]. The crate is a `cdylib` built for `wasm32-unknown-unknown`;
/// the generated functions exist only there.
///
/// Exports: `services_ui_abi() -> u32`, `services_ui_alloc(len) -> ptr` for a request,
/// `services_ui_call(ptr, len) -> answer` (consumes the request; the answer is a
/// little-endian `u32` length followed by the text) and `services_ui_free(answer)`.
#[macro_export]
macro_rules! export {
    ($ui:expr) => {
        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn services_ui_abi() -> u32 {
            $crate::ABI_VERSION
        }

        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn services_ui_alloc(len: usize) -> *mut u8 {
            ::std::boxed::Box::into_raw(::std::vec![0u8; len].into_boxed_slice()).cast()
        }

        /// # Safety
        /// `pointer` and `len` come from `services_ui_alloc` with `len` bytes written.
        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn services_ui_call(pointer: *mut u8, len: usize) -> *mut u8 {
            let request = unsafe {
                ::std::boxed::Box::from_raw(::std::ptr::slice_from_raw_parts_mut(pointer, len))
            };
            let answer = $crate::dispatch(&$ui, &::std::string::String::from_utf8_lossy(&request));
            let mut framed = ::std::vec::Vec::with_capacity(4 + answer.len());
            framed.extend_from_slice(&(answer.len() as u32).to_le_bytes());
            framed.extend_from_slice(answer.as_bytes());
            ::std::boxed::Box::into_raw(framed.into_boxed_slice()).cast()
        }

        /// # Safety
        /// `answer` comes from `services_ui_call` and is freed once.
        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn services_ui_free(answer: *mut u8) {
            let len = u32::from_le_bytes(unsafe { *(answer as *const [u8; 4]) }) as usize;
            drop(unsafe {
                ::std::boxed::Box::from_raw(::std::ptr::slice_from_raw_parts_mut(answer, 4 + len))
            });
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl ServiceUi for Echo {
        fn check(&self, source: &str) -> Result<(), String> {
            if source.contains("bad") {
                Err("bad check".into())
            } else {
                Ok(())
            }
        }
        fn request(&self, sections: &Sections) -> Result<Value, String> {
            Ok(json!({"endpoint": sections.text("endpoint"), "headers": sections.pairs("headers")}))
        }
        fn import(&self, command: &str) -> Result<Imported, String> {
            let command = command::parse_curl(command)?;
            Ok(Imported {
                endpoint: command["url"].as_str().unwrap_or("").into(),
                sections: [imported_headers(&command), imported_body(&command, "body")].concat(),
            })
        }
    }

    fn remote() -> Remote<impl Fn(String) -> Result<String, String> + Sync> {
        Remote(|request: String| Ok(dispatch(&Echo, &request)))
    }

    #[test]
    fn every_call_crosses_the_wire_unchanged() {
        let remote = remote();
        assert_eq!(remote.check("ok"), Ok(()));
        assert_eq!(remote.check("bad"), Err("bad check".into()));
        let sections = Sections(BTreeMap::from([
            ("endpoint".into(), "https://x".into()),
            ("headers".into(), "A: 1\n# no\nB: two".into()),
        ]));
        assert_eq!(
            remote.request(&sections).unwrap(),
            json!({"endpoint":"https://x","headers":{"A":"1","B":"two"}})
        );
        let imported = remote
            .import(r#"curl https://x -H 'A: 1' -H 'B: 2' --data '{"a":1}'"#)
            .unwrap();
        assert_eq!(imported.endpoint, "https://x");
        assert_eq!(
            imported.sections,
            vec![
                ("headers".to_owned(), "A: 1\nB: 2".to_owned()),
                ("body".to_owned(), "{\n  \"a\": 1\n}".to_owned())
            ]
        );
        assert_eq!(
            remote.import("wget https://x").map(|_| ()),
            Err("Command must start with \"curl\"".into())
        );
        assert_eq!(
            remote.export(&json!({})),
            Err("Export isn't supported".into())
        );
    }

    #[test]
    fn a_broken_request_or_answer_is_an_error() {
        assert_eq!(
            dispatch(&Echo, "{"),
            r#"{"error":"EOF while parsing an object at line 1 column 1"}"#
        );
        assert_eq!(
            dispatch(&Echo, r#"{"op":"x"}"#),
            r#"{"error":"Unknown console call \"x\""}"#
        );
        let broken = Remote(|_: String| Ok("nope".to_owned()));
        assert!(
            broken
                .check("")
                .unwrap_err()
                .starts_with("Invalid console part answer")
        );
    }

    #[test]
    fn curl_quotes_every_word_and_implies_get() {
        let headers = pairs("Accept: */*\nX: it's");
        let Value::Object(headers) = headers else {
            unreachable!()
        };
        assert_eq!(
            curl("POST", "https://x/a b", headers, "{}"),
            "curl \\\n  --request POST \\\n  --url 'https://x/a b' \\\n  --header 'Accept: */*' \\\n  --header 'X: it'\\''s' \\\n  --data '{}'"
        );
        assert_eq!(curl("GET", "u", Map::new(), ""), "curl \\\n  --url 'u'");
    }
}
