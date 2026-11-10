//! Target lines: a bare URL.

use super::{placeholder, run, token};
use crate::Token;

/// The HTTP methods of a REST `#action`.
pub const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// Tokens of a target line: `scheme://` is a `keyword`, the host `def`, the path
/// `string`. With `http` `{name}` path segments are `rest-path-param` and the query after
/// `?` is `number`.
pub fn tokens(line: &str, http: bool) -> Vec<Token> {
    let mut out = Vec::new();
    let mut pos = 0;
    // 0: before the scheme, 1: the host, 2: the path, 3: the query.
    let mut part = 0;
    while pos < line.len() {
        if let Some(end) = placeholder(line, pos) {
            pos = end;
            continue;
        }
        let rest = &line[pos..];
        let c = rest.chars().next().unwrap_or(' ');
        if c.is_whitespace() {
            pos = run(line, pos, char::is_whitespace);
            continue;
        }
        if part == 0 {
            part = 1;
            if let Some(end) = rest.find("://")
                && end > 0
                && rest[..end]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+.-_".contains(c))
            {
                out.push(token(pos, pos + end + 3, "keyword"));
                pos += end + 3;
                continue;
            }
        }
        if part == 1 {
            part = 2;
            let end = rest.find(['/', '?', '$', '{']).unwrap_or(rest.len());
            if end > 0 {
                out.push(token(pos, pos + end, "def"));
                pos += end;
                continue;
            }
        }
        if http
            && part == 2
            && c == '{'
            && !line[..pos].contains('#')
            && let Some(end) = rest.find('}')
            && end > 1
            && rest[1..end]
                .chars()
                .all(|c| !c.is_whitespace() && !"/?#{}&=".contains(c))
        {
            out.push(token(pos, pos + end + 1, "rest-path-param"));
            pos += end + 1;
            continue;
        }
        if http && c == '?' {
            part = 3;
            pos += 1;
            continue;
        }
        let stop = rest
            .find(|c: char| c == '{' || c == '$' || (http && c == '?' && part == 2))
            .unwrap_or(rest.len());
        let end = if stop == 0 { c.len_utf8() } else { stop };
        out.push(token(
            pos,
            pos + end,
            if part == 3 { "number" } else { "string" },
        ));
        pos += end;
    }
    out
}

/// An absolute URL split into its parts; `{{…}}` placeholders stay as they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Url<'a> {
    pub scheme: &'a str,
    pub authority: &'a str,
    /// The path, `/` when empty.
    pub path: &'a str,
    pub query: &'a str,
}

impl<'a> Url<'a> {
    /// `scheme://authority/path?query#fragment`; `None` without a scheme or an authority,
    /// or with whitespace.
    pub fn parse(url: &'a str) -> Option<Self> {
        if url.contains(char::is_whitespace) {
            return None;
        }
        let (scheme, rest) = url.split_once("://")?;
        let valid = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..end];
        if !valid || authority.is_empty() || authority.ends_with(':') {
            return None;
        }
        let rest = &rest[end..];
        let rest = rest.split('#').next().unwrap_or("");
        let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
        Some(Self {
            scheme,
            authority,
            path: if path.is_empty() { "/" } else { path },
            query,
        })
    }
    /// `scheme://authority` with the scheme lowercased.
    pub fn origin(&self) -> String {
        format!("{}://{}", self.scheme.to_ascii_lowercase(), self.authority)
    }
}

/// The path of a URL without its query or fragment; a URL may also start with a `{{…}}`
/// placeholder that holds its origin.
pub fn path(url: &str) -> &str {
    let rest = match Url::parse(url) {
        Some(url) => return url.path,
        None => placeholder(url, 0).map_or(url, |end| &url[end..]),
    };
    rest.split(['?', '#']).next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::styled;

    fn kinds(line: &str, http: bool, kind: &str) -> Vec<String> {
        styled(line, &tokens(line, http))
            .into_iter()
            .filter(|(_, k)| k == kind)
            .map(|(text, _)| text)
            .collect()
    }

    #[test]
    fn scheme_host_path_and_query() {
        let line = "https://api.test:8080/v1/items?limit=5&q=a";
        assert_eq!(
            styled(line, &tokens(line, true)),
            [
                ("https://", "keyword"),
                ("api.test:8080", "def"),
                ("/v1/items", "string"),
                ("limit=5&q=a", "number"),
            ]
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
        );
        let line = "https://api.test/v1?x=1";
        assert_eq!(
            styled(line, &tokens(line, false)),
            [
                ("https://", "keyword"),
                ("api.test", "def"),
                ("/v1?x=1", "string"),
            ]
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
        );
    }

    #[test]
    fn path_parameters_are_distinct_from_variables_and_query_values() {
        let line = "http://{{var:magento}}/rest/default/V1/categories/{categoryId}/move/{имя-id}?filter={query}";
        assert_eq!(
            kinds(line, true, "rest-path-param"),
            ["{categoryId}", "{имя-id}"]
        );
        assert!(
            styled(line, &tokens(line, true))
                .iter()
                .all(|(text, _)| !text.contains("{{") && !text.contains("magento"))
        );
        for (http, text) in [
            (true, "http://test.com/categories/42/move"),
            (true, "http://test.com/{incomplete"),
            (true, "http://test.com/{}"),
            (true, "http://test.com/{not a name}"),
            (true, "http://test.com/#fragment/{categoryId}"),
            (false, "http://test.com/{categoryId}"),
        ] {
            assert!(
                kinds(text, http, "rest-path-param").is_empty(),
                "{http}: {text}"
            );
        }
    }

    #[test]
    fn tokens_never_split_characters() {
        for line in [
            "\u{a0}https://пример.рф/api/{x}?é={{a}}$b",
            "{{base}}/x",
            "{{",
            "GET",
            "$ { ? é",
        ] {
            for http in [true, false] {
                let mut end = 0;
                for token in tokens(line, http) {
                    let (start, stop) = (token.start as usize, (token.start + token.len) as usize);
                    assert!(start >= end && stop > start, "{line}");
                    assert!(line.is_char_boundary(start) && line.is_char_boundary(stop));
                    end = stop;
                }
            }
        }
    }

    #[test]
    fn urls() {
        let url = Url::parse("HTTP://{{var:host}}:80/a/b?x=1#f").unwrap();
        assert_eq!(url.origin(), "http://{{var:host}}:80");
        assert_eq!((url.path, url.query), ("/a/b", "x=1"));
        assert_eq!(Url::parse("https://h").unwrap().path, "/");
        for bad in ["https://", "https://h:/x", "h/x", "https://a b", "1x://h"] {
            assert_eq!(Url::parse(bad), None, "{bad}");
        }
        assert_eq!(path("{{var:base}}/rest/V1?x"), "/rest/V1");
        assert_eq!(path("https://h/p#x"), "/p");
        assert_eq!(path("/p?q"), "/p");
    }
}
