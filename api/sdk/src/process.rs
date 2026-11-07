//! The Host `process` driver: starts a program in its own process group and reports its output
//! and exit. Cancel sends SIGTERM to the group, SIGKILL after a grace period.

use crate::{http::Timing, wire::DriverCall};
use serde::{Deserialize, Serialize};

pub const DRIVER: &str = "process";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// The executable, found on the Host's `PATH` (the login-shell environment).
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Added to the Host's environment.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Written to stdin, then stdin is closed; without it stdin is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    /// When it passes the group is stopped as on cancel and the call fails at `timeout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Output comes back as text (invalid UTF-8 replaced) instead of base64.
    #[serde(default)]
    pub text: bool,
}

impl Request {
    pub fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(|arg| (*arg).into()).collect(),
            text: true,
            ..Self::default()
        }
    }
    pub fn call(&self) -> DriverCall {
        DriverCall::new(DRIVER, self)
    }
}

/// A call answer, or the end of a stream (then without output: it came as [`Event`]s).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exit {
    pub pid: u32,
    /// `None` when a signal ended the process.
    #[serde(default)]
    pub code: Option<i32>,
    /// `SIGTERM`, `SIGKILL`…
    #[serde(default)]
    pub signal: Option<String>,
    /// stdout or stderr was still open after the exit: something detached keeps them.
    #[serde(default)]
    pub output_incomplete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_base64: Option<String>,
    /// Output past the call limit was dropped (streams have none).
    #[serde(default)]
    pub truncated: bool,
    /// `firstByteUs` is the first output.
    #[serde(default)]
    pub timing: Timing,
}

impl Exit {
    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        Self::deserialize(value).map_err(|error| format!("Invalid process answer: {error}"))
    }
}

/// One piece of a process stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Event {
    Spawned { pid: u32 },
    Stdout(String),
    Stderr(String),
    StdoutBase64(String),
    StderrBase64(String),
}

impl Event {
    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        Self::deserialize(value).map_err(|error| format!("Invalid process event: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_exit_and_events_round_trip() {
        let mut request = Request::new("sh", &["-c", "echo $X"]);
        request.env.push(("X".into(), "1".into()));
        let value = request.call().request();
        assert_eq!(value["env"], json!([["X", "1"]]));
        assert_eq!(Request::deserialize(&value).unwrap(), request);

        let exit = json!({"pid": 7, "code": null, "signal": "SIGTERM", "outputIncomplete": true,
            "stdout": "a", "stderr": "", "truncated": false,
            "timing": {"firstByteUs": 1, "totalUs": 2}});
        let parsed = Exit::parse(&exit).unwrap();
        assert_eq!(parsed.signal.as_deref(), Some("SIGTERM"));
        assert_eq!(serde_json::to_value(&parsed).unwrap(), exit);
        for event in [
            json!({"spawned": {"pid": 7}}),
            json!({"stderrBase64": "AA=="}),
        ] {
            let parsed = Event::parse(&event).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), event);
        }
    }
}
