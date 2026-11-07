//! Texts with `{{…}}` variables resolved through the App (`vars.resolve`), remembered by
//! their raw text, each request carrying why it was made.

use crate::{Cmd, Failure, cmd};
use serde_json::Value;

/// Resolved texts remembered, the most recent last.
const REMEMBERED: usize = 64;
const TAG: &str = "resolve:";

#[derive(Debug)]
pub struct Resolver<P> {
    resolved: Vec<(String, String)>,
    pending: Vec<(u64, String, P)>,
    count: u64,
}

impl<P> Default for Resolver<P> {
    fn default() -> Self {
        Self {
            resolved: vec![],
            pending: vec![],
            count: 0,
        }
    }
}

/// A finished resolution: the raw text, its value or why it failed, and the purpose.
#[derive(Debug, PartialEq)]
pub struct Resolved<P> {
    pub raw: String,
    pub result: Result<String, String>,
    pub purpose: P,
}

impl<P> Resolver<P> {
    /// `raw` as last resolved; itself without variables; `None` while unknown.
    pub fn get<'a>(&'a self, raw: &'a str) -> Option<&'a str> {
        if !raw.contains("{{") {
            return Some(raw);
        }
        self.resolved
            .iter()
            .rev()
            .find(|(text, _)| text == raw)
            .map(|(_, value)| value.as_str())
    }

    /// Whether `raw` is being resolved.
    pub fn pending(&self, raw: &str) -> bool {
        self.pending.iter().any(|(_, text, _)| text == raw)
    }

    /// Asks the App to resolve `raw`.
    pub fn request(&mut self, raw: &str, purpose: P) -> Cmd {
        self.count += 1;
        self.pending.push((self.count, raw.to_owned(), purpose));
        cmd::resolve(format!("{TAG}{}", self.count), raw)
    }

    /// The App's answer; `None` when `tag` is not a resolution's.
    pub fn reply(&mut self, tag: &str, result: Result<Value, Failure>) -> Option<Resolved<P>> {
        let id: u64 = tag.strip_prefix(TAG)?.parse().ok()?;
        let at = self.pending.iter().position(|(n, ..)| *n == id)?;
        let (_, raw, purpose) = self.pending.remove(at);
        let result = match result {
            Ok(Value::String(value)) => Ok(value),
            Ok(other) => Err(format!("Unexpected value {other}")),
            Err(failure) => Err(failure.message),
        };
        if let Ok(value) = &result {
            self.resolved.retain(|(text, _)| *text != raw);
            self.resolved.push((raw.clone(), value.clone()));
            if self.resolved.len() > REMEMBERED {
                self.resolved.remove(0);
            }
        }
        Some(Resolved {
            raw,
            result,
            purpose,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_are_remembered_by_their_raw_form() {
        let mut resolver = Resolver::default();
        assert_eq!(resolver.get("plain"), Some("plain"));
        assert_eq!(resolver.get("{{a}}"), None);
        let cmd = resolver.request("{{a}}", 7);
        assert!(resolver.pending("{{a}}"));
        let tag = cmd.tag.clone();
        assert_eq!(resolver.reply("other", Ok(Value::Null)), None);
        let done = resolver.reply(&tag, Ok(Value::String("x".into()))).unwrap();
        assert_eq!(
            done,
            Resolved {
                raw: "{{a}}".into(),
                result: Ok("x".into()),
                purpose: 7
            }
        );
        assert_eq!(resolver.get("{{a}}"), Some("x"));
        assert!(!resolver.pending("{{a}}"));
    }
}
