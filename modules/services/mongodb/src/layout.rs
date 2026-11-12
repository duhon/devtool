//! Lays a compact snippet out the way JSON completion does: objects and arrays that hold
//! something span lines, and a bare cursor mark sits alone between its brackets.

enum Node {
    Atom(String),
    Object(Vec<(Option<String>, Node)>),
    Array(Vec<Node>),
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn space(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    fn string(&mut self) -> &str {
        let start = self.at;
        self.at += 1;
        while let Some(b) = self.peek() {
            self.at += 1;
            match b {
                b'\\' => self.at += 1,
                b'"' => break,
                _ => {}
            }
        }
        &self.text[start..self.at.min(self.text.len())]
    }

    fn value(&mut self) -> Node {
        self.space();
        match self.peek() {
            Some(b'{') => {
                self.at += 1;
                let mut entries = Vec::new();
                loop {
                    self.space();
                    match self.peek() {
                        None => break,
                        Some(b'}') => {
                            self.at += 1;
                            break;
                        }
                        Some(b',') => self.at += 1,
                        Some(_) => entries.push(self.entry()),
                    }
                }
                Node::Object(entries)
            }
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.space();
                    match self.peek() {
                        None => break,
                        Some(b']') => {
                            self.at += 1;
                            break;
                        }
                        Some(b',') => self.at += 1,
                        Some(_) => items.push(self.value()),
                    }
                }
                Node::Array(items)
            }
            _ => Node::Atom(self.atom()),
        }
    }

    fn entry(&mut self) -> (Option<String>, Node) {
        if self.peek() == Some(b'"') {
            let key = self.string().to_owned();
            self.space();
            if self.peek() == Some(b':') {
                self.at += 1;
                return (Some(key), self.value());
            }
            return (None, Node::Atom(key));
        }
        (None, self.value())
    }

    fn atom(&mut self) -> String {
        let start = self.at;
        while let Some(b) = self.peek() {
            match b {
                b',' | b'}' | b']' => break,
                b'"' => {
                    self.string();
                }
                _ => self.at += 1,
            }
        }
        self.text[start..self.at].trim().to_owned()
    }
}

fn render(node: &Node, indent: &str) -> String {
    let inner = format!("{indent}  ");
    match node {
        Node::Atom(text) => text.clone(),
        Node::Object(entries) if entries.is_empty() => "{}".into(),
        Node::Array(items) if items.is_empty() => "[]".into(),
        Node::Object(entries) => {
            let lines: Vec<String> = entries
                .iter()
                .map(|(key, value)| match key {
                    Some(key) => format!("{inner}{key}: {}", render(value, &inner)),
                    None => format!("{inner}{}", render(value, &inner)),
                })
                .collect();
            format!("{{\n{}\n{indent}}}", lines.join(",\n"))
        }
        Node::Array(items)
            if items
                .iter()
                .all(|item| matches!(item, Node::Atom(text) if text != "$0")) =>
        {
            let texts: Vec<&str> = items
                .iter()
                .filter_map(|item| match item {
                    Node::Atom(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            format!("[{}]", texts.join(", "))
        }
        Node::Array(items) => {
            let lines: Vec<String> = items
                .iter()
                .map(|item| format!("{inner}{}", render(item, &inner)))
                .collect();
            format!("[\n{}\n{indent}]", lines.join(",\n"))
        }
    }
}

/// `snippet` laid out across lines for a line indented by `indent`.
pub fn pretty(snippet: &str, indent: &str) -> String {
    let mut parser = Parser {
        text: snippet,
        at: 0,
    };
    render(&parser.value(), indent)
}

/// An item's text with the object or array it ends with laid out; text without one is
/// unchanged.
pub fn lay_out(insert: &str, indent: &str) -> String {
    let start = if insert.starts_with(['{', '[']) {
        0
    } else {
        match insert.find("\": {").or_else(|| insert.find("\": [")) {
            Some(at) => at + 3,
            None => return insert.to_owned(),
        }
    };
    format!("{}{}", &insert[..start], pretty(&insert[start..], indent))
}
