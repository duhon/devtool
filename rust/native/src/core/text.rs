//! Text shared without copying: module answers pass through the host as the module wrote them.

use bytes::Bytes;
use std::ops::{Deref, Range};

/// Valid UTF-8 backed by shared bytes; clones and slices do not copy.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SharedStr(Bytes);

impl SharedStr {
    pub fn from_utf8(bytes: Bytes) -> Result<Self, std::str::Utf8Error> {
        std::str::from_utf8(&bytes)?;
        Ok(Self(bytes))
    }

    /// Panics unless `range` lies on char boundaries, like `&str` indexing.
    pub fn slice(&self, range: Range<usize>) -> Self {
        let _ = &self[range.clone()];
        Self(self.0.slice(range))
    }
}

impl Deref for SharedStr {
    type Target = str;

    fn deref(&self) -> &str {
        // SAFETY: built from a `String` or validated in `from_utf8`; slices keep char boundaries.
        unsafe { std::str::from_utf8_unchecked(&self.0) }
    }
}

impl From<String> for SharedStr {
    fn from(text: String) -> Self {
        Self(Bytes::from(text))
    }
}

impl From<&'static str> for SharedStr {
    fn from(text: &'static str) -> Self {
        Self(Bytes::from_static(text.as_bytes()))
    }
}

impl std::fmt::Debug for SharedStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&**self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_validated_text() {
        let text = SharedStr::from_utf8(Bytes::from("a🦀b")).unwrap();
        assert_eq!(&*text.slice(1..5), "🦀");
        assert!(SharedStr::from_utf8(Bytes::from_static(b"\xff")).is_err());
        assert!(std::panic::catch_unwind(|| text.slice(1..2)).is_err());
    }
}
