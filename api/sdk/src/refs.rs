//! Variable data and `{{ }}` references.
//!
//! Variables are strings or hashes (`Name: value` lines with indented nested hashes). Every
//! panel is a first-level key of one tree ([`Root`]); a reference is an absolute (`{{.var.x}}`)
//! or relative (`{{:x}}`) path in it, a template parameter (`{{name}}`), a template call
//! (`{{@name(args)}}`) or a jq function call (`{{$name(args)}}`).

use serde_json::{Map, Value};
use std::borrow::Cow;
use std::fmt;

const MAX_DEPTH: usize = 16;
const COMMENT_SUFFIX: char = '\u{1}';

fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn is_name(text: &str) -> bool {
    !text.is_empty() && text.chars().all(word)
}

fn comment(key: &str) -> bool {
    key.starts_with('#')
}

// ---------------------------------------------------------------------------------------------
// Editor text: `Name: value` lines and indented hashes
// ---------------------------------------------------------------------------------------------

struct Line<'a> {
    number: usize,
    indent: usize,
    text: &'a str,
}

fn block(lines: &[Line], pos: &mut usize, indent: usize) -> Result<Map<String, Value>, String> {
    let mut map = Map::new();
    while let Some(line) = lines.get(*pos) {
        if line.text.starts_with('#') {
            // Comments are keys; repeated ones get a hidden suffix that `format_lines` drops.
            let mut key = line.text.to_owned();
            let mut n = 1;
            while map.contains_key(&key) {
                key = format!("{}{COMMENT_SUFFIX}{n}", line.text);
                n += 1;
            }
            map.insert(key, "".into());
            *pos += 1;
            continue;
        }
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return Err(format!("Line {}: inconsistent indentation.", line.number));
        }
        let Some((name, value)) = line.text.split_once(':') else {
            return Err(format!("Line {}: expected NAME: value.", line.number));
        };
        let name = name.trim();
        if name.is_empty() {
            return Err(format!("Line {}: expected NAME: value.", line.number));
        }
        if !is_name(name) {
            return Err(format!(
                "Line {}: invalid name \"{name}\" (use letters, digits, _ and -).",
                line.number
            ));
        }
        if map.contains_key(name) {
            return Err(format!("Line {}: duplicate variable {name}.", line.number));
        }
        *pos += 1;
        let value = value.trim();
        if !value.is_empty() {
            map.insert(name.into(), value.into());
            continue;
        }
        let child = lines[*pos..]
            .iter()
            .find(|next| !next.text.starts_with('#'))
            .filter(|next| next.indent > line.indent);
        match child {
            Some(child) => {
                let nested = block(lines, pos, child.indent)?;
                map.insert(name.into(), Value::Object(nested));
            }
            None => {
                map.insert(name.into(), "".into());
            }
        }
    }
    Ok(map)
}

/// Variables written as `NAME: value` lines; a `NAME:` line followed by deeper-indented lines is a
/// hash. `#` lines are comments, kept in place.
pub fn parse_lines(text: &str) -> Result<Map<String, Value>, String> {
    let lines: Vec<Line> = text
        .lines()
        .enumerate()
        .filter_map(|(index, raw)| {
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| Line {
                number: index + 1,
                indent: raw.len() - raw.trim_start().len(),
                text: trimmed,
            })
        })
        .collect();
    let Some(first) = lines.first() else {
        return Ok(Map::new());
    };
    let mut pos = 0;
    let map = block(&lines, &mut pos, first.indent)?;
    if let Some(line) = lines.get(pos) {
        return Err(format!("Line {}: inconsistent indentation.", line.number));
    }
    Ok(map)
}

fn format_block(vars: &Map<String, Value>, depth: usize, out: &mut Vec<String>) {
    let pad = "  ".repeat(depth);
    for (name, value) in vars {
        if comment(name) {
            let name = name.split(COMMENT_SUFFIX).next().unwrap_or(name);
            out.push(format!("{pad}{name}"));
            continue;
        }
        match value {
            Value::Object(nested) => {
                out.push(format!("{pad}{name}:"));
                format_block(nested, depth + 1, out);
            }
            other => out.push(format!("{pad}{name}: {}", text(other))),
        }
    }
}

/// `vars` as [`parse_lines`] reads them (nested hashes indented by two spaces).
pub fn format_lines(vars: &Map<String, Value>) -> String {
    let mut out = Vec::new();
    format_block(vars, 0, &mut out);
    out.join("\n")
}

/// The value of a variable as text.
pub fn text(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

// ---------------------------------------------------------------------------------------------
// Reference grammar
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Key(String),
    Star,
    DoubleStar,
}

impl fmt::Display for Segment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Segment::Key(key) => f.write_str(key),
            Segment::Star => f.write_str("*"),
            Segment::DoubleStar => f.write_str("**"),
        }
    }
}

/// Segments plus a trailing `~` (`keys`: the keys of the selected nodes instead of their values).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Path {
    pub segments: Vec<Segment>,
    pub keys: bool,
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.segments.iter().map(Segment::to_string).collect();
        f.write_str(&parts.join("."))?;
        if self.keys {
            f.write_str("~")?;
        }
        Ok(())
    }
}

impl Path {
    fn parse(text: &str) -> Option<Path> {
        let (body, keys) = match text.strip_suffix('~') {
            Some(body) => (body, true),
            None => (text, false),
        };
        let mut segments = Vec::new();
        if !body.is_empty() {
            for part in body.split('.') {
                segments.push(match part {
                    "*" => Segment::Star,
                    "**" => Segment::DoubleStar,
                    key if is_name(key) => Segment::Key(key.to_owned()),
                    _ => return None,
                });
            }
        }
        Some(Path { segments, keys })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ref {
    /// `{{.a.b}}`, from the root.
    Absolute(Path),
    /// `{{:a.b}}`, from the root of a panel.
    Relative(Path),
    /// `{{name}}`, a template parameter.
    Param(String),
    /// `{{@name(args)}}`; `args_text` is the text between the parentheses, unparsed.
    Call { name: String, args_text: String },
    /// `{{$name(args)}}`, a jq function of the Functions panel; `args_text` as for a call.
    /// `eager` for `{{$!name(args)}}`, which the editor replaces by its result on Enter (or
    /// when a template expands); left in the text it resolves like `{{$name(args)}}`.
    Function {
        name: String,
        args_text: String,
        eager: bool,
    },
    /// `{{.a.{{:b}}}}`: a variable path built from other references, the text between the
    /// outer braces; it is resolved first, then read as a variable.
    Dynamic(String),
}

impl fmt::Display for Ref {
    /// The reference as written, with the braces.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ref::Absolute(path) => write!(f, "{{{{.{path}}}}}"),
            Ref::Relative(path) => write!(f, "{{{{:{path}}}}}"),
            Ref::Param(name) => write!(f, "{{{{{name}}}}}"),
            Ref::Call { name, args_text } => write!(f, "{{{{@{name}({args_text})}}}}"),
            Ref::Function {
                name,
                args_text,
                eager,
            } => {
                let bang = if *eager { "!" } else { "" };
                write!(f, "{{{{${bang}{name}({args_text})}}}}")
            }
            Ref::Dynamic(body) => write!(f, "{{{{{body}}}}}"),
        }
    }
}

/// A reference found in a text; `start..end` covers the braces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub start: usize,
    pub end: usize,
    pub reference: Ref,
}

/// The end of the call arguments in `tail` (just after `(`): the offset of `)` that is followed
/// by `}}` outside quoted strings.
fn call_end(tail: &str) -> Option<usize> {
    let bytes = tail.as_bytes();
    let (mut quoted, mut escaped) = (false, false);
    for (index, &byte) in bytes.iter().enumerate() {
        if quoted {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => quoted = false,
                _ => {}
            }
        } else if byte == b'"' {
            quoted = true;
        } else if byte == b')' && tail[index + 1..].starts_with("}}") {
            return Some(index);
        }
    }
    None
}

/// The offset in `tail` (just after `{{`) of the `}}` that closes it, past nested `{{…}}`
/// (an escaped `\{{` opens none).
pub fn closing(tail: &str) -> Option<usize> {
    let (mut index, mut depth) = (0, 0usize);
    while index < tail.len() {
        let rest = &tail[index..];
        if rest.starts_with("{{") && !tail[..index].ends_with('\\') {
            depth += 1;
            index += 2;
        } else if rest.starts_with("}}") {
            if depth == 0 {
                return Some(index);
            }
            depth -= 1;
            index += 2;
        } else {
            index += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    None
}

/// The reference at the start of `tail` (just after `{{`) and its length up to and including `}}`.
fn at(tail: &str) -> Option<(Ref, usize)> {
    if let Some(call) = tail.strip_prefix(['@', '$']) {
        let eager = tail.starts_with('$') && call.starts_with('!');
        let call = if eager { &call[1..] } else { call };
        let sigil = 1 + usize::from(eager);
        let name_end = call.find(|c| !word(c))?;
        let name = &call[..name_end];
        if name.is_empty() {
            return None;
        }
        let made = |args_text: &str| {
            let (name, args_text) = (name.to_owned(), args_text.to_owned());
            if tail.starts_with('@') {
                Ref::Call { name, args_text }
            } else {
                Ref::Function {
                    name,
                    args_text,
                    eager,
                }
            }
        };
        let rest = &call[name_end..];
        if rest.starts_with("}}") {
            return Some((made(""), sigil + name_end + 2));
        }
        let args = rest.strip_prefix('(')?;
        let end = call_end(args)?;
        return Some((made(&args[..end]), sigil + name_end + 1 + end + 1 + 2));
    }
    let end = tail.find("}}")?;
    let body = &tail[..end];
    let nested = body
        .match_indices("{{")
        .any(|(index, _)| !escaped(body, index));
    if body.starts_with(['.', ':']) && nested {
        let end = closing(tail)?;
        return Some((Ref::Dynamic(tail[..end].to_owned()), end + 2));
    }
    let reference = if let Some(path) = body.strip_prefix('.') {
        Ref::Absolute(Path::parse(path)?)
    } else if let Some(path) = body.strip_prefix(':') {
        let path = Path::parse(path)?;
        if path.segments.is_empty() {
            return None;
        }
        Ref::Relative(path)
    } else if is_name(body) {
        Ref::Param(body.into())
    } else {
        return None;
    };
    Some((reference, end + 2))
}

/// Whether the `{{` at byte `start` of `text` is escaped as `\{{`: literal text, no reference.
pub fn escaped(text: &str, start: usize) -> bool {
    text[..start].ends_with('\\')
}

/// `text` with each `\{{` written as the literal `{{` it stands for.
pub fn unescape(text: &str) -> String {
    text.replace("\\{{", "{{")
}

/// Every reference of `text` with its byte span, in order; an escaped `\{{` starts none.
pub fn scan(text: &str) -> Vec<Found> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = text[from..].find("{{") {
        let start = from + offset;
        if escaped(text, start) {
            from = start + 2;
            continue;
        }
        match at(&text[start + 2..]) {
            Some((reference, length)) => {
                let end = start + 2 + length;
                found.push(Found {
                    start,
                    end,
                    reference,
                });
                from = end;
            }
            None => from = start + 2,
        }
    }
    found
}

/// Every reference of `text`, in order.
pub fn find(text: &str) -> Vec<Ref> {
    scan(text).into_iter().map(|f| f.reference).collect()
}

/// Replaces each reference of `text` for which `resolve` gives `Some`; the others stay.
pub fn replace(
    text: &str,
    mut resolve: impl FnMut(&Ref) -> Result<Option<String>, String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for found in scan(text) {
        out.push_str(&text[last..found.start]);
        match resolve(&found.reference)? {
            Some(value) => out.push_str(&value),
            None => out.push_str(&text[found.start..found.end]),
        }
        last = found.end;
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// `value` as the content of a JSON string.
pub fn escape_json(value: &str) -> String {
    let quoted = Value::String(value.into()).to_string();
    quoted[1..quoted.len() - 1].to_owned()
}

/// Names whose values never show in History: they are masked wherever they appear.
pub fn sensitive(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "pwd",
        "token",
        "secret",
        "credential",
        "api_key",
        "apikey",
        "api-key",
        "authorization",
        "cookie",
        "private_key",
    ]
    .iter()
    .any(|part| name.contains(part))
}

// ---------------------------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------------------------

/// Every panel as a first-level key, in priority order (`var`, the sets, `env`).
#[derive(Debug, Clone, Default)]
pub struct Root {
    panels: Vec<(String, Map<String, Value>)>,
    tree: Value,
}

impl Root {
    pub fn new(panels: Vec<(String, Map<String, Value>)>) -> Root {
        let mut tree = Map::new();
        for (name, vars) in &panels {
            tree.insert(name.clone(), Value::Object(vars.clone()));
        }
        Root {
            panels,
            tree: Value::Object(tree),
        }
    }

    pub fn panels(&self) -> &[(String, Map<String, Value>)] {
        &self.panels
    }

    /// Every panel as a key of one object, as written (comments and references kept).
    pub fn tree(&self) -> &Value {
        &self.tree
    }
}

/// `value` without its comment keys, at every depth.
pub fn without_comments(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !comment(key))
                .map(|(key, value)| (key.clone(), without_comments(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// One selected node: its path from the root (the panel first) and its value (for `~`, a key).
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub path: Vec<String>,
    pub value: Value,
}

impl Item {
    /// Whether the value lives under a `sensitive` key.
    pub fn secret(&self) -> bool {
        self.path.iter().any(|segment| sensitive(segment))
    }
}

/// The nodes a reference selects, de-duplicated, in order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Selection {
    pub items: Vec<Item>,
    /// The path was relative: error messages leave the panel out.
    pub relative: bool,
    /// The path had `**`: list context collects string leaves only.
    pub deep: bool,
}

type Node<'a> = (Vec<String>, Cow<'a, Value>);

fn children<'a>(node: &'a Node) -> Vec<Node<'a>> {
    let Value::Object(map) = node.1.as_ref() else {
        return Vec::new();
    };
    // the lifetime of the borrowed value is the node's own
    map.iter()
        .filter(|(key, _)| !comment(key))
        .map(|(key, value)| {
            let mut path = node.0.clone();
            path.push(key.clone());
            (path, Cow::Owned(value.clone()))
        })
        .collect()
}

fn descendants(node: Node, out: &mut Vec<Node<'static>>) {
    let own: Node<'static> = (node.0.clone(), Cow::Owned(node.1.clone().into_owned()));
    let kids = children(&node);
    out.push(own);
    for kid in kids {
        descendants(kid, out);
    }
}

fn select(start: Vec<Node<'static>>, path: &Path) -> Selection {
    let mut nodes = start;
    let mut deep = false;
    for segment in &path.segments {
        let mut next: Vec<Node<'static>> = Vec::new();
        for node in &nodes {
            match segment {
                Segment::Key(key) => {
                    if let Value::Object(map) = node.1.as_ref()
                        && let Some(value) = map.get(key)
                    {
                        let mut path = node.0.clone();
                        path.push(key.clone());
                        next.push((path, Cow::Owned(value.clone())));
                    }
                }
                Segment::Star => {
                    for (path, value) in children(node) {
                        next.push((path, Cow::Owned(value.into_owned())));
                    }
                }
                Segment::DoubleStar => {
                    deep = true;
                    descendants((node.0.clone(), node.1.clone()), &mut next);
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        next.retain(|(path, _)| seen.insert(path.clone()));
        nodes = next;
    }
    if path.keys {
        let mut keys: Vec<Node<'static>> = Vec::new();
        for node in &nodes {
            for (path, _) in children(node) {
                let key = path.last().cloned().unwrap_or_default();
                keys.push((path, Cow::Owned(Value::String(key))));
            }
        }
        nodes = keys;
    }
    Selection {
        items: nodes
            .into_iter()
            .map(|(path, value)| Item {
                path,
                value: value.into_owned(),
            })
            .collect(),
        relative: false,
        deep,
    }
}

/// The nodes `path` selects from the root.
pub fn eval_absolute(root: &Root, path: &Path) -> Selection {
    select(vec![(Vec::new(), Cow::Owned(root.tree.clone()))], path)
}

/// The nodes `path` selects from the first panel (in priority order) where it selects any.
pub fn eval_relative(root: &Root, path: &Path) -> Selection {
    for (name, vars) in &root.panels {
        let start = vec![(vec![name.clone()], Cow::Owned(Value::Object(vars.clone())))];
        let mut selection = select(start, path);
        if !selection.items.is_empty() {
            selection.relative = true;
            return selection;
        }
    }
    Selection {
        relative: true,
        ..Selection::default()
    }
}

/// The nodes an absolute or relative reference selects; `None` for the other references.
pub fn eval(root: &Root, reference: &Ref) -> Option<Selection> {
    match reference {
        Ref::Absolute(path) => Some(eval_absolute(root, path)),
        Ref::Relative(path) => Some(eval_relative(root, path)),
        _ => None,
    }
}

impl Selection {
    fn shown(&self, item: &Item) -> String {
        let path = if self.relative {
            &item.path[1.min(item.path.len())..]
        } else {
            &item.path[..]
        };
        path.join(".")
    }

    /// Whether any selected value is under a `sensitive` key.
    pub fn secret(&self) -> bool {
        self.items.iter().any(Item::secret)
    }
}

/// The single string the selection holds (string context).
pub fn as_string(selection: &Selection) -> Result<String, String> {
    match selection.items.as_slice() {
        [] => Err("expected a string, got nothing".into()),
        [item] => match &item.value {
            Value::Object(_) => Err("expected a string, got a hash".into()),
            value => Ok(text(value)),
        },
        items => Err(format!("expected a string, got a list ({})", items.len())),
    }
}

/// One option of a list: the hash key it came from (if any) and the string value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    pub key: Option<String>,
    pub value: String,
}

/// The strings the selection holds (list context): a hash gives its values, `**` collects string
/// leaves, with several nodes the strings are skipped when any is a hash.
pub fn as_list(selection: &Selection) -> Result<Vec<ListItem>, String> {
    let mut out: Vec<ListItem> = Vec::new();
    let mut push = |key: Option<String>, value: String| {
        if !out
            .iter()
            .any(|item| item.key == key && item.value == value)
        {
            out.push(ListItem { key, value });
        }
    };
    if selection.deep {
        for item in &selection.items {
            if !matches!(item.value, Value::Object(_)) {
                push(item.path.last().cloned(), text(&item.value));
            }
        }
        return Ok(out);
    }
    let hashes = selection
        .items
        .iter()
        .filter(|item| item.value.is_object())
        .count();
    if hashes == 0 {
        for item in &selection.items {
            push(None, text(&item.value));
        }
        return Ok(out);
    }
    for item in &selection.items {
        let Value::Object(map) = &item.value else {
            continue;
        };
        for (key, value) in map.iter().filter(|(key, _)| !comment(key)) {
            if value.is_object() {
                return Err(format!("{}.{key} is a hash", selection.shown(item)));
            }
            push(Some(key.clone()), text(value));
        }
    }
    Ok(out)
}

/// A string leaf of a panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leaf {
    pub panel: String,
    /// The path inside the panel, like `hosts.catalog`.
    pub path: String,
    pub value: String,
}

fn collect(panel: &str, prefix: &str, vars: &Map<String, Value>, out: &mut Vec<Leaf>) {
    for (key, value) in vars.iter().filter(|(key, _)| !comment(key)) {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Object(nested) => collect(panel, &path, nested, out),
            value => out.push(Leaf {
                panel: panel.into(),
                path,
                value: text(value),
            }),
        }
    }
}

/// Every string leaf of every panel, in priority order.
pub fn leaves(root: &Root) -> Vec<Leaf> {
    let mut out = Vec::new();
    for (panel, vars) in root.panels() {
        collect(panel, "", vars, &mut out);
    }
    out
}

/// A child of a node, for tree navigation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    pub key: String,
    /// The string value of a leaf; `None` for a hash.
    pub value: Option<String>,
}

/// The children of the node at the absolute `path` (`""` is the root, `dev.hosts` a hash).
pub fn children_at(root: &Root, path: &str) -> Vec<Child> {
    let mut segments = Vec::new();
    if !path.is_empty() {
        for part in path.split('.') {
            if !is_name(part) {
                return Vec::new();
            }
            segments.push(Segment::Key(part.to_owned()));
        }
    }
    let selection = eval_absolute(
        root,
        &Path {
            segments,
            keys: false,
        },
    );
    let [item] = selection.items.as_slice() else {
        return Vec::new();
    };
    let Value::Object(map) = &item.value else {
        return Vec::new();
    };
    map.iter()
        .filter(|(key, _)| !comment(key))
        .map(|(key, value)| Child {
            key: key.clone(),
            value: (!value.is_object()).then(|| text(value)),
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------------------------

/// The arguments of a call, split on the commas outside quoted literals and `{{ }}`.
fn split_arguments(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut quoted, mut escaped, mut depth, mut start) = (false, false, 0usize, 0);
    for (index, c) in text.char_indices() {
        if quoted {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => quoted = false,
                _ => {}
            }
        } else {
            match c {
                '"' => quoted = true,
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    parts.push(&text[start..index]);
                    start = index + 1;
                }
                _ => {}
            }
        }
    }
    parts.push(&text[start..]);
    parts
}

/// A whole `"…"` literal with its `\"` and `\\` escapes.
fn parse_literal(text: &str) -> Result<String, String> {
    let mut value = String::new();
    let mut chars = text[1..].chars();
    loop {
        match chars.next() {
            None => return Err(format!("unterminated literal {text}")),
            Some('"') if chars.as_str().is_empty() => return Ok(value),
            Some('"') => return Err(format!("unexpected text after the literal in `{text}`")),
            Some('\\') => match chars.next() {
                Some(c @ ('"' | '\\')) => value.push(c),
                _ => return Err(format!("only \\\" and \\\\ escapes are allowed in {text}")),
            },
            Some(c) => value.push(c),
        }
    }
}

/// The text with every reference resolved and the secret values it holds (to mask in History).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub text: String,
    pub secrets: Vec<String>,
}

/// Runs the function `name` on its arguments; gives its result as text.
pub type Function<'a> = dyn FnMut(&str, &[Value]) -> Result<String, String> + 'a;

struct Resolver<'a> {
    root: &'a Root,
    expand: &'a mut dyn FnMut(&str, &str) -> Result<String, String>,
    function: &'a mut Function<'a>,
    chain: Vec<String>,
    secrets: Vec<String>,
}

impl Resolver<'_> {
    fn enter(&mut self, link: String) -> Result<(), String> {
        if self.chain.contains(&link) {
            return Err(format!(
                "Circular variable reference: {} -> {link}",
                self.chain.join(" -> ")
            ));
        }
        if self.chain.len() >= MAX_DEPTH {
            return Err(format!(
                "Variable references are nested more than {MAX_DEPTH} levels deep: {} -> {link}",
                self.chain.join(" -> ")
            ));
        }
        self.chain.push(link);
        Ok(())
    }

    /// The values of a function call's arguments: a `"literal"` with its references resolved
    /// is a string; a `:path`/`.path` is the string it selects, a hash as a JSON object, or
    /// an array when it selects several nodes.
    fn arguments(&mut self, args_text: &str) -> Result<Vec<Value>, String> {
        let mut values = Vec::new();
        if args_text.trim().is_empty() {
            return Ok(values);
        }
        for arg in split_arguments(args_text) {
            let arg = arg.trim();
            if arg.starts_with('"') {
                let literal = parse_literal(arg)?;
                values.push(Value::String(self.text(&literal, false)?));
                continue;
            }
            let reference = match arg.chars().next() {
                Some('.') => Path::parse(&arg[1..]).map(Ref::Absolute),
                Some(':') => Path::parse(&arg[1..])
                    .filter(|path| !path.segments.is_empty())
                    .map(Ref::Relative),
                _ if is_name(arg) => {
                    return Err(format!(
                        "`{arg}` is not a parameter in scope (use \"{arg}\" for text or :{arg} for a variable)"
                    ));
                }
                _ => None,
            }
            .ok_or_else(|| {
                format!("invalid argument `{arg}`: use \"literal\", :path or .path")
            })?;
            let selection = eval(self.root, &reference).unwrap_or_default();
            if selection.items.is_empty() {
                return Err(format!("Unknown variable {{{{{arg}}}}}"));
            }
            let mut items = Vec::new();
            for item in &selection.items {
                items.push(match &item.value {
                    Value::String(value) => {
                        let resolved = self.text(value, false)?;
                        if item.secret() {
                            self.secrets.push(resolved.clone());
                        }
                        Value::String(resolved)
                    }
                    other => without_comments(other),
                });
            }
            values.push(match <[Value; 1]>::try_from(items) {
                Ok([value]) => value,
                Err(items) => Value::Array(items),
            });
        }
        Ok(values)
    }

    /// The secret values of the tree that `result` holds: a function may read any of them.
    fn secrets_in(&mut self, result: &str) {
        for leaf in leaves(self.root) {
            let secret = sensitive(&leaf.panel) || leaf.path.split('.').any(sensitive);
            if secret && !leaf.value.is_empty() && result.contains(&leaf.value) {
                self.secrets.push(leaf.value);
            }
        }
    }

    fn text(&mut self, text: &str, json: bool) -> Result<String, String> {
        let mut out = String::with_capacity(text.len());
        let mut unknown: Vec<String> = Vec::new();
        let mut last = 0;
        for found in scan(text) {
            out.push_str(&unescape(&text[last..found.start]));
            last = found.end;
            let mut shown = found.reference.to_string();
            let mut reference = found.reference;
            if let Ref::Dynamic(body) = &reference {
                self.enter(shown.clone())?;
                let path = self.text(body, false)?;
                self.chain.pop();
                let built = format!("{{{{{path}}}}}");
                reference = match at(&built[2..]) {
                    Some((found @ (Ref::Absolute(_) | Ref::Relative(_)), length))
                        if length == path.len() + 2 =>
                    {
                        found
                    }
                    _ => return Err(format!("{shown}: `{built}` is not a variable reference")),
                };
                shown = built;
            }
            match &reference {
                Ref::Param(_) | Ref::Dynamic(_) => {
                    return Err(format!("Unknown reference {shown}"));
                }
                Ref::Function {
                    name, args_text, ..
                } => {
                    self.enter(shown.clone())?;
                    let args = self
                        .arguments(args_text)
                        .map_err(|e| format!("{shown}: {e}"))?;
                    self.chain.pop();
                    let result =
                        (self.function)(name, &args).map_err(|e| format!("{shown}: {e}"))?;
                    self.secrets_in(&result);
                    out.push_str(&if json { escape_json(&result) } else { result });
                }
                Ref::Call { name, args_text } => {
                    let body =
                        (self.expand)(name, args_text).map_err(|e| format!("{shown}: {e}"))?;
                    self.enter(shown)?;
                    let resolved = self.text(&body, json)?;
                    self.chain.pop();
                    out.push_str(&resolved);
                }
                reference => {
                    let selection = eval(self.root, reference).unwrap_or_default();
                    if selection.items.is_empty() {
                        unknown.push(shown);
                        continue;
                    }
                    let value = as_string(&selection).map_err(|e| format!("{shown}: {e}"))?;
                    self.enter(shown)?;
                    let resolved = self.text(&value, false)?;
                    self.chain.pop();
                    if selection.secret() {
                        self.secrets.push(resolved.clone());
                    }
                    out.push_str(&if json {
                        escape_json(&resolved)
                    } else {
                        resolved
                    });
                }
            }
        }
        out.push_str(&unescape(&text[last..]));
        if !unknown.is_empty() {
            let plural = if unknown.len() == 1 { "" } else { "s" };
            return Err(format!("Unknown variable{plural} {}", unknown.join(", ")));
        }
        Ok(out)
    }
}

/// Resolves every reference of `text`: variables in string context (values are resolved
/// recursively), template calls through `expand(name, args_text)` (the expansion is resolved
/// too). With `json` the variable values are escaped for a JSON string. A function call
/// `{{$name(args)}}` fails; [`resolve_with`] runs them.
pub fn resolve(
    text: &str,
    root: &Root,
    json: bool,
    expand_call: &mut dyn FnMut(&str, &str) -> Result<String, String>,
) -> Result<Resolution, String> {
    resolve_with(text, root, json, expand_call, &mut |name, _| {
        Err(format!("Unknown function ${name}"))
    })
}

/// [`resolve`] that runs each function call `{{$name(args)}}` through `function(name, args)`.
/// An argument is a JSON value: a `"literal"` (its references resolved) or the string a
/// `:path`/`.path` selects is a string, a selected hash an object, several nodes an array.
/// The result is data: inserted as it is (escaped with `json`), not resolved again; the
/// secret values of the tree it holds are masked in History.
pub fn resolve_with(
    text: &str,
    root: &Root,
    json: bool,
    expand_call: &mut dyn FnMut(&str, &str) -> Result<String, String>,
    function: &mut Function<'_>,
) -> Result<Resolution, String> {
    let mut resolver = Resolver {
        root,
        expand: expand_call,
        function,
        chain: Vec::new(),
        secrets: Vec::new(),
    };
    let text = resolver.text(text, json)?;
    resolver.secrets.dedup();
    Ok(Resolution {
        text,
        secrets: resolver.secrets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(text: &str) -> Map<String, Value> {
        parse_lines(text).unwrap()
    }

    fn root(panels: &[(&str, &str)]) -> Root {
        Root::new(
            panels
                .iter()
                .map(|(n, t)| (n.to_string(), vars(t)))
                .collect(),
        )
    }

    fn no_calls(_: &str, _: &str) -> Result<String, String> {
        Err("no calls".into())
    }

    fn run(text: &str, root: &Root) -> Result<String, String> {
        resolve(text, root, false, &mut no_calls).map(|r| r.text)
    }

    fn envs() -> Root {
        root(&[(
            "dev",
            "envs:\n  qa:\n    1: catalog\n    2: checkout\n  prod:\n    1: search\n  stage: test",
        )])
    }

    fn list(root: &Root, source: &str) -> Result<Vec<String>, String> {
        let found = scan(source).remove(0);
        let selection = eval(root, &found.reference).unwrap();
        as_list(&selection).map(|l| l.into_iter().map(|i| i.value).collect())
    }

    #[test]
    fn lines_round_trip_with_hashes() {
        let text = "# note\ntype: qa\nservices:\n  1: catalog\n  2: checkout\nenvs:\n  qa:\n    1: catalog\n  stage: test\nB: x: y";
        let parsed = vars(text);
        assert_eq!(parsed["services"]["2"], "checkout");
        assert_eq!(parsed["envs"]["qa"]["1"], "catalog");
        assert_eq!(parsed["B"], "x: y");
        assert_eq!(format_lines(&parsed), text);
        assert_eq!(vars("a:\nb: 1")["a"], "");
        assert_eq!(vars("  a: 1\n  b: 2").len(), 2);
        let repeated = "#\na: 1\n#\nb:\n  # x\n  c: 2\n  # x\nd: 3";
        assert_eq!(format_lines(&vars(repeated)), repeated);
    }

    #[test]
    fn line_errors() {
        let err = |t| parse_lines(t).unwrap_err();
        assert_eq!(err("A: 1\nmissing"), "Line 2: expected NAME: value.");
        assert!(err("A: 1\nA: 2").contains("Line 2: duplicate"));
        assert!(err("a:\n  b: 1\n  b: 2").contains("Line 3: duplicate"));
        assert!(err("a:\n    b: 1\n  c: 2").contains("Line 3: inconsistent"));
        assert!(err("a: 1\n  b: 2").contains("Line 2: inconsistent"));
        assert!(err("a:\n  b: 1\n c: 2").contains("Line 3: inconsistent"));
        assert!(err("my var: 1").contains("invalid name"));
    }

    #[test]
    fn scans_references() {
        let text = r#"a {{.var.x}} {{:hosts.*.a}} {{:envs~}} {{.~}} {{p}} {{dev:y}} {{@f("a,)}}", :x)}} {{@g}}"#;
        let found = scan(text);
        let kinds: Vec<String> = found.iter().map(|f| f.reference.to_string()).collect();
        assert_eq!(
            kinds,
            [
                "{{.var.x}}",
                "{{:hosts.*.a}}",
                "{{:envs~}}",
                "{{.~}}",
                "{{p}}",
                r#"{{@f("a,)}}", :x)}}"#,
                "{{@g()}}",
            ]
        );
        assert_eq!(&text[found[0].start..found[0].end], "{{.var.x}}");
        assert_eq!(
            found[5].reference,
            Ref::Call {
                name: "f".into(),
                args_text: r#""a,)}}", :x"#.into()
            }
        );
        assert_eq!(
            found[3].reference,
            Ref::Absolute(Path {
                segments: vec![],
                keys: true
            })
        );
        assert!(scan("{{:}} {{set:name}} {{}} {{.a b}}").is_empty());
    }

    #[test]
    fn envs_table() {
        let root = envs();
        assert_eq!(list(&root, "{{:envs}}").unwrap_err(), "envs.qa is a hash");
        assert_eq!(
            run("{{:envs}}", &root).unwrap_err(),
            "{{:envs}}: expected a string, got a hash"
        );
        assert_eq!(
            list(&root, "{{:envs.*}}").unwrap(),
            ["catalog", "checkout", "search"]
        );
        assert_eq!(
            list(&root, "{{:envs.**}}").unwrap(),
            ["catalog", "checkout", "search", "test"]
        );
        assert_eq!(list(&root, "{{:envs~}}").unwrap(), ["qa", "prod", "stage"]);
        assert_eq!(list(&root, "{{:envs.stage}}").unwrap(), ["test"]);
        assert_eq!(run("{{:envs.qa.2}}", &root).unwrap(), "checkout");
        assert_eq!(
            run("{{:envs.*.1}}", &root).unwrap_err(),
            "{{:envs.*.1}}: expected a string, got a list (2)"
        );
    }

    #[test]
    fn priority_and_root() {
        let root = root(&[
            ("var", "region: eu\ntype: v"),
            ("dev", "type: d\nhost: h-{{:type}}"),
            ("env", "type: e\nHOME: /home"),
        ]);
        assert_eq!(
            run("{{:type}} {{.dev.type}} {{:HOME}}", &root).unwrap(),
            "v d /home"
        );
        assert_eq!(run("{{:host}}", &root).unwrap(), "h-v");
        assert_eq!(list(&root, "{{.*.type}}").unwrap(), ["v", "d", "e"]);
        assert_eq!(list(&root, "{{.~}}").unwrap(), ["var", "dev", "env"]);
        assert_eq!(list(&root, "{{.**.type}}").unwrap(), ["v", "d", "e"]);
        assert_eq!(
            run("{{:nope}} {{.dev.nope}}", &root).unwrap_err(),
            "Unknown variables {{:nope}}, {{.dev.nope}}"
        );
        assert_eq!(
            run("{{name}}", &root).unwrap_err(),
            "Unknown reference {{name}}"
        );
        assert_eq!(
            run("{{set:name}} text", &root).unwrap(),
            "{{set:name}} text"
        );
    }

    #[test]
    fn cycles_and_depth() {
        let r = root(&[("var", "a: {{:b}}\nb: x{{:a}}")]);
        assert_eq!(
            run("{{:a}}", &r).unwrap_err(),
            "Circular variable reference: {{:a}} -> {{:b}} -> {{:a}}"
        );
        let mut text = String::new();
        for i in 0..20 {
            text.push_str(&format!("v{i}: {{{{:v{}}}}}\n", i + 1));
        }
        text.push_str("v20: end");
        let r = root(&[("var", &text)]);
        assert!(
            run("{{:v0}}", &r)
                .unwrap_err()
                .contains("nested more than 16")
        );
        assert_eq!(run("{{:v10}}", &r).unwrap(), "end");
    }

    #[test]
    fn json_escaping_and_secrets() {
        let r = root(&[(
            "var",
            "q: a\"b\nauth:\n  token: t\"k\nname: {{.var.auth.token}}",
        )]);
        let out = resolve(r#"{"q":"{{:q}}","t":"{{:name}}"}"#, &r, true, &mut no_calls).unwrap();
        assert_eq!(out.text, r#"{"q":"a\"b","t":"t\"k"}"#);
        assert_eq!(out.secrets, ["t\"k"]);
        let out = resolve("{{:q}}", &r, false, &mut no_calls).unwrap();
        assert_eq!(out.text, "a\"b");
        assert!(out.secrets.is_empty());
    }

    #[test]
    fn calls_are_expanded_and_resolved() {
        let r = root(&[("var", "d: example.com")]);
        let mut seen = Vec::new();
        let out = resolve(
            r#"https://{{@host("a", :d)}}/"#,
            &r,
            false,
            &mut |name, args| {
                seen.push((name.to_owned(), args.to_owned()));
                Ok("a.{{:d}}".into())
            },
        )
        .unwrap();
        assert_eq!(out.text, "https://a.example.com/");
        assert_eq!(seen, [("host".to_owned(), r#""a", :d"#.to_owned())]);
        let err = resolve("{{@loop()}}", &r, false, &mut |_, _| {
            Ok("{{@loop()}}".into())
        });
        assert!(err.unwrap_err().starts_with("Circular variable reference"));
    }

    #[test]
    fn nested_references_build_the_path_first() {
        let r = root(&[
            ("tree", "alpha:\n  one:\n    1: leaf"),
            ("var", "first: alpha\nsecond: one"),
        ]);
        let text = "x {{.tree.{{.var.first}}.{{:second}}.1}} y";
        assert_eq!(scan(text).len(), 1);
        assert_eq!(run(text, &r).unwrap(), "x leaf y");
        let err = run("{{.tree.{{.var.first}}.none}}", &r).unwrap_err();
        assert_eq!(err, "Unknown variable {{.tree.alpha.none}}");
        let err = run("{{.tree.{{.var.first}} z}}", &r).unwrap_err();
        assert!(err.contains("is not a variable reference"), "{err}");
    }

    #[test]
    fn functions_get_json_arguments_and_insert_data() {
        let r = root(&[
            ("tree", "alpha:\n  one: x\n  # note\n  two: y"),
            ("var", "name: alpha\nauth:\n  token: t\"k\nraw: {{:name}}"),
        ]);
        assert_eq!(
            scan("{{$f}} {{$g(\"a)}}\", :x)}} \\{{$h()}}")
                .into_iter()
                .map(|found| found.reference.to_string())
                .collect::<Vec<_>>(),
            ["{{$f()}}", r#"{{$g("a)}}", :x)}}"#]
        );
        let mut seen = Vec::new();
        let mut function = |name: &str, args: &[Value]| {
            seen.push((name.to_owned(), args.to_vec()));
            Ok(format!("{{{{:name}}}} \"{}\"", args.len()))
        };
        let text = r#"{{$f("{{:name}}-b", :raw, .tree.alpha, :alpha~)}}"#;
        let out = resolve_with(text, &r, true, &mut no_calls, &mut function).unwrap();
        assert_eq!(out.text, r#"{{:name}} \"4\""#);
        assert!(out.secrets.is_empty());
        assert_eq!(
            seen[0].1,
            [
                Value::from("alpha-b"),
                Value::from("alpha"),
                serde_json::json!({"one": "x", "two": "y"}),
                serde_json::json!(["one", "two"]),
            ]
        );
        let out = resolve_with("{{$s()}}", &r, false, &mut no_calls, &mut |_, _| {
            Ok("key t\"k".into())
        });
        assert_eq!(out.unwrap().secrets, ["t\"k"]);
        let fail = |text: &str| {
            resolve_with(
                text,
                &r,
                false,
                &mut no_calls,
                &mut |_, _| Ok(String::new()),
            )
            .unwrap_err()
        };
        assert!(fail("{{$f(x)}}").contains("not a parameter in scope"));
        assert!(fail("{{$f(:nope)}}").contains("Unknown variable {{:nope}}"));
        assert_eq!(
            run("{{$f()}}", &r).unwrap_err(),
            "{{$f()}}: Unknown function $f"
        );
    }

    #[test]
    fn eager_function_calls_parse_round_trip_and_resolve_like_lazy_ones() {
        let r = root(&[("tree", "a: x")]);
        let found = scan(r#"<{{$!f("a")}}> {{$!g}} {{$!}} {{$!!h()}}"#);
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0].reference,
            Ref::Function {
                name: "f".into(),
                args_text: "\"a\"".into(),
                eager: true,
            }
        );
        assert_eq!((found[0].start, found[0].end), (1, 13));
        assert_eq!(found[0].reference.to_string(), r#"{{$!f("a")}}"#);
        assert_eq!(found[1].reference.to_string(), "{{$!g()}}");
        let out = resolve_with(
            "{{$!f(:a)}}",
            &r,
            false,
            &mut no_calls,
            &mut |name, args| Ok(format!("{name}={}", args[0])),
        );
        assert_eq!(out.unwrap().text, "f=\"x\"");
    }

    #[test]
    fn escaped_braces_stay_literal_once() {
        let r = root(&[("var", "d: x\nlit: \\{{:d}}")]);
        assert!(scan(r"a \{{:d}} {{:d}}").len() == 1);
        assert_eq!(
            run(r"\{{:d}}-{{:d}}-{{:lit}}", &r).unwrap(),
            "{{:d}}-x-{{:d}}"
        );
        let out = resolve("{{@t()}}", &r, false, &mut |_, _| Ok(r"[\{{kind}}]".into()));
        assert_eq!(out.unwrap().text, "[{{kind}}]");
    }

    #[test]
    fn leaves_and_children() {
        let r = root(&[
            ("var", "region: eu"),
            ("dev", "hosts:\n  catalog: c\n  2: x"),
        ]);
        let all = leaves(&r);
        assert_eq!(
            all[1],
            Leaf {
                panel: "dev".into(),
                path: "hosts.catalog".into(),
                value: "c".into()
            }
        );
        assert_eq!(all.len(), 3);
        let top = children_at(&r, "");
        assert_eq!(
            top.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(),
            ["var", "dev"]
        );
        assert_eq!(top[0].value, None);
        let hosts = children_at(&r, "dev.hosts");
        assert_eq!(hosts[0].value.as_deref(), Some("c"));
        assert!(children_at(&r, "dev.hosts.catalog").is_empty());
    }
}
