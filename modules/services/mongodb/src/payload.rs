//! Completion inside the JSON payload. The text before the cursor is read as the
//! structure it opens: which object or array the cursor is in, what the key above it
//! is, which collection the pipeline reads and which fields it has by then. That says
//! what comes next: a key, a value, an element or a skeleton of a whole stage.

use crate::editor::Catalog;
use crate::layout;
use crate::mql::{self, Snippets, Table};
use services_sdk::Item;
use services_sdk::lang::label;

/// The most collections a check samples.
pub const MAX_SAMPLED: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Lookup,
    GraphLookup,
    UnionWith,
    Merge,
    Bucket,
    BucketAuto,
    Unwind,
    ReplaceRoot,
    Sample,
    Facet,
    AddFields,
    Cond,
    Let,
    Map,
    FilterExpr,
}

/// What an object or an array of the payload is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The payload of an action: its fields.
    Root,
    /// The payload of `runCommand`: the command document.
    Command,
    Filter,
    /// An array of filters: `$and`.
    Filters,
    /// The operators on one field of a filter.
    FieldQuery,
    Update,
    /// The body of an update operator: field names and values.
    Assign,
    Sort,
    Project,
    /// Field names with a direction or type: an index key.
    Fields,
    Docs,
    Doc,
    Pipeline,
    /// A stage: one `$stage` key.
    Stage,
    Group,
    /// The accumulator of a `$group` field.
    Accumulator,
    /// An aggregation expression: operators, or a document of expressions.
    Expr,
    ExprList,
    /// Names with an expression for each: `let`.
    Vars,
    Shape(Shape),
    Index,
    Indexes,
    Plain,
}

/// The value of a key, as far as completion knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Val {
    None,
    Bool,
    Strings(&'static [&'static str]),
    Numbers(&'static [&'static str]),
    Collection,
    Field,
    /// A field of the collection `from` of the object.
    ForeignField,
    Path,
    Expr,
    Object(Role),
}

const LOOKUP: &[(&str, Val)] = &[
    ("from", Val::Collection),
    ("localField", Val::Field),
    ("foreignField", Val::ForeignField),
    ("as", Val::None),
    ("let", Val::Object(Role::Vars)),
    ("pipeline", Val::Object(Role::Pipeline)),
];
const GRAPH_LOOKUP: &[(&str, Val)] = &[
    ("from", Val::Collection),
    ("startWith", Val::Expr),
    ("connectFromField", Val::ForeignField),
    ("connectToField", Val::ForeignField),
    ("as", Val::None),
    ("maxDepth", Val::None),
    ("depthField", Val::None),
    ("restrictSearchWithMatch", Val::Object(Role::Filter)),
];
const UNION_WITH: &[(&str, Val)] = &[
    ("coll", Val::Collection),
    ("pipeline", Val::Object(Role::Pipeline)),
];
const MERGE: &[(&str, Val)] = &[
    ("into", Val::Collection),
    ("on", Val::Field),
    ("let", Val::Object(Role::Vars)),
    ("whenMatched", Val::Strings(mql::MERGE_MATCHED)),
    ("whenNotMatched", Val::Strings(mql::MERGE_NOT_MATCHED)),
];
const BUCKET: &[(&str, Val)] = &[
    ("groupBy", Val::Expr),
    ("boundaries", Val::None),
    ("default", Val::None),
    ("output", Val::Object(Role::Group)),
];
const BUCKET_AUTO: &[(&str, Val)] = &[
    ("groupBy", Val::Expr),
    ("buckets", Val::None),
    ("output", Val::Object(Role::Group)),
    ("granularity", Val::Strings(mql::GRANULARITY)),
];
const UNWIND: &[(&str, Val)] = &[
    ("path", Val::Path),
    ("includeArrayIndex", Val::None),
    ("preserveNullAndEmptyArrays", Val::Bool),
];
const REPLACE_ROOT: &[(&str, Val)] = &[("newRoot", Val::Expr)];
const SAMPLE: &[(&str, Val)] = &[("size", Val::None)];
const COND: &[(&str, Val)] = &[("if", Val::Expr), ("then", Val::Expr), ("else", Val::Expr)];
const LET: &[(&str, Val)] = &[("vars", Val::Object(Role::Vars)), ("in", Val::Expr)];
const MAP: &[(&str, Val)] = &[("input", Val::Expr), ("as", Val::None), ("in", Val::Expr)];
const FILTER_EXPR: &[(&str, Val)] = &[
    ("input", Val::Expr),
    ("as", Val::None),
    ("cond", Val::Expr),
    ("limit", Val::None),
];

impl Shape {
    fn keys(self) -> &'static [(&'static str, Val)] {
        match self {
            Self::Lookup => LOOKUP,
            Self::GraphLookup => GRAPH_LOOKUP,
            Self::UnionWith => UNION_WITH,
            Self::Merge => MERGE,
            Self::Bucket => BUCKET,
            Self::BucketAuto => BUCKET_AUTO,
            Self::Unwind => UNWIND,
            Self::ReplaceRoot => REPLACE_ROOT,
            Self::Sample => SAMPLE,
            Self::Cond => COND,
            Self::Let => LET,
            Self::Map => MAP,
            Self::FilterExpr => FILTER_EXPR,
            Self::Facet | Self::AddFields => &[],
        }
    }

    /// The value of keys the shape does not list.
    fn other(self) -> Val {
        match self {
            Self::Facet => Val::Object(Role::Pipeline),
            Self::AddFields => Val::Expr,
            _ => Val::None,
        }
    }
}

/// The fields the payload of `action` takes, and what they hold.
fn root_keys(action: &str) -> &'static [&'static str] {
    match action.to_ascii_lowercase().as_str() {
        "find" => &[
            "filter",
            "projection",
            "sort",
            "limit",
            "skip",
            "hint",
            "collation",
            "batchSize",
            "singleBatch",
            "maxTimeMS",
            "allowDiskUse",
            "comment",
            "min",
            "max",
            "returnKey",
            "showRecordId",
        ],
        "aggregate" => &[
            "pipeline",
            "allowDiskUse",
            "collation",
            "hint",
            "maxTimeMS",
            "comment",
            "let",
        ],
        "count" => &["filter", "limit", "skip", "hint", "collation", "maxTimeMS"],
        "countdocuments" => &["filter", "skip", "limit"],
        "distinct" => &["key", "filter", "collation", "maxTimeMS"],
        "insert" => &[
            "documents",
            "ordered",
            "bypassDocumentValidation",
            "writeConcern",
        ],
        "update" => &[
            "filter",
            "update",
            "upsert",
            "multi",
            "arrayFilters",
            "collation",
            "hint",
            "ordered",
            "writeConcern",
        ],
        "delete" => &[
            "filter",
            "limit",
            "collation",
            "hint",
            "ordered",
            "writeConcern",
        ],
        "findandmodify" => &[
            "filter",
            "update",
            "remove",
            "new",
            "upsert",
            "sort",
            "projection",
            "arrayFilters",
            "collation",
            "hint",
            "maxTimeMS",
        ],
        "createindexes" => &[
            "indexes",
            "key",
            "name",
            "unique",
            "sparse",
            "expireAfterSeconds",
        ],
        "dropindexes" => &["index"],
        "listcollections" => &["filter", "nameOnly", "authorizedCollections"],
        "listdatabases" => &["filter", "nameOnly", "authorizedDatabases"],
        _ => &[],
    }
}

/// The text after `"key": ` of a field of the payload root.
fn root_body(key: &str) -> &'static str {
    match key {
        "filter" | "sort" | "projection" | "min" | "max" | "hint" | "collation" | "let" => "{$0}",
        "update" => "{\"$set\": {$0}}",
        "pipeline" | "arrayFilters" => "[$0]",
        "documents" => "[{$0}]",
        "indexes" => "[{\"key\": {$0}, \"name\": \"\"}]",
        "key" | "index" | "name" => "\"$0\"",
        _ => "$0",
    }
}

const BOOLEAN_KEYS: &[&str] = &[
    "upsert",
    "multi",
    "new",
    "remove",
    "ordered",
    "singleBatch",
    "allowDiskUse",
    "returnKey",
    "showRecordId",
    "nameOnly",
    "authorizedCollections",
    "authorizedDatabases",
    "bypassDocumentValidation",
];

/// The role of the root of a payload that opens with `open`.
fn root(action: &str, open: char) -> Role {
    match (open, action.to_ascii_lowercase().as_str()) {
        ('{', "runcommand") => Role::Command,
        ('{', _) => Role::Root,
        (_, "aggregate") => Role::Pipeline,
        (_, "insert") => Role::Docs,
        (_, "createindexes") => Role::Indexes,
        _ => Role::Plain,
    }
}

/// The role of the object, or the array, that `key` holds in a `{}` of `parent`.
fn child(parent: Role, key: &str, array: bool) -> Role {
    let role = match (parent, key) {
        (Role::Root | Role::Command, "filter" | "query" | "q" | "partialFilterExpression") => {
            Role::Filter
        }
        (Role::Root | Role::Command, "sort") => Role::Sort,
        (Role::Root | Role::Command, "projection" | "fields") => Role::Project,
        (Role::Root | Role::Command, "hint" | "min" | "max" | "key") => Role::Fields,
        (Role::Root | Role::Command, "update" | "u") if array => Role::Pipeline,
        (Role::Root | Role::Command, "update" | "u") => Role::Update,
        (Role::Root | Role::Command, "pipeline") => Role::Pipeline,
        (Role::Root | Role::Command, "documents") => Role::Docs,
        (Role::Root | Role::Command, "indexes") => Role::Indexes,
        (Role::Root | Role::Command, "let") => Role::Vars,
        (Role::Index, "key") => Role::Fields,
        (Role::Index, "partialFilterExpression") => Role::Filter,
        (Role::Filter, "$and" | "$or" | "$nor") => Role::Filters,
        (Role::Filter, "$expr") => Role::Expr,
        (Role::Filter, key) if key.starts_with('$') => Role::Plain,
        (Role::Filter, _) => Role::FieldQuery,
        (Role::FieldQuery, "$elemMatch") => Role::Filter,
        (Role::FieldQuery, "$not") => Role::FieldQuery,
        (Role::Update, _) => Role::Assign,
        (Role::Stage, "$match") => Role::Filter,
        (Role::Stage, "$sort") => Role::Sort,
        (Role::Stage, "$project") => Role::Project,
        (Role::Stage, "$addFields" | "$set") => Role::Shape(Shape::AddFields),
        (Role::Stage, "$group") => Role::Group,
        (Role::Stage, "$lookup") => Role::Shape(Shape::Lookup),
        (Role::Stage, "$graphLookup") => Role::Shape(Shape::GraphLookup),
        (Role::Stage, "$unionWith") => Role::Shape(Shape::UnionWith),
        (Role::Stage, "$merge") => Role::Shape(Shape::Merge),
        (Role::Stage, "$bucket") => Role::Shape(Shape::Bucket),
        (Role::Stage, "$bucketAuto") => Role::Shape(Shape::BucketAuto),
        (Role::Stage, "$unwind") => Role::Shape(Shape::Unwind),
        (Role::Stage, "$replaceRoot") => Role::Shape(Shape::ReplaceRoot),
        (Role::Stage, "$replaceWith") => Role::Expr,
        (Role::Stage, "$sample") => Role::Shape(Shape::Sample),
        (Role::Stage, "$facet") => Role::Shape(Shape::Facet),
        (Role::Group, "_id") => Role::Expr,
        (Role::Group, _) => Role::Accumulator,
        (Role::Project, _) => Role::Expr,
        (Role::Accumulator | Role::Vars, _) => Role::Expr,
        (Role::Expr, "$cond") if !array => Role::Shape(Shape::Cond),
        (Role::Expr, "$let") => Role::Shape(Shape::Let),
        (Role::Expr, "$map") => Role::Shape(Shape::Map),
        (Role::Expr, "$filter") => Role::Shape(Shape::FilterExpr),
        (Role::Expr, key) if key.starts_with('$') && array => Role::ExprList,
        (Role::Expr, _) => Role::Expr,
        (Role::Shape(shape), key) => {
            let val = shape
                .keys()
                .iter()
                .find(|(name, _)| *name == key)
                .map_or(shape.other(), |(_, val)| *val);
            return match val {
                Val::Object(role) if array == matches!(role, Role::Pipeline | Role::Filters) => {
                    role
                }
                Val::Expr if array => Role::ExprList,
                Val::Expr => Role::Expr,
                _ => Role::Plain,
            };
        }
        _ => Role::Plain,
    };
    let list = matches!(
        role,
        Role::Pipeline | Role::Filters | Role::Docs | Role::Indexes | Role::ExprList
    );
    if array == list { role } else { Role::Plain }
}

/// The role of an element of the array `parent`.
fn element(parent: Role) -> Role {
    match parent {
        Role::Pipeline => Role::Stage,
        Role::Filters => Role::Filter,
        Role::Docs => Role::Doc,
        Role::Indexes => Role::Index,
        Role::ExprList => Role::Expr,
        _ => Role::Plain,
    }
}

/// What the value of `key` in an object of `role` is.
fn value_of(role: Role, key: &str, action: &str) -> Val {
    match (role, key) {
        (Role::Root, "key") if action.eq_ignore_ascii_case("distinct") => Val::Field,
        (Role::Root | Role::Command, key) if BOOLEAN_KEYS.contains(&key) => Val::Bool,
        (Role::Root | Role::Command, "verbosity") => Val::Strings(mql::VERBOSITY),
        (Role::Root | Role::Command, "pipeline") => Val::Object(Role::Pipeline),
        (Role::Root | Role::Command, "documents") => Val::Object(Role::Docs),
        (Role::Root | Role::Command, "indexes") => Val::Object(Role::Indexes),
        (Role::Root | Role::Command, "filter" | "query" | "q") => Val::Object(Role::Filter),
        (Role::Root | Role::Command, "sort") => Val::Object(Role::Sort),
        (Role::Root | Role::Command, "projection") => Val::Object(Role::Project),
        (Role::Root | Role::Command, "update" | "u") => Val::Object(Role::Update),
        (Role::Sort, _) => Val::Numbers(mql::DIRECTIONS),
        (Role::Project, _) => Val::Numbers(mql::PROJECTIONS),
        (Role::Fields, _) => Val::Numbers(mql::DIRECTIONS),
        (Role::FieldQuery, "$type") => Val::Strings(mql::TYPES),
        (Role::FieldQuery, "$exists") => Val::Bool,
        (Role::FieldQuery, "$options") => Val::Strings(mql::REGEX_OPTIONS),
        (Role::Index, "unique" | "sparse" | "hidden" | "background") => Val::Bool,
        (Role::Index, "key") => Val::Object(Role::Fields),
        (Role::Stage, "$match") => Val::Object(Role::Filter),
        (Role::Stage, "$sort") => Val::Object(Role::Sort),
        (Role::Stage, "$project") => Val::Object(Role::Project),
        (Role::Stage, "$group") => Val::Object(Role::Group),
        (Role::Stage, "$unwind" | "$sortByCount") => Val::Path,
        (Role::Stage, "$unionWith" | "$out") => Val::Collection,
        (Role::Stage, "$replaceWith") => Val::Expr,
        (Role::Group, "_id") => Val::Expr,
        (Role::Accumulator | Role::Vars | Role::Expr, _) => Val::Expr,
        (Role::Shape(shape), key) => shape
            .keys()
            .iter()
            .find(|(name, _)| *name == key)
            .map_or(shape.other(), |(_, val)| *val),
        _ => Val::None,
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Op {
    Add(Vec<String>),
    Reset(Vec<String>),
    Remove(Vec<String>),
    Lookup { name: String, from: Option<String> },
}

/// A closed object or array: what the stages after it and the variables read of it.
#[derive(Clone, Debug, Default)]
struct Closed {
    key: String,
    keys: Vec<String>,
    values: Vec<(String, String)>,
    kids: Vec<Closed>,
}

#[derive(Debug)]
struct Frame {
    open: char,
    role: Role,
    key: Option<String>,
    expect_key: bool,
    /// Nothing but the colon, or the opening bracket and commas, came since.
    start: bool,
    first: bool,
    keys: Vec<String>,
    values: Vec<(String, String)>,
    kids: Vec<Closed>,
    ops: Vec<Op>,
}

impl Frame {
    fn new(open: char, role: Role) -> Self {
        Self {
            open,
            role,
            key: None,
            expect_key: open == '{',
            start: open == '[',
            first: true,
            keys: Vec::new(),
            values: Vec::new(),
            kids: Vec::new(),
            ops: Vec::new(),
        }
    }

    fn value(&self, key: &str) -> Option<String> {
        self.values
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
            .filter(|value| !value.is_empty())
    }

    fn kid(&self, key: &str) -> Option<&Closed> {
        self.kids.iter().find(|kid| kid.key == key)
    }

    fn scalar(&mut self, text: &str) {
        if self.open == '{' {
            if self.expect_key && self.key.is_none() {
                self.key = Some(text.to_owned());
                self.keys.push(text.to_owned());
            } else if !self.expect_key
                && self.start
                && let Some(key) = &self.key
            {
                self.values.push((key.clone(), text.to_owned()));
            }
        }
        self.start = false;
    }

    fn close(&mut self, done: Frame) {
        let kid = Closed {
            key: self.key.clone().unwrap_or_default(),
            keys: done.keys.clone(),
            values: done.values.clone(),
            kids: done.kids.clone(),
        };
        if self.role == Role::Pipeline && done.role == Role::Stage {
            self.ops.extend(stage_ops(&done));
        }
        self.kids.push(kid);
        self.start = false;
    }
}

impl Closed {
    fn kid(&self, key: &str) -> Option<&Closed> {
        self.kids.iter().find(|kid| kid.key == key)
    }
}

/// How a closed stage changes the fields of the documents.
fn stage_ops(stage: &Frame) -> Vec<Op> {
    let Some(name) = stage.keys.first() else {
        return vec![];
    };
    let body = stage.kid(name);
    let keys = body.map(|body| body.keys.clone()).unwrap_or_default();
    let text = stage.value(name);
    match name.as_str() {
        "$addFields" | "$set" => vec![Op::Add(keys)],
        "$project" => {
            let kept = |key: &str| {
                body.and_then(|body| {
                    body.values
                        .iter()
                        .find(|(name, _)| name == key)
                        .map(|(_, value)| !matches!(value.as_str(), "0" | "false"))
                })
            };
            if keys
                .iter()
                .any(|key| key != "_id" && kept(key).unwrap_or(true))
            {
                let mut fields: Vec<String> = keys
                    .iter()
                    .filter(|key| kept(key).unwrap_or(true))
                    .cloned()
                    .collect();
                if !keys.iter().any(|key| key == "_id") {
                    fields.insert(0, "_id".into());
                }
                vec![Op::Reset(fields)]
            } else {
                vec![Op::Remove(keys)]
            }
        }
        "$unset" => vec![Op::Remove(text.into_iter().collect())],
        "$group" => {
            let mut fields = vec!["_id".to_owned()];
            if let Some(id) = body.and_then(|body| body.kids.iter().find(|kid| kid.key == "_id")) {
                fields.extend(
                    id.keys
                        .iter()
                        .filter(|key| !key.starts_with('$'))
                        .map(|key| format!("_id.{key}")),
                );
            }
            fields.extend(keys.into_iter().filter(|key| key != "_id"));
            vec![Op::Reset(fields)]
        }
        "$count" => vec![Op::Reset(text.into_iter().collect())],
        "$sortByCount" => vec![Op::Reset(vec!["_id".into(), "count".into()])],
        "$facet" => vec![Op::Reset(keys)],
        "$bucket" | "$bucketAuto" => {
            let mut fields = vec!["_id".to_owned(), "count".to_owned()];
            if let Some(output) = body.and_then(|body| body.kid("output")) {
                fields.extend(output.keys.iter().cloned());
            }
            vec![Op::Reset(fields)]
        }
        "$replaceRoot" | "$replaceWith" => vec![Op::Reset(vec![])],
        "$lookup" => body
            .and_then(|body| {
                let as_ = body.values.iter().find(|(key, _)| key == "as")?.1.clone();
                let from = body
                    .values
                    .iter()
                    .find(|(key, _)| key == "from")
                    .map(|(_, from)| from.clone());
                Some(vec![Op::Lookup { name: as_, from }])
            })
            .unwrap_or_default(),
        "$graphLookup" => body
            .and_then(|body| body.values.iter().find(|(key, _)| key == "as"))
            .map(|(_, name)| vec![Op::Add(vec![name.clone()])])
            .unwrap_or_default(),
        _ => vec![],
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Lex<'a> {
    Word(&'a str),
    Text(&'a str),
    Punct(char),
    Other,
}

/// The tokens of `source`; the start of a string left open at the end.
fn lex(source: &str) -> (Vec<Lex<'_>>, Option<usize>) {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = source[i..].find('\n').map_or(bytes.len(), |n| i + n);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = source[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |n| i + n + 4);
            }
            b'"' | b'\'' => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b {
                    j += if bytes[j] == b'\\' { 2 } else { 1 };
                }
                if j >= bytes.len() {
                    return (out, Some(i));
                }
                out.push(Lex::Text(&source[i + 1..j]));
                i = j + 1;
            }
            b'{' | b'}' | b'[' | b']' | b',' | b':' => {
                out.push(Lex::Punct(b as char));
                i += 1;
            }
            _ if word(b as char) => {
                let end = source[i..]
                    .find(|c: char| !word(c))
                    .map_or(bytes.len(), |n| i + n);
                out.push(Lex::Word(&source[i..end]));
                i = end;
            }
            _ => {
                out.push(Lex::Other);
                i += source[i..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    (out, None)
}

fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// `text` with every `{{…}}` placeholder replaced by digits of the same length.
fn mask(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let end = rest[start..]
            .find("}}")
            .map_or(rest.len(), |n| start + n + 2);
        out.push_str(&"0".repeat(end - start));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Whether `name` is a wrapper of Extended JSON: `$oid`, `$date`, `$numberLong`, …
pub fn wrapper(name: &str) -> bool {
    matches!(
        name,
        "$oid"
            | "$date"
            | "$numberInt"
            | "$numberLong"
            | "$numberDouble"
            | "$numberDecimal"
            | "$regularExpression"
            | "$timestamp"
            | "$binary"
            | "$uuid"
            | "$minKey"
            | "$maxKey"
            | "$symbol"
            | "$code"
            | "$undefined"
            | "$dbPointer"
    )
}

/// Whether `name` is a fixed field of a command or of a stage, not a field of the data.
pub fn field(name: &str) -> bool {
    const FIELDS: &[&str] = &[
        "filter",
        "q",
        "u",
        "query",
        "projection",
        "sort",
        "limit",
        "skip",
        "hint",
        "collation",
        "batchSize",
        "singleBatch",
        "maxTimeMS",
        "allowDiskUse",
        "comment",
        "pipeline",
        "let",
        "key",
        "documents",
        "ordered",
        "update",
        "upsert",
        "multi",
        "arrayFilters",
        "remove",
        "new",
        "indexes",
        "index",
        "unique",
        "sparse",
        "expireAfterSeconds",
        "from",
        "localField",
        "foreignField",
        "as",
        "startWith",
        "connectFromField",
        "connectToField",
        "maxDepth",
        "depthField",
        "restrictSearchWithMatch",
        "coll",
        "into",
        "on",
        "whenMatched",
        "whenNotMatched",
        "groupBy",
        "boundaries",
        "default",
        "output",
        "buckets",
        "path",
        "includeArrayIndex",
        "preserveNullAndEmptyArrays",
        "newRoot",
        "size",
        "if",
        "then",
        "else",
        "vars",
        "in",
        "input",
        "cond",
        "branches",
        "case",
        "nameOnly",
        "writeConcern",
        "bypassDocumentValidation",
    ];
    FIELDS.contains(&name)
}

/// Where the cursor is in the payload.
#[derive(Debug, PartialEq)]
pub enum Want {
    Nothing,
    /// Before the payload.
    Start,
    /// A key of an object.
    Keys {
        role: Role,
        quoted: bool,
        /// The first key of the payload object.
        first: bool,
    },
    /// The value of `key`.
    Value {
        role: Role,
        key: String,
        quoted: bool,
    },
    /// An element of an array of `role`.
    Element {
        role: Role,
        quoted: bool,
    },
}

pub struct Analysis {
    /// What is typed of the word before the cursor.
    pub fragment: String,
    pub want: Want,
    /// The characters after the cursor that belong to the word.
    pub tail: usize,
    /// A closing quote follows the cursor.
    pub closed: bool,
    /// A colon follows the cursor and its key.
    pub colon: bool,
    /// The indentation of the line the cursor is on.
    indent: String,
    frames: Vec<Frame>,
    scopes: Vec<Option<String>>,
    action: String,
}

/// The analysis of a payload at the cursor: `before` is the text up to it and `after` the
/// text from it on.
pub fn analyze(before: &str, after: &str, action: &str, collection: &str) -> Analysis {
    let before = mask(before);
    let after = mask(after);
    let line = before.rsplit('\n').next().unwrap_or("");
    let indent = line[..line.len() - line.trim_start_matches([' ', '\t']).len()].to_owned();
    let (_, open) = lex(&before);
    let quote = open.map(|start| before.as_bytes()[start]);
    let (fragment_start, quoted) = match open {
        Some(start) => (start + 1, true),
        None => {
            let start = before.rfind(|c: char| !word(c)).map_or(0, |n| {
                n + before[n..].chars().next().map_or(1, char::len_utf8)
            });
            (start, false)
        }
    };
    let fragment = before[fragment_start..].to_owned();
    let head = &before[..if quoted {
        fragment_start - 1
    } else {
        fragment_start
    }];
    let (tokens, _) = lex(head);
    let mut frames = walk(&tokens, action);
    let want = classify(&frames, tokens.is_empty(), quoted);

    let (tail, closed, rest) = match quote {
        Some(quote) => {
            let end = after.find(quote as char);
            let inner = &after[..end.unwrap_or(0)];
            let tail = if end.is_some() {
                inner.len()
            } else {
                after
                    .find(|c: char| !(word(c) || c == '.'))
                    .unwrap_or(after.len())
            };
            (tail, end.is_some(), end.map_or("", |n| &after[n + 1..]))
        }
        None => {
            let tail = after.find(|c: char| !word(c)).unwrap_or(after.len());
            (tail, false, &after[tail..])
        }
    };
    let trail = rest.trim_start();
    let colon = trail.starts_with(':');
    siblings(&mut frames, &tokens_of(rest), &want);
    let scopes = scopes(&frames, collection);
    Analysis {
        fragment,
        want,
        tail,
        closed,
        colon,
        indent,
        frames,
        scopes,
        action: action.to_owned(),
    }
}

fn tokens_of(text: &str) -> Vec<Lex<'_>> {
    lex(text).0
}

fn walk(tokens: &[Lex], action: &str) -> Vec<Frame> {
    let mut stack: Vec<Frame> = Vec::new();
    for token in tokens {
        match token {
            Lex::Punct(open @ ('{' | '[')) => {
                let role = match stack.last_mut() {
                    Some(parent) => {
                        parent.start = false;
                        if parent.open == '{' {
                            child(
                                parent.role,
                                parent.key.as_deref().unwrap_or(""),
                                *open == '[',
                            )
                        } else if *open == '{' {
                            element(parent.role)
                        } else if element(parent.role) == Role::Expr {
                            Role::ExprList
                        } else {
                            Role::Plain
                        }
                    }
                    None => root(action, *open),
                };
                stack.push(Frame::new(*open, role));
            }
            Lex::Punct('}' | ']') => {
                if let Some(done) = stack.pop()
                    && let Some(parent) = stack.last_mut()
                {
                    parent.close(done);
                }
            }
            Lex::Punct(',') => {
                if let Some(frame) = stack.last_mut() {
                    frame.expect_key = frame.open == '{';
                    frame.key = None;
                    frame.first = false;
                    frame.start = frame.open == '[';
                }
            }
            Lex::Punct(':') => {
                if let Some(frame) = stack.last_mut() {
                    frame.expect_key = false;
                    frame.start = true;
                }
            }
            Lex::Word(text) | Lex::Text(text) => {
                if let Some(frame) = stack.last_mut() {
                    frame.scalar(text);
                }
            }
            _ => {
                if let Some(frame) = stack.last_mut() {
                    frame.start = false;
                }
            }
        }
    }
    stack
}

fn classify(frames: &[Frame], empty: bool, quoted: bool) -> Want {
    match frames.last() {
        None if empty => Want::Start,
        None => Want::Nothing,
        Some(top) if top.open == '{' => {
            if top.expect_key && top.key.is_none() {
                Want::Keys {
                    role: top.role,
                    quoted,
                    first: top.first && frames.len() == 1,
                }
            } else if !top.expect_key && top.start {
                Want::Value {
                    role: top.role,
                    key: top.key.clone().unwrap_or_default(),
                    quoted,
                }
            } else {
                Want::Nothing
            }
        }
        Some(top) if top.start => Want::Element {
            role: top.role,
            quoted,
        },
        Some(_) => Want::Nothing,
    }
}

/// Strings that follow the cursor in the objects it is in: `from` may come after `pipeline`.
fn siblings(frames: &mut [Frame], rest: &[Lex], want: &Want) {
    let mut depth = frames.len();
    let mut pending: Vec<(Option<String>, bool)> = frames
        .iter()
        .map(|frame| (frame.key.clone(), !frame.expect_key))
        .collect();
    if matches!(want, Want::Value { .. } | Want::Element { .. })
        && let Some(last) = pending.last_mut()
    {
        *last = (None, false);
    }
    for token in rest {
        match token {
            Lex::Punct('{' | '[') => {
                if depth >= 1 && depth <= frames.len() {
                    pending[depth - 1] = (None, false);
                }
                depth += 1;
            }
            Lex::Punct('}' | ']') => depth = depth.saturating_sub(1),
            Lex::Punct(',') if depth >= 1 && depth <= frames.len() => {
                pending[depth - 1] = (None, false);
            }
            Lex::Punct(':') if depth >= 1 && depth <= frames.len() => {
                if pending[depth - 1].0.is_some() {
                    pending[depth - 1].1 = true;
                }
            }
            Lex::Word(text) | Lex::Text(text) if depth >= 1 && depth <= frames.len() => {
                let at = depth - 1;
                if frames[at].open != '{' {
                    continue;
                }
                match &pending[at] {
                    (Some(key), true) => {
                        if !frames[at].values.iter().any(|(name, _)| name == key) {
                            let value = ((*key).clone(), (*text).to_owned());
                            frames[at].values.push(value);
                        }
                        pending[at] = (None, false);
                    }
                    (None, _) => pending[at] = (Some((*text).to_owned()), false),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// The collection each frame reads.
fn scopes(frames: &[Frame], collection: &str) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = Vec::new();
    for (index, _) in frames.iter().enumerate() {
        let scope = match index.checked_sub(1).map(|parent| &frames[parent]) {
            None => (!collection.is_empty()).then(|| collection.to_owned()),
            Some(parent) => match (parent.role, parent.key.as_deref()) {
                (Role::Shape(Shape::Lookup), Some("pipeline")) => parent.value("from"),
                (Role::Shape(Shape::UnionWith), Some("pipeline")) => parent.value("coll"),
                (Role::Shape(Shape::GraphLookup), Some("restrictSearchWithMatch")) => {
                    parent.value("from")
                }
                _ => out[index - 1].clone(),
            },
        };
        out.push(scope);
    }
    out
}

impl Analysis {
    /// How many characters after the cursor the picked item replaces: the word, and the
    /// closing quote of a key that gets its own.
    pub fn replaced(&self) -> usize {
        let key = matches!(self.want, Want::Keys { .. });
        self.tail + usize::from(key && self.closed && !self.colon)
    }

    fn top(&self) -> Option<&Frame> {
        self.frames.last()
    }

    /// The fields documents have where the cursor is.
    fn fields(&self, known: &Catalog) -> Vec<String> {
        let of = |name: &Option<String>| -> Vec<String> {
            name.as_ref()
                .and_then(|name| known.fields.get(name))
                .cloned()
                .unwrap_or_default()
        };
        let Some(at) = self
            .frames
            .iter()
            .rposition(|frame| frame.role == Role::Pipeline)
        else {
            return of(&self.scopes.last().cloned().flatten());
        };
        let mut fields = of(&self.scopes[at]);
        for op in &self.frames[at].ops {
            match op {
                Op::Add(names) => {
                    for name in names {
                        if !fields.contains(name) {
                            fields.push(name.clone());
                        }
                    }
                }
                Op::Reset(names) => fields = names.clone(),
                Op::Remove(names) => fields.retain(|field| !names.contains(field)),
                Op::Lookup { name, from } => {
                    let from = known.fields.get(from.as_deref().unwrap_or(""));
                    fields.retain(|field| field != name);
                    fields.push(name.clone());
                    fields.extend(
                        from.into_iter()
                            .flatten()
                            .map(|field| format!("{name}.{field}")),
                    );
                }
            }
        }
        fields
    }

    /// The fields of the collection `from` of the object the cursor is in.
    fn foreign(&self, known: &Catalog) -> Vec<String> {
        self.top()
            .and_then(|top| top.value("from"))
            .and_then(|from| known.fields.get(&from).cloned())
            .unwrap_or_default()
    }

    /// The field a value or an array element at the cursor is compared with.
    fn compared(&self) -> Option<String> {
        const COMPARISONS: &[&str] = &["$eq", "$ne", "$gt", "$gte", "$lt", "$lte"];
        const LISTS: &[&str] = &["$in", "$nin", "$all"];
        let frames = &self.frames;
        let key_at = |back: usize| {
            frames
                .len()
                .checked_sub(back)
                .and_then(|at| frames[at].key.clone())
        };
        match &self.want {
            Want::Value { role, key, .. } => match role {
                Role::Filter if !key.starts_with('$') => Some(key.clone()),
                Role::FieldQuery if COMPARISONS.contains(&key.as_str()) => key_at(2),
                _ => None,
            },
            Want::Element { .. } if self.top()?.open == '[' => {
                let list = key_at(2)?;
                (LISTS.contains(&list.as_str())
                    && frames.get(frames.len().checked_sub(2)?)?.role == Role::FieldQuery)
                    .then(|| key_at(3))
                    .flatten()
            }
            _ => None,
        }
    }

    /// The values the sample shows for the field the cursor compares with.
    fn sampled(&self, known: &Catalog, quoted: bool) -> Vec<Item> {
        let Some(field) = self.compared() else {
            return vec![];
        };
        let pipeline = self
            .frames
            .iter()
            .rposition(|frame| frame.role == Role::Pipeline);
        if pipeline.is_some_and(|at| {
            self.frames[at]
                .ops
                .iter()
                .any(|op| matches!(op, Op::Reset(_)))
        }) {
            return vec![];
        }
        let scope = match pipeline {
            Some(at) => self.scopes[at].clone(),
            None => self.scopes.last().cloned().flatten(),
        };
        let Some(values) = scope
            .and_then(|name| known.values.get(&name))
            .and_then(|fields| fields.get(&field))
        else {
            return vec![];
        };
        values
            .iter()
            .filter_map(|text| {
                if !quoted {
                    return Some(make(text, text.clone(), "Sampled value", "value", false));
                }
                match serde_json::from_str::<serde_json::Value>(text) {
                    Ok(serde_json::Value::String(raw)) => Some(make(
                        &raw,
                        quote(&raw, true, self.closed),
                        "Sampled value",
                        "value",
                        false,
                    )),
                    _ => None,
                }
            })
            .collect()
    }

    /// The `$$names` the cursor can read.
    fn variables(&self) -> Vec<String> {
        let mut names = Vec::new();
        for frame in &self.frames {
            let kid = match frame.role {
                Role::Shape(Shape::Lookup | Shape::Merge) | Role::Root | Role::Command => "let",
                Role::Shape(Shape::Let) => "vars",
                _ => "",
            };
            if let Some(kid) = frame.kid(kid) {
                names.extend(kid.keys.iter().cloned());
            }
            if matches!(frame.role, Role::Shape(Shape::Map | Shape::FilterExpr)) {
                names.push(frame.value("as").unwrap_or_else(|| "this".into()));
            }
        }
        names
    }

    /// Whether the cursor is in an aggregation, where strings can name fields.
    fn aggregation(&self) -> bool {
        self.frames.iter().any(|frame| {
            matches!(
                frame.role,
                Role::Pipeline
                    | Role::Stage
                    | Role::Group
                    | Role::Accumulator
                    | Role::Expr
                    | Role::ExprList
                    | Role::Vars
                    | Role::Project
                    | Role::Shape(_)
            )
        })
    }
}

fn make(label: &str, insert: String, detail: &str, kind: &str, next: bool) -> Item {
    Item {
        label: label.to_owned(),
        insert,
        detail: detail.to_owned(),
        kind: kind.to_owned(),
        next,
    }
}

/// What to put around a key: the quoting the text already has, and a body.
struct Keys {
    quoted: bool,
    closed: bool,
    colon: bool,
}

impl Keys {
    fn insert(&self, name: &str, body: &str) -> String {
        match (self.quoted, self.closed, self.colon) {
            (true, true, true) => name.to_owned(),
            (true, true, false) => format!("{name}\": {body}"),
            (true, false, true) => format!("{name}\""),
            (true, false, false) => format!("{name}\": {body}"),
            (false, _, true) => format!("\"{name}\""),
            (false, _, false) => format!("\"{name}\": {body}"),
        }
    }
}

/// Whether a value of this body is picked from a list the App should ask for.
fn picks(val: Val) -> bool {
    matches!(
        val,
        Val::Bool
            | Val::Strings(_)
            | Val::Numbers(_)
            | Val::Collection
            | Val::Field
            | Val::ForeignField
            | Val::Path
            | Val::Expr
    )
}

fn body_of(val: Val) -> &'static str {
    match val {
        Val::Strings(_) | Val::Collection | Val::Field | Val::ForeignField | Val::Path => "\"$0\"",
        Val::Expr => "\"$0\"",
        Val::Object(Role::Pipeline | Role::Filters) => "[$0]",
        Val::Object(Role::Docs) => "[{$0}]",
        Val::Object(_) => "{$0}",
        _ => "$0",
    }
}

fn names(
    list: &Table,
    keys: &Keys,
    detail: &str,
    kind: &str,
    body: impl Fn(&str) -> &'static str,
) -> Vec<Item> {
    list.iter()
        .map(|(name, text)| {
            let body = body(name);
            make(
                name,
                keys.insert(name, body),
                if detail.is_empty() { text } else { detail },
                kind,
                false,
            )
        })
        .collect()
}

fn snippets(list: &Snippets, keys: &Keys, kind: &str) -> Vec<Item> {
    list.iter()
        .map(|(name, detail, body)| make(name, keys.insert(name, body), detail, kind, false))
        .collect()
}

fn fields_items(fields: &[String], keys: &Keys, body: &str, next: bool, detail: &str) -> Vec<Item> {
    fields
        .iter()
        .map(|name| make(name, keys.insert(name, body), detail, "property", next))
        .collect()
}

/// A key of an object of `role`.
fn key_items(
    analysis: &Analysis,
    known: &Catalog,
    role: Role,
    quoted: bool,
    first: bool,
) -> Vec<Item> {
    let keys = Keys {
        quoted,
        closed: analysis.closed,
        colon: analysis.colon,
    };
    let fields = analysis.fields(known);
    let action = analysis.action.as_str();
    match role {
        Role::Root => root_keys(action)
            .iter()
            .map(|name| {
                let val = value_of(Role::Root, name, action);
                make(
                    name,
                    keys.insert(name, root_body(name)),
                    "Field",
                    "property",
                    picks(val),
                )
            })
            .collect(),
        Role::Command if first => mql::COMMANDS
            .iter()
            .map(|(name, detail)| {
                let body = if mql::COLLECTION_COMMANDS.contains(name) {
                    "\"$0\""
                } else {
                    "1$0"
                };
                make(
                    name,
                    keys.insert(name, body),
                    detail,
                    "keyword",
                    body != "1$0",
                )
            })
            .collect(),
        Role::Filter => {
            let mut items = fields_items(&fields, &keys, "$0", false, "Field");
            items.extend(snippets(mql::LOGICAL, &keys, "keyword"));
            items
        }
        Role::FieldQuery => {
            let mut items = snippets(mql::QUERY_OPERATORS, &keys, "keyword");
            for item in &mut items {
                item.next = matches!(item.label.as_str(), "$type" | "$exists");
            }
            items
        }
        Role::Update => names(mql::UPDATE_OPERATORS, &keys, "", "keyword", |_| "{$0}"),
        Role::Assign | Role::Doc => fields_items(&fields, &keys, "$0", false, "Field"),
        Role::Sort | Role::Project | Role::Fields => {
            fields_items(&fields, &keys, "$0", true, "Field")
        }
        Role::Stage => stage_items(&keys, |body| body.to_owned()),
        Role::Group => vec![make(
            "_id",
            keys.insert("_id", "\"$0\""),
            "Group key",
            "property",
            true,
        )],
        Role::Accumulator => names(
            mql::ACCUMULATORS,
            &keys,
            "Accumulator",
            "keyword",
            |_| "\"$0\"",
        ),
        Role::Expr => snippets(mql::EXPRESSIONS, &keys, "keyword"),
        Role::Shape(shape) => shape
            .keys()
            .iter()
            .map(|(name, val)| {
                make(
                    name,
                    keys.insert(name, body_of(*val)),
                    "Option",
                    "property",
                    picks(*val) && !matches!(val, Val::Expr),
                )
            })
            .collect(),
        Role::Index => mql::INDEX_OPTIONS
            .iter()
            .map(|(name, detail, body)| {
                let val = value_of(Role::Index, name, action);
                make(
                    name,
                    keys.insert(name, body),
                    detail,
                    "property",
                    picks(val),
                )
            })
            .collect(),
        Role::Command
        | Role::Filters
        | Role::Docs
        | Role::Pipeline
        | Role::ExprList
        | Role::Vars
        | Role::Indexes
        | Role::Plain => vec![],
    }
}

/// The stages with their skeletons; `wrap` shapes the text after the key.
fn stage_items(keys: &Keys, wrap: impl Fn(&str) -> String) -> Vec<Item> {
    let mut items = Vec::new();
    for (name, detail, body) in mql::STAGES {
        items.push(make(
            name,
            keys.insert(name, &wrap(body)),
            detail,
            "keyword",
            false,
        ));
        for (stage, label, variant) in mql::STAGE_VARIANTS {
            if stage == name {
                items.push(make(
                    label,
                    keys.insert(name, &wrap(variant)),
                    detail,
                    "keyword",
                    false,
                ));
            }
        }
    }
    items
}

fn strings(list: &[&str], quoted: bool, closed: bool, detail: &str) -> Vec<Item> {
    list.iter()
        .map(|text| make(text, quote(text, quoted, closed), detail, "value", false))
        .collect()
}

fn quote(text: &str, quoted: bool, closed: bool) -> String {
    match (quoted, closed) {
        (true, true) => text.to_owned(),
        (true, false) => format!("{text}\""),
        (false, _) => format!("\"{text}\""),
    }
}

fn paths(analysis: &Analysis, known: &Catalog, quoted: bool) -> Vec<Item> {
    let closed = analysis.closed;
    let mut items: Vec<Item> = analysis
        .fields(known)
        .iter()
        .map(|name| {
            let path = format!("${name}");
            make(
                &path,
                quote(&path, quoted, closed),
                "Field",
                "property",
                false,
            )
        })
        .collect();
    for name in analysis.variables() {
        let path = format!("$${name}");
        items.push(make(
            &path,
            quote(&path, quoted, closed),
            "Variable",
            "variable",
            false,
        ));
    }
    for (name, detail) in mql::SYSTEM_VARIABLES {
        items.push(make(
            name,
            quote(name, quoted, closed),
            detail,
            "variable",
            false,
        ));
    }
    items
}

fn literals(list: &[&str], detail: &str) -> Vec<Item> {
    list.iter()
        .map(|text| make(text, (*text).to_owned(), detail, "value", false))
        .collect()
}

/// A value of `val`: its choices.
fn value_items(analysis: &Analysis, known: &Catalog, val: Val, quoted: bool) -> Vec<Item> {
    let closed = analysis.closed;
    let fields = |list: Vec<String>, detail: &str| -> Vec<Item> {
        list.iter()
            .map(|name| make(name, quote(name, quoted, closed), detail, "property", false))
            .collect()
    };
    match val {
        Val::Bool if !quoted => literals(mql::BOOLEANS, "Boolean"),
        Val::Numbers(list) if !quoted => literals(list, "Value"),
        Val::Strings(list) => strings(list, quoted, closed, "Value"),
        Val::Collection => known
            .collections
            .iter()
            .map(|name| {
                make(
                    name,
                    quote(name, quoted, closed),
                    "Collection",
                    "class",
                    false,
                )
            })
            .collect(),
        Val::Field => fields(analysis.fields(known), "Field"),
        Val::ForeignField => fields(analysis.foreign(known), "Field"),
        Val::Path | Val::Expr => paths(analysis, known, quoted),
        Val::Object(role) if !quoted => vec![make(
            if matches!(role, Role::Pipeline | Role::Filters | Role::Docs) {
                "[]"
            } else {
                "{}"
            },
            body_of(val).to_owned(),
            "Skeleton",
            "snippet",
            false,
        )],
        _ => vec![],
    }
}

/// The starter of a payload: what an empty payload of `action` holds.
fn starters(action: &str) -> Vec<(&'static str, &'static str)> {
    let starter: &[(&str, &str)] = match action.to_ascii_lowercase().as_str() {
        "find" => &[("Filter", "{\"filter\": {$0}}")],
        "aggregate" => &[("Pipeline", "{\"pipeline\": [$0]}")],
        "count" | "countdocuments" => &[("Filter", "{\"filter\": {$0}}")],
        "distinct" => &[("Key", "{\"key\": \"$0\"}")],
        "insert" => &[("Documents", "{\"documents\": [{$0}]}")],
        "update" => &[("Update", "{\"filter\": {$0}, \"update\": {\"$set\": {}}}")],
        "delete" => &[("Delete", "{\"filter\": {$0}}")],
        "findandmodify" => &[(
            "Find and modify",
            "{\"filter\": {$0}, \"update\": {\"$set\": {}}, \"new\": true}",
        )],
        "createindexes" => &[("Index", "{\"indexes\": [{\"key\": {$0}, \"name\": \"\"}]}")],
        "runcommand" => &[("Command", "{\"$0\": 1}")],
        _ => &[],
    };
    starter.to_vec()
}

/// The items for the cursor, objects and arrays laid out for the line the cursor is on.
pub fn items(analysis: &Analysis, known: &Catalog) -> Vec<Item> {
    let mut items = candidates(analysis, known);
    for item in &mut items {
        item.insert = layout::lay_out(&item.insert, &analysis.indent);
    }
    items
}

fn candidates(analysis: &Analysis, known: &Catalog) -> Vec<Item> {
    let action = analysis.action.as_str();
    match &analysis.want {
        Want::Nothing => vec![],
        Want::Start => starters(action)
            .into_iter()
            .map(|(detail, body)| make(&label(body), body.to_owned(), detail, "snippet", false))
            .collect(),
        Want::Keys {
            role,
            quoted,
            first,
        } => key_items(analysis, known, *role, *quoted, *first),
        Want::Value { role, key, quoted } => {
            let val = value_of(*role, key, action);
            let mut items = analysis.sampled(known, *quoted);
            if !*quoted {
                items.extend(shaped(*role, key));
            }
            items.extend(value_items(analysis, known, val, *quoted));
            if items.is_empty()
                && *quoted
                && analysis.fragment.starts_with('$')
                && analysis.aggregation()
            {
                items = paths(analysis, known, true);
            }
            if !*quoted {
                items.extend(generic(shaped_first(*role, key), true));
            }
            unique(items)
        }
        Want::Element { role, quoted } => {
            let mut items = analysis.sampled(known, *quoted);
            items.extend(match role {
                Role::Pipeline if !quoted => {
                    let keys = Keys {
                        quoted: false,
                        closed: false,
                        colon: false,
                    };
                    stage_items(&keys, |body| body.to_owned())
                        .into_iter()
                        .map(|mut item| {
                            item.insert = format!("{{{}}}", item.insert);
                            item
                        })
                        .collect()
                }
                Role::Filters | Role::Docs if !quoted => {
                    vec![make("{}", "{$0}".into(), "Skeleton", "snippet", false)]
                }
                Role::Indexes if !quoted => vec![make(
                    "{}",
                    "{\"key\": {$0}, \"name\": \"\"}".into(),
                    "Skeleton",
                    "snippet",
                    false,
                )],
                Role::ExprList => paths(analysis, known, *quoted),
                _ => vec![],
            });
            if items.is_empty()
                && *quoted
                && analysis.fragment.starts_with('$')
                && analysis.aggregation()
            {
                items = paths(analysis, known, true);
            }
            if !*quoted {
                items.extend(generic(Body::Object, false));
            }
            unique(items)
        }
    }
}

/// What the text after a key starts with.
#[derive(Clone, Copy, PartialEq)]
enum Body {
    Object,
    Array,
    Text,
    Any,
}

fn body_kind(body: &str) -> Body {
    match body.chars().next() {
        Some('{') => Body::Object,
        Some('[') => Body::Array,
        Some('"') => Body::Text,
        _ => Body::Any,
    }
}

/// The shapes `key` takes in an object of `role`: label, description and body.
fn shapes(role: Role, key: &str) -> Vec<(String, &'static str, &'static str)> {
    let named = |list: &'static Snippets| {
        list.iter()
            .filter(|(name, _, _)| *name == key)
            .map(|(_, detail, body)| (String::new(), *detail, *body))
            .collect::<Vec<_>>()
    };
    match role {
        Role::Stage => {
            let mut list = named(mql::STAGES);
            for (stage, label, body) in mql::STAGE_VARIANTS {
                if *stage == key {
                    list.push(((*label).to_owned(), "Variant", *body));
                }
            }
            list
        }
        Role::Update => vec![(String::new(), "Update operator", "{$0}")],
        Role::Group if key != "_id" => vec![(String::new(), "Accumulator", "{$0}")],
        Role::Filter | Role::FieldQuery => {
            let mut list = named(mql::QUERY_OPERATORS);
            list.extend(named(mql::LOGICAL));
            list.into_iter()
                .map(|(label, detail, body)| {
                    let body = if body.starts_with("[{") { "[$0]" } else { body };
                    (label, detail, body)
                })
                .collect()
        }
        _ if key.starts_with('$') => named(mql::EXPRESSIONS),
        _ => vec![],
    }
}

fn shaped(role: Role, key: &str) -> Vec<Item> {
    shapes(role, key)
        .into_iter()
        .filter(|(_, _, body)| body_kind(body) != Body::Any)
        .map(|(name, detail, body)| {
            let name = if name.is_empty() { label(body) } else { name };
            make(&name, body.to_owned(), detail, "snippet", false)
        })
        .collect()
}

/// The kind of body the key expects first.
fn shaped_first(role: Role, key: &str) -> Body {
    shapes(role, key)
        .first()
        .map_or(Body::Object, |(_, _, body)| body_kind(body))
}

/// What any value can be: a block, an array, a string, and the literals.
fn generic(first: Body, literals_too: bool) -> Vec<Item> {
    let mut list = vec![
        (Body::Object, "{}", "Object", "{$0}"),
        (Body::Array, "[]", "Array", "[$0]"),
        (Body::Text, "\"\"", "String", "\"$0\""),
    ];
    list.sort_by_key(|(body, ..)| *body != first);
    let mut items: Vec<Item> = list
        .into_iter()
        .map(|(_, name, detail, body)| make(name, body.to_owned(), detail, "snippet", false))
        .collect();
    if literals_too {
        items.extend(literals(&["true", "false", "null"], "Literal"));
    }
    items
}

/// The items without a label twice, the first one kept.
fn unique(items: Vec<Item>) -> Vec<Item> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.label.clone()))
        .collect()
}

/// The collections a payload names besides its own: `from`, `coll` and `$unionWith`.
pub fn referenced(payload: &str) -> Vec<String> {
    let payload = mask(payload);
    let (tokens, _) = lex(&payload);
    let mut found = Vec::new();
    for window in tokens.windows(3) {
        if let [
            Lex::Text(key) | Lex::Word(key),
            Lex::Punct(':'),
            Lex::Text(name),
        ] = window
            && matches!(*key, "from" | "coll" | "$unionWith")
            && !name.is_empty()
            && !found.iter().any(|known| known == name)
        {
            found.push((*name).to_owned());
        }
    }
    found
}
