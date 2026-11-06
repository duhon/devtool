//! Helpers shared by the `core` and `modules` UI test binaries.

use services_ui::syntax::{Language, State, token};

/// Splits `text` into the (fragment, style) runs the editor colors.
pub fn tokens(
    lang: Language,
    text: &str,
    state: &mut State,
) -> Vec<(String, Option<&'static str>)> {
    tokens_for(lang, text, state)
}

/// Like [`tokens`] for a language variant (e.g. a REST endpoint field).
pub fn tokens_for(
    lang: Language,
    text: &str,
    state: &mut State,
) -> Vec<(String, Option<&'static str>)> {
    let chars: Vec<_> = text.chars().collect();
    let mut pos = 0;
    let mut out = vec![];
    while pos < chars.len() {
        let (n, style) = token(lang, text, pos, state);
        assert!(n > 0);
        out.push((
            chars[pos..(pos + n).min(chars.len())].iter().collect(),
            style,
        ));
        pos += n;
    }
    out
}
