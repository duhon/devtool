use crate::completion::block_snippet;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Name,
    Number,
    String,
    Symbol,
}
#[derive(Clone, Copy)]
struct Token<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    kind: Kind,
}
fn lex(source: &str) -> Option<Vec<Token<'_>>> {
    lex_prefix(source, false)
}
fn lex_prefix(source: &str, partial: bool) -> Option<Vec<Token<'_>>> {
    let bytes = source.as_bytes();
    let mut tokens = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if source[i..].starts_with('\u{feff}') {
            i += '\u{feff}'.len_utf8();
            continue;
        }
        let start = i;
        let kind = match bytes[i] {
            b' ' | b'\t' | b'\r' | b'\n' | b',' => {
                i += 1;
                continue;
            }
            b'#' => {
                let Some(end) = source[i..].find(['\n', '\r']) else {
                    return partial.then_some(tokens);
                };
                i += end;
                continue;
            }
            b'"' => {
                let block = source[i..].starts_with("\"\"\"");
                let delimiter = if block { "\"\"\"" } else { "\"" };
                i += delimiter.len();
                loop {
                    if i >= bytes.len() {
                        return partial.then_some(tokens);
                    }
                    if source[i..].starts_with(delimiter) {
                        i += delimiter.len();
                        break;
                    }
                    if bytes[i] == b'\\' {
                        i += 1;
                        if i >= bytes.len() {
                            return partial.then_some(tokens);
                        }
                    }
                    i += source[i..].chars().next()?.len_utf8();
                }
                Kind::String
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                Kind::Name
            }
            b'0'..=b'9' | b'-' => {
                i += 1;
                while i < bytes.len()
                    && (bytes[i].is_ascii_digit()
                        || matches!(bytes[i], b'.' | b'e' | b'E' | b'+' | b'-'))
                {
                    i += 1;
                }
                Kind::Number
            }
            b'.' if source[i..].starts_with("...") => {
                i += 3;
                Kind::Symbol
            }
            _ => {
                i += source[i..].chars().next()?.len_utf8();
                Kind::Symbol
            }
        };
        tokens.push(Token {
            text: &source[start..i],
            start,
            end: i,
            kind,
        });
    }
    Some(tokens)
}

pub fn operation_fold(source: &str) -> Option<(usize, usize)> {
    let mut tokens = lex_prefix(source, true)?.into_iter().peekable();
    let operation = tokens.next()?;
    if !matches!(operation.text, "query" | "mutation" | "subscription") {
        return None;
    }
    let header = if tokens.peek().is_some_and(|token| token.kind == Kind::Name) {
        tokens.next()?.end
    } else {
        operation.end
    };
    let mut parentheses = 0usize;
    let opening = loop {
        let token = tokens.next()?;
        if parentheses > 0 {
            match token.text {
                "(" => parentheses += 1,
                ")" => parentheses -= 1,
                _ => {}
            }
            continue;
        }
        match token.text {
            "(" => parentheses += 1,
            "@" => {
                if tokens.next()?.kind != Kind::Name {
                    return None;
                }
            }
            "{" => break token,
            _ => return None,
        }
    };
    let mut braces = 1usize;
    for token in tokens {
        match token.text {
            "{" => braces += 1,
            "}" => {
                braces -= 1;
                if braces == 0 {
                    let first_end = source.find(['\r', '\n']).unwrap_or(source.len());
                    return Some(if opening.start < first_end {
                        (opening.end, token.start)
                    } else {
                        (header.min(first_end), token.end)
                    });
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq)]
enum Type {
    Named(String),
    List(Box<Type>),
    NonNull(Box<Type>),
}
impl Type {
    fn from_schema(value: &Value) -> Option<Self> {
        Self::read_schema(value, 0)
    }
    fn read_schema(value: &Value, depth: usize) -> Option<Self> {
        if depth > 32 {
            return None;
        }
        if let Some(name) = value.as_str() {
            return Some(Self::Named(name.into()));
        }
        match value["kind"].as_str() {
            Some("NON_NULL") => Some(Self::NonNull(Box::new(Self::read_schema(
                &value["ofType"],
                depth + 1,
            )?))),
            Some("LIST") => Some(Self::List(Box::new(Self::read_schema(
                &value["ofType"],
                depth + 1,
            )?))),
            _ => Some(Self::Named(value["name"].as_str()?.into())),
        }
    }
    fn nullable(&self) -> &Self {
        match self {
            Self::NonNull(inner) => inner,
            _ => self,
        }
    }
    fn name(&self) -> &str {
        match self {
            Self::Named(name) => name,
            Self::List(inner) | Self::NonNull(inner) => inner.name(),
        }
    }
    fn label(&self) -> String {
        match self {
            Self::Named(name) => name.clone(),
            Self::List(inner) => format!("[{}]", inner.label()),
            Self::NonNull(inner) => format!("{}!", inner.label()),
        }
    }
    fn accepts(&self, variable: &Self, default: bool) -> bool {
        match (self, variable) {
            (Self::NonNull(expected), Self::NonNull(actual)) => expected.accepts(actual, false),
            (Self::NonNull(expected), actual) if default => expected.accepts(actual, false),
            (Self::NonNull(_), _) => false,
            (expected, Self::NonNull(actual)) => expected.accepts(actual, false),
            (Self::List(expected), Self::List(actual)) => expected.accepts(actual, false),
            (Self::Named(expected), Self::Named(actual)) => expected == actual,
            _ => false,
        }
    }
}
#[derive(Clone)]
struct Variable {
    name: String,
    ty: Type,
    default: bool,
}
#[derive(Default)]
struct Selection {
    typename: bool,
    spreads: BTreeSet<String>,
}
struct Fragment {
    name: String,
    condition: String,
    selection: Selection,
    dependencies: BTreeSet<String>,
}
struct DirectiveSite {
    location: &'static str,
    used: BTreeSet<String>,
}
enum Context<'a> {
    Document(bool),
    Header(bool),
    FragmentOn,
    VariableName,
    InputTypes,
    Tokens(Vec<&'static str>),
    Fields {
        name: String,
        used: BTreeSet<String>,
        args: bool,
        typename: bool,
        spreads: Option<BTreeSet<String>>,
        directives: Option<DirectiveSite>,
    },
    Spread(String, BTreeSet<String>, bool),
    TypeConditions(Option<String>),
    ObjectTail(bool, bool, Option<DirectiveSite>),
    Directives(&'static str, BTreeSet<String>),
    Members(&'a [Value], BTreeSet<String>, &'static str),
    Values {
        ty: Option<Type>,
        variables: bool,
        default: bool,
        closing: Option<&'static str>,
    },
    Quiet,
}
type ParseResult<'a, T> = Result<T, Context<'a>>;
struct Parser<'a> {
    tokens: Vec<Token<'a>>,
    i: usize,
    schema: &'a Value,
    variables: Vec<Variable>,
    fragments: Vec<Fragment>,
    fragment: Option<String>,
    operation: bool,
    objects: Vec<(usize, usize)>,
}
fn items(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn directive_definitions(schema: &Value) -> &[Value] {
    static BUILTINS: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
        serde_json::json!(["include", "skip"].map(|name| serde_json::json!({
            "name":name,
            "locations":["FIELD","FRAGMENT_SPREAD","INLINE_FRAGMENT"],
            "isRepeatable":false,
            "args":[{"name":"if","type":{"kind":"NON_NULL","ofType":{"kind":"SCALAR","name":"Boolean"}}}]
        })))
    });
    schema["directives"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| items(&BUILTINS))
}
fn directive_allowed(directive: &Value, location: &str) -> bool {
    items(&directive["locations"])
        .iter()
        .any(|value| value == location)
}
fn field_type(field: &Value) -> Option<Type> {
    Type::from_schema(&field["type"]).or_else(|| {
        field["typeName"]
            .as_str()
            .map(|name| Type::Named(name.into()))
    })
}
fn is_object(field: &Value, ty: Option<&Type>, schema: &Value) -> bool {
    field["isObject"] == true
        || field["kind"] == "object"
        || ty.is_some_and(|t| {
            let target = &schema["types"][t.name()];
            matches!(
                target["kind"].as_str(),
                Some("OBJECT" | "INTERFACE" | "UNION")
            ) || !items(&target["fields"]).is_empty()
        })
}
fn required_missing(fields: &[Value], used: &BTreeSet<String>) -> bool {
    fields.iter().any(|field| {
        field_type(field).is_some_and(|ty| matches!(ty, Type::NonNull(_)))
            && field["defaultValue"].is_null()
            && field["name"]
                .as_str()
                .is_some_and(|name| !used.contains(name))
    })
}
fn composite(ty: &Value) -> bool {
    matches!(ty["kind"].as_str(), Some("OBJECT" | "INTERFACE" | "UNION"))
        || (ty["kind"].is_null() && ty["fields"].is_array())
}
fn possible_objects<'a>(schema: &'a Value, name: &'a str) -> BTreeSet<&'a str> {
    let ty = &schema["types"][name];
    if ty["kind"] == "OBJECT" || (ty["kind"].is_null() && ty["fields"].is_array()) {
        return BTreeSet::from([name]);
    }
    items(&ty["possibleTypes"])
        .iter()
        .filter_map(|value| value.as_str().or_else(|| value["name"].as_str()))
        .collect()
}
fn compatible(schema: &Value, parent: &str, condition: &str) -> bool {
    parent == condition
        || !possible_objects(schema, parent).is_disjoint(&possible_objects(schema, condition))
}
fn type_conditions<'a>(
    schema: &'a Value,
    parent: Option<&'a str>,
) -> impl Iterator<Item = (&'a String, &'a Value)> + 'a {
    schema["types"]
        .as_object()
        .into_iter()
        .flat_map(|types| types.iter())
        .filter(move |(name, ty)| {
            composite(ty) && parent.is_none_or(|parent| compatible(schema, parent, name))
        })
}
impl<'a> Parser<'a> {
    fn new(tokens: Vec<Token<'a>>, schema: &'a Value) -> Self {
        Self {
            tokens,
            i: 0,
            schema,
            variables: vec![],
            fragments: vec![],
            fragment: None,
            operation: false,
            objects: vec![],
        }
    }
    fn end(&self) -> bool {
        self.i == self.tokens.len()
    }
    fn take(&mut self, text: &str) -> bool {
        if self.tokens.get(self.i).is_some_and(|t| t.text == text) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn name(&mut self) -> Option<&'a str> {
        let token = self.tokens.get(self.i).filter(|t| t.kind == Kind::Name)?;
        self.i += 1;
        Some(token.text)
    }
    fn expect(&mut self, text: &'static str) -> ParseResult<'a, ()> {
        if self.take(text) {
            Ok(())
        } else if self.end() {
            Err(Context::Tokens(vec![text]))
        } else {
            Err(Context::Quiet)
        }
    }
    fn document(&mut self) -> ParseResult<'a, ()> {
        loop {
            if self.end() {
                return Err(Context::Document(!self.operation));
            }
            self.variables.clear();
            if self.take("fragment") {
                let name = self.name().ok_or(Context::Quiet)?.to_owned();
                if name == "on" || self.fragments.iter().any(|fragment| fragment.name == name) {
                    return Err(Context::Quiet);
                }
                self.fragment = Some(name.clone());
                if self.end() {
                    return Err(Context::FragmentOn);
                }
                self.expect("on")?;
                if self.end() {
                    return Err(Context::TypeConditions(None));
                }
                let condition = self.name().ok_or(Context::Quiet)?.to_owned();
                let ty = &self.schema["types"][&condition];
                if !ty.is_null() && !composite(ty) {
                    return Err(Context::Quiet);
                }
                let directives = self.directives("FRAGMENT_DEFINITION", 0)?;
                if self.end() {
                    return Err(Context::ObjectTail(false, true, Some(directives)));
                }
                self.expect("{")?;
                let start = self.i;
                let selection = self.selection(&condition, 0, true)?;
                let dependencies = self.tokens[start..self.i]
                    .windows(2)
                    .filter(|tokens| {
                        tokens[0].text == "..."
                            && tokens[1].kind == Kind::Name
                            && tokens[1].text != "on"
                    })
                    .map(|tokens| tokens[1].text.to_owned())
                    .collect();
                self.fragments.push(Fragment {
                    name,
                    condition,
                    selection,
                    dependencies,
                });
                self.fragment = None;
                continue;
            }
            let root = if self.take("query") {
                "queryType"
            } else if self.take("mutation") {
                "mutationType"
            } else if self.take("subscription") {
                "subscriptionType"
            } else if self.tokens[self.i].text == "{" {
                "queryType"
            } else {
                return Err(Context::Quiet);
            };
            self.operation = true;
            if self
                .tokens
                .get(self.i)
                .is_some_and(|t| t.kind == Kind::Name)
            {
                self.name();
            }
            let definitions = self.take("(");
            if definitions {
                self.definitions()?;
            }
            if self.end() {
                return Err(Context::Header(!definitions));
            }
            let location = match root {
                "mutationType" => "MUTATION",
                "subscriptionType" => "SUBSCRIPTION",
                _ => "QUERY",
            };
            self.directives(location, 0)?;
            if self.end() {
                return Err(Context::Header(false));
            }
            self.expect("{")?;
            let name = self.schema[root].as_str().unwrap_or("").to_owned();
            self.selection(&name, 0, root != "subscriptionType")?;
        }
    }
    fn fragment_typename(
        &self,
        name: &str,
        path: &mut BTreeSet<String>,
        cache: &mut BTreeMap<String, Option<bool>>,
    ) -> Option<bool> {
        if let Some(result) = cache.get(name) {
            return *result;
        }
        let fragment = self
            .fragments
            .iter()
            .find(|fragment| fragment.name == name)?;
        if self.fragment.as_deref() == Some(name) || path.len() >= 64 || !path.insert(name.into()) {
            return None;
        }
        let result = fragment.dependencies.iter().try_fold(
            fragment.selection.typename,
            |typename, dependency| {
                let nested = self.fragment_typename(dependency, path, cache)?;
                Some(typename || (fragment.selection.spreads.contains(dependency) && nested))
            },
        );
        path.remove(name);
        cache.insert(name.into(), result);
        result
    }
    fn available_fragments(
        &self,
        parent: &str,
        used: &BTreeSet<String>,
        typename: bool,
    ) -> Candidates {
        let mut cache = BTreeMap::new();
        self.fragments
            .iter()
            .filter(|defined| {
                !used.contains(&defined.name)
                    && compatible(self.schema, parent, &defined.condition)
                    && self
                        .fragment_typename(&defined.name, &mut BTreeSet::new(), &mut cache)
                        .is_some_and(|meta| typename || !meta)
            })
            .map(|defined| {
                (
                    defined.name.clone(),
                    format!("Fragment on {}", defined.condition),
                )
            })
            .collect()
    }
    fn definitions(&mut self) -> ParseResult<'a, ()> {
        while !self.take(")") {
            if self.end() {
                return Err(Context::VariableName);
            }
            self.expect("$")?;
            let name = self.name().ok_or(Context::Quiet)?.to_owned();
            self.expect(":")?;
            let ty = self.input_type(0)?;
            let mut default = false;
            let has_default = self.take("=");
            if has_default {
                default = self.tokens.get(self.i).is_none_or(|t| t.text != "null");
                self.value(Some(ty.clone()), false, false, 0)?;
            }
            self.directives("VARIABLE_DEFINITION", 0)?;
            self.variables.push(Variable {
                name,
                ty: ty.clone(),
                default,
            });
            if self.end() {
                let mut next = vec!["$", ")"];
                if !has_default {
                    next.insert(0, "=");
                    if !matches!(ty, Type::NonNull(_)) {
                        next.insert(0, "!");
                    }
                }
                return Err(Context::Tokens(next));
            }
        }
        Ok(())
    }
    fn input_type(&mut self, depth: usize) -> ParseResult<'a, Type> {
        if depth > 64 {
            return Err(Context::Quiet);
        }
        let mut ty = if self.take("[") {
            let inner = self.input_type(depth + 1)?;
            if self.end() {
                return Err(Context::Tokens(if matches!(inner, Type::NonNull(_)) {
                    vec!["]"]
                } else {
                    vec!["!", "]"]
                }));
            }
            self.expect("]")?;
            Type::List(Box::new(inner))
        } else if let Some(name) = self.name() {
            Type::Named(name.into())
        } else if self.end() {
            return Err(Context::InputTypes);
        } else {
            return Err(Context::Quiet);
        };
        if self.take("!") {
            ty = Type::NonNull(Box::new(ty));
        }
        Ok(ty)
    }
    fn directives(
        &mut self,
        location: &'static str,
        depth: usize,
    ) -> ParseResult<'a, DirectiveSite> {
        let mut used = BTreeSet::new();
        while self.take("@") {
            if self.end() {
                return Err(Context::Directives(location, used));
            }
            let name = self.name().ok_or(Context::Quiet)?;
            used.insert(name.into());
            let definition = directive_definitions(self.schema)
                .iter()
                .find(|directive| directive["name"] == name);
            let args = definition
                .map(|directive| items(&directive["args"]))
                .unwrap_or(&[]);
            if self.take("(") {
                self.members(args, ")", location != "VARIABLE_DEFINITION", depth)?;
            } else if self.end() && !args.is_empty() {
                return Err(Context::ObjectTail(true, false, None));
            }
        }
        Ok(DirectiveSite { location, used })
    }
    fn selection(
        &mut self,
        name: &str,
        depth: usize,
        typename: bool,
    ) -> ParseResult<'a, Selection> {
        if depth > 64 {
            return Err(Context::Quiet);
        }
        let fields = items(&self.schema["types"][name]["fields"]);
        let mut used = BTreeSet::new();
        let mut selected = Selection::default();
        let mut tail_directives = None;
        while !self.take("}") {
            if self.end() {
                return Err(Context::Fields {
                    name: name.into(),
                    used,
                    args: false,
                    typename,
                    spreads: Some(selected.spreads),
                    directives: tail_directives,
                });
            }
            if self.take("...") {
                tail_directives = None;
                if self.end() {
                    return Err(Context::Spread(name.into(), selected.spreads, typename));
                }
                if self.take("on") {
                    if self.end() {
                        return Err(Context::TypeConditions(Some(name.into())));
                    }
                    let condition = self.name().ok_or(Context::Quiet)?;
                    let directives = self.directives("INLINE_FRAGMENT", depth)?;
                    if self.end() {
                        return Err(Context::ObjectTail(false, true, Some(directives)));
                    }
                    self.expect("{")?;
                    let selection = self.selection(condition, depth + 1, typename)?;
                    selected.typename |= selection.typename;
                    selected.spreads.extend(selection.spreads);
                } else if self
                    .tokens
                    .get(self.i)
                    .is_some_and(|token| matches!(token.text, "@" | "{"))
                {
                    let directives = self.directives("INLINE_FRAGMENT", depth)?;
                    if self.end() {
                        return Err(Context::ObjectTail(false, true, Some(directives)));
                    }
                    self.expect("{")?;
                    let selection = self.selection(name, depth + 1, typename)?;
                    selected.typename |= selection.typename;
                    selected.spreads.extend(selection.spreads);
                } else {
                    selected
                        .spreads
                        .insert(self.name().ok_or(Context::Quiet)?.into());
                    tail_directives = Some(self.directives("FRAGMENT_SPREAD", depth)?);
                }
                continue;
            }
            let response_name = self.name().ok_or(Context::Quiet)?;
            tail_directives = None;
            let actual_name = if self.take(":") {
                if self.end() {
                    return Err(Context::Fields {
                        name: name.into(),
                        used: BTreeSet::new(),
                        args: false,
                        typename,
                        spreads: None,
                        directives: None,
                    });
                }
                self.name().ok_or(Context::Quiet)?
            } else {
                response_name
            };
            selected.typename |= actual_name == "__typename";
            used.insert(response_name.into());
            let field = fields.iter().find(|field| field["name"] == actual_name);
            let args = field.map(|f| items(&f["args"])).unwrap_or(&[]);
            let ty = field.and_then(field_type);
            let object = field.is_some_and(|f| is_object(f, ty.as_ref(), self.schema));
            if object {
                let token = &self.tokens[self.i - 1];
                self.objects.push((token.start, token.end));
            }
            let has_arguments = self.take("(");
            if has_arguments {
                self.members(args, ")", true, depth)?;
            }
            let directives = self.directives("FIELD", depth)?;
            let has_directives = !directives.used.is_empty();
            if self.take("{") {
                self.selection(ty.as_ref().map(Type::name).unwrap_or(""), depth + 1, true)?;
            } else if self.end() {
                let required = !has_arguments && required_missing(args, &BTreeSet::new());
                if object {
                    return Err(Context::ObjectTail(
                        !args.is_empty() && !has_arguments && !has_directives,
                        !required,
                        (!required).then_some(directives),
                    ));
                }
                if required {
                    return Err(Context::ObjectTail(true, false, None));
                }
                return Err(Context::Fields {
                    name: name.into(),
                    used,
                    args: !args.is_empty() && !has_arguments && !has_directives,
                    typename,
                    spreads: Some(selected.spreads),
                    directives: Some(directives),
                });
            }
        }
        Ok(selected)
    }
    fn members(
        &mut self,
        fields: &'a [Value],
        closing: &'static str,
        variables: bool,
        depth: usize,
    ) -> ParseResult<'a, ()> {
        let mut used = BTreeSet::new();
        while !self.take(closing) {
            if self.end() {
                return Err(Context::Members(fields, used, closing));
            }
            let name = self.name().ok_or(Context::Quiet)?;
            used.insert(name.into());
            self.expect(":")?;
            let field = fields.iter().find(|f| f["name"] == name);
            let ty = field.and_then(field_type);
            let default = field.is_some_and(|f| !f["defaultValue"].is_null());
            self.value(ty, variables, default, depth + 1)?;
            if self.end() {
                return Err(Context::Members(fields, used, closing));
            }
        }
        Ok(())
    }
    fn value(
        &mut self,
        ty: Option<Type>,
        variables: bool,
        default: bool,
        depth: usize,
    ) -> ParseResult<'a, ()> {
        if depth > 64 {
            return Err(Context::Quiet);
        }
        if self.end() {
            return Err(Context::Values {
                ty,
                variables,
                default,
                closing: None,
            });
        }
        if self.take("$") {
            if !variables {
                return Err(Context::Quiet);
            }
            self.name().ok_or(Context::Quiet)?;
        } else if self.take("{") {
            let fields = ty
                .as_ref()
                .map(|t| items(&self.schema["types"][t.name()]["inputFields"]))
                .unwrap_or(&[]);
            self.members(fields, "}", variables, depth)?;
        } else if self.take("[") {
            let inner = ty.as_ref().and_then(|t| match t.nullable() {
                Type::List(inner) => Some(*inner.clone()),
                _ => None,
            });
            while !self.take("]") {
                if self.end() {
                    return Err(Context::Values {
                        ty: inner,
                        variables,
                        default: false,
                        closing: Some("]"),
                    });
                }
                self.value(inner.clone(), variables, false, depth + 1)?;
            }
        } else if self
            .tokens
            .get(self.i)
            .is_some_and(|t| t.kind != Kind::Symbol)
        {
            self.i += 1;
        } else {
            return Err(Context::Quiet);
        }
        Ok(())
    }
}

type Candidates = Vec<(String, String)>;

fn directive_candidates(
    schema: &Value,
    site: &DirectiveSite,
    prefix: &str,
    arguments: bool,
) -> Candidates {
    directive_definitions(schema)
        .iter()
        .filter(|directive| directive_allowed(directive, site.location))
        .filter(|directive| {
            directive["isRepeatable"] == true
                || directive["name"]
                    .as_str()
                    .is_some_and(|name| !site.used.contains(name))
        })
        .filter_map(|directive| {
            let name = directive["name"].as_str()?;
            let tail = if arguments && !items(&directive["args"]).is_empty() {
                "()"
            } else {
                ""
            };
            Some((format!("{prefix}{name}{tail}"), "Directive".into()))
        })
        .collect()
}

/// `console` is the service of a Console document; `None` for a single query.
pub fn object_fields(
    source: &str,
    schema: &Value,
    console: Option<&str>,
) -> Vec<(usize, usize, usize)> {
    let mut sections = vec![];
    if let Some(service) = console {
        let service = crate::services::get(service);
        let section = |marker: &str| service.as_ref().and_then(|service| service.section(marker));
        let mut start = 0;
        let mut query = None::<Vec<&str>>;
        for (line, text) in source.split('\n').enumerate() {
            let marker = text
                .trim()
                .strip_prefix('#')
                .unwrap_or("")
                .to_ascii_lowercase();
            let request = marker
                .split_whitespace()
                .next()
                .is_some_and(|name| matches!(name, "request" | "response" | "responce"));
            if request || section(&marker).is_some() {
                if let Some(body) = query.take() {
                    sections.push((start, body.join("\n")));
                }
                if section(&marker).is_some_and(|section| section.language == "graphql") {
                    start = line + 1;
                    query = Some(vec![]);
                }
            } else if let Some(body) = &mut query {
                body.push(text);
            }
        }
        if let Some(body) = query {
            sections.push((start, body.join("\n")));
        }
    } else {
        sections.push((0, source.into()));
    }
    let mut fields = vec![];
    for (start_line, body) in sections {
        let Some(tokens) = lex_prefix(&body, true) else {
            continue;
        };
        let mut parser = Parser::new(tokens, schema);
        let _ = parser.document();
        let mut lines = vec![0];
        lines.extend(
            body.bytes()
                .enumerate()
                .filter_map(|(i, c)| (c == b'\n').then_some(i + 1)),
        );
        for (start, end) in parser.objects {
            let line = lines.partition_point(|offset| *offset <= start) - 1;
            fields.push((
                start_line + line,
                body[lines[line]..start].encode_utf16().count(),
                body[lines[line]..end].encode_utf16().count(),
            ));
        }
    }
    fields
}

fn following(mut source: &str) -> Option<char> {
    loop {
        source =
            source.trim_start_matches(|c: char| c.is_whitespace() || c == ',' || c == '\u{feff}');
        if source.starts_with('#') {
            source = &source[source.find(['\r', '\n'])?..];
        } else {
            return source.chars().next();
        }
    }
}

fn operation_declarations(source: &str, schema: &Value) -> Vec<(usize, Vec<Variable>)> {
    let Some(tokens) = lex_prefix(source, true) else {
        return vec![];
    };
    let mut declarations = vec![];
    let (mut braces, mut parentheses) = (0usize, 0usize);
    let mut definition = true;
    for (i, token) in tokens.iter().enumerate() {
        if definition {
            if matches!(token.text, "query" | "mutation" | "subscription") {
                definition = false;
                let mut header = Parser::new(tokens[i + 1..].to_vec(), schema);
                header.name();
                if header.take("(") && header.definitions().is_err() {
                    declarations.push((token.start, vec![]));
                } else {
                    declarations.push((token.start, header.variables));
                }
            } else if token.text == "{" {
                definition = false;
                declarations.push((token.start, vec![]));
            } else if token.text == "fragment" {
                definition = false;
            }
        }
        match token.text {
            "{" => braces += 1,
            "}" => {
                braces = braces.saturating_sub(1);
                if braces == 0 && parentheses == 0 {
                    definition = true;
                }
            }
            "(" => parentheses += 1,
            ")" => parentheses = parentheses.saturating_sub(1),
            _ => {}
        }
    }
    declarations
}

fn fragment_variables(source: &str, schema: &Value) -> Vec<Variable> {
    let mut declarations = operation_declarations(source, schema)
        .into_iter()
        .map(|(_, variables)| variables);
    let mut common = declarations.next().unwrap_or_default();
    // A fragment may be shared; do not suggest variables absent from another operation.
    for variables in declarations {
        common.retain_mut(|variable| {
            if let Some(other) = variables
                .iter()
                .find(|other| other.name == variable.name && other.ty == variable.ty)
            {
                variable.default &= other.default;
                true
            } else {
                false
            }
        });
    }
    common
}

pub(super) fn operation_complete(source: &str, schema: &Value) -> bool {
    let Some(tokens) = lex(source) else {
        return false;
    };
    matches!(
        Parser::new(tokens, schema).document(),
        Err(Context::Document(false))
    )
}

pub(super) fn suggest(
    body: &str,
    before: &str,
    after: &str,
    schema: &Value,
    request_variables: &Value,
) -> (String, Candidates, bool, bool) {
    let Some(mut tokens) = lex(body) else {
        return (String::new(), vec![], false, false);
    };
    let mut fragment = String::new();
    if let Some(last) = tokens.last()
        && last.end == body.len()
        && (last.kind == Kind::Name || last.text == "$")
    {
        let mut index = tokens.len() - 1;
        if index > 0 && tokens[index - 1].text == "$" && tokens[index - 1].end == last.start {
            index -= 1;
        }
        fragment = body[tokens[index].start..].into();
        tokens.truncate(index);
    }
    if fragment.is_empty() && body.ends_with('.') && !body.ends_with("...") {
        let start = body.trim_end_matches('.').len();
        if body.len() - start <= 2 {
            fragment = body[start..].into();
            tokens.retain(|token| token.end <= start);
        }
    }
    let mut parser = Parser::new(tokens, schema);
    let context = parser.document().err().unwrap_or(Context::Quiet);
    if parser.fragment.is_some()
        && matches!(
            &context,
            Context::Values {
                variables: true,
                ..
            }
        )
    {
        parser.variables = fragment_variables(&format!("{body}{after}"), schema);
    }
    let next = following(after);
    if fragment.is_empty()
        && (after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$')
            || (matches!(&context, Context::Values { .. })
                && next.is_some_and(|c| !matches!(c, ')' | ']' | '}')))
            || (matches!(&context, Context::InputTypes)
                && next.is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '['))
            || (matches!(&context, Context::TypeConditions(_))
                && next.is_some_and(|c| c.is_ascii_alphabetic() || c == '_'))
            || (matches!(&context, Context::FragmentOn | Context::Spread(..))
                && next.is_some_and(|c| c.is_ascii_alphabetic() || c == '_')))
    {
        return (fragment, vec![], false, false);
    }
    let automatic_directives_only = fragment.is_empty()
        && !before.trim().is_empty()
        && matches!(
            &context,
            Context::Fields {
                args: false,
                directives: Some(_),
                ..
            }
        );
    let automatic = match &context {
        Context::Fields {
            name,
            args,
            spreads,
            directives,
            ..
        } => {
            if fragment.is_empty() {
                before.trim().is_empty()
                    || *args
                    || (spreads.is_none() && before.ends_with(char::is_whitespace))
                    || (directives.is_some() && before.ends_with(char::is_whitespace))
            } else {
                fragment != "__typename"
                    && !items(&schema["types"][name]["fields"])
                        .iter()
                        .any(|field| field["name"] == fragment)
            }
        }
        Context::Document(_) => !fragment.is_empty() || before.trim().is_empty(),
        Context::Spread(..) => !parser
            .fragments
            .iter()
            .any(|defined| defined.name == fragment),
        Context::TypeConditions(_) => !composite(&schema["types"][&fragment]),
        Context::Directives(_, _) => !directive_definitions(schema)
            .iter()
            .any(|directive| directive["name"] == fragment),
        Context::Quiet => false,
        _ => true,
    };
    let parsed_variables = request_variables
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    let variable_names: Vec<_> = parsed_variables
        .as_ref()
        .unwrap_or(request_variables)
        .as_object()
        .into_iter()
        .flat_map(|values| values.keys())
        .filter(|name| {
            let mut bytes = name.bytes();
            bytes
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
                && bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_')
        })
        .collect();
    let declarations = || {
        variable_names
            .iter()
            .filter(|name| {
                !parser
                    .variables
                    .iter()
                    .any(|variable| &variable.name == **name)
            })
            .map(|name| (format!("${name}: "), "Request variable".into()))
            .collect()
    };
    let object = || block_snippet(before, '{', '}');
    let separated = |text: String| {
        if before.is_empty() || before.ends_with(char::is_whitespace) {
            text
        } else {
            format!(" {text}")
        }
    };
    let token_candidates = |tokens: Vec<&str>| {
        tokens
            .into_iter()
            .map(|text| (text.into(), "GraphQL".into()))
            .collect::<Candidates>()
    };
    let on = || {
        (
            if fragment.is_empty() {
                separated("on ".into())
            } else {
                "on ".into()
            },
            "Type condition".into(),
        )
    };
    let mut candidates = match context {
        Context::Document(fragments) => {
            if !fragments {
                return (fragment, vec![], automatic, false);
            }
            let source = format!("{body}{after}");
            let start = body.len().saturating_sub(fragment.len());
            let existing = operation_declarations(&source, schema)
                .iter()
                .any(|(position, _)| *position != start);
            let mut result = if existing {
                vec![]
            } else {
                token_candidates(vec!["query", "mutation", "subscription"])
            };
            result.push(("fragment".into(), "GraphQL".into()));
            if !existing && next != Some('{') {
                result.push((object(), "Selection set".into()));
            }
            result
        }
        Context::FragmentOn => vec![on()],
        Context::Header(definitions) => {
            let mut result = vec![];
            if next != Some('(') && !next.is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
                if next != Some('{') {
                    result.push((separated(object()), "Selection set".into()));
                }
                if definitions {
                    result.push((separated("()".into()), "Variable definitions".into()));
                }
            }
            result
        }
        Context::VariableName => declarations(),
        Context::InputTypes => {
            let types = schema["types"].as_object();
            let typed_schema =
                types.is_some_and(|types| types.values().any(|ty| ty["kind"].is_string()));
            let mut result: Candidates = ["ID", "String", "Int", "Float", "Boolean"]
                .into_iter()
                .filter(|name| {
                    !typed_schema
                        || types.is_some_and(|types| {
                            types.get(*name).is_some_and(|ty| ty["kind"] == "SCALAR")
                        })
                })
                .map(|name| (name.into(), "Input type".into()))
                .collect();
            if let Some(types) = types {
                result.extend(
                    types
                        .iter()
                        .filter(|(name, ty)| {
                            !name.starts_with("__")
                                && matches!(
                                    ty["kind"].as_str(),
                                    Some("SCALAR" | "ENUM" | "INPUT_OBJECT")
                                )
                        })
                        .map(|(name, ty)| (name.clone(), ty["kind"].as_str().unwrap_or("").into())),
                );
            }
            result.push(("[".into(), "List type".into()));
            result
        }
        Context::Tokens(tokens) => tokens
            .into_iter()
            .flat_map(|token| {
                if token == "$" {
                    declarations()
                } else {
                    token_candidates(vec![token])
                }
            })
            .collect(),
        Context::Fields {
            name,
            used,
            args,
            typename,
            spreads,
            directives,
        } => {
            let mut result = vec![];
            if fragment.is_empty() && !before.trim().is_empty() && args {
                if args && next != Some('(') {
                    result.push(("()".into(), "Arguments".into()));
                }
                if next != Some('(')
                    && let Some(site) = &directives
                {
                    result.extend(directive_candidates(schema, site, "@", true));
                }
                return (fragment, result, automatic, false);
            }
            for field in items(&schema["types"][&name]["fields"]) {
                let Some(name) = field["name"].as_str() else {
                    continue;
                };
                if used.contains(name) {
                    continue;
                }
                let ty = field_type(field);
                let detail = ty.as_ref().map(Type::label).unwrap_or_default();
                result.push((name.into(), detail));
            }
            if typename
                && !used.contains("__typename")
                && (fragment.is_empty() || fragment.starts_with('_'))
            {
                result.push(("__typename".into(), "String!".into()));
            }
            if let Some(spreads) = spreads {
                let spread_next = following(
                    after.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_'),
                );
                if !matches!(spread_next, Some('{' | '(' | ':')) {
                    result.extend(
                        parser
                            .available_fragments(&name, &spreads, typename)
                            .into_iter()
                            .map(|(name, detail)| (format!("...{name}"), detail)),
                    );
                }
                if type_conditions(schema, Some(&name)).any(|(condition, _)| condition != &name) {
                    result.push(("... on ".into(), "Type condition".into()));
                }
            }
            if next != Some('(')
                && let Some(site) = &directives
            {
                result.extend(directive_candidates(schema, site, "@", true));
            }
            result
        }
        Context::Spread(parent, used, typename) => {
            let mut result = vec![];
            if !matches!(next, Some('{' | '(')) {
                result.extend(parser.available_fragments(&parent, &used, typename));
            }
            if type_conditions(schema, Some(&parent)).any(|(condition, _)| condition != &parent) {
                result.push(on());
            }
            result.extend(directive_candidates(
                schema,
                &DirectiveSite {
                    location: "INLINE_FRAGMENT",
                    used: BTreeSet::new(),
                },
                "@",
                true,
            ));
            result
        }
        Context::TypeConditions(parent) => type_conditions(schema, parent.as_deref())
            .map(|(name, ty)| (name.clone(), ty["kind"].as_str().unwrap_or("OBJECT").into()))
            .collect(),
        Context::Directives(location, used) => directive_candidates(
            schema,
            &DirectiveSite { location, used },
            "",
            following(after.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_'))
                != Some('('),
        ),
        Context::ObjectTail(args, selection, directives) => {
            let mut result = vec![];
            if next != Some('(') {
                if selection && next != Some('{') {
                    result.push((separated(object()), "Selection set".into()));
                }
                if args {
                    result.push(("()".into(), "Arguments".into()));
                }
                if let Some(site) = &directives {
                    result.extend(directive_candidates(schema, site, "@", true));
                }
            }
            result
        }
        Context::Members(fields, used, closing) => {
            let mut result: Candidates = fields
                .iter()
                .filter_map(|field| {
                    let name = field["name"].as_str()?;
                    (!used.contains(name)).then(|| {
                        (
                            if fragment.is_empty()
                                && !before.is_empty()
                                && !before.ends_with(char::is_whitespace)
                                && !before.ends_with(['(', '{', ','])
                            {
                                format!(" {name}: ")
                            } else {
                                format!("{name}: ")
                            },
                            field_type(field).map(|t| t.label()).unwrap_or_default(),
                        )
                    })
                })
                .collect();
            if !required_missing(fields, &used) {
                result.push((closing.into(), "Close arguments/input".into()));
            }
            result
        }
        Context::Values {
            ty,
            variables: allow_variables,
            default,
            closing,
        } => {
            let mut result = vec![];
            if allow_variables {
                result.extend(
                    parser
                        .variables
                        .iter()
                        .filter(|variable| {
                            ty.as_ref().is_none_or(|ty| {
                                ty.accepts(&variable.ty, variable.default || default)
                            })
                        })
                        .map(|variable| (format!("${}", variable.name), variable.ty.label())),
                );
                if fragment.starts_with('$') && parser.variables.is_empty() {
                    result.extend(
                        variable_names
                            .iter()
                            .map(|name| (format!("${name}"), "Undeclared request variable".into())),
                    );
                }
            }
            if let Some(ty) = ty {
                if !matches!(ty, Type::NonNull(_)) {
                    result.push(("null".into(), ty.label()));
                }
                let literals: Vec<String> = match ty.nullable() {
                    Type::List(_) => vec!["[]".into()],
                    Type::Named(name) => match name.as_str() {
                        "Boolean" => vec!["true".into(), "false".into()],
                        "Int" | "Float" => vec!["0".into()],
                        "String" | "ID" => vec!["\"\"".into()],
                        _ => {
                            let target = &schema["types"][name];
                            if target["kind"] == "INPUT_OBJECT"
                                || !items(&target["inputFields"]).is_empty()
                            {
                                vec![object()]
                            } else {
                                items(&target["enumValues"])
                                    .iter()
                                    .filter_map(|v| v.as_str().or(v["name"].as_str()))
                                    .filter(|name| {
                                        name.to_ascii_lowercase()
                                            .starts_with(&fragment.to_ascii_lowercase())
                                    })
                                    .map(str::to_owned)
                                    .collect()
                            }
                        }
                    },
                    Type::NonNull(_) => vec![],
                };
                result.extend(literals.into_iter().map(|literal| (literal, ty.label())));
            }
            if let Some(closing) = closing {
                result.push((closing.into(), "Close list".into()));
            }
            result
        }
        Context::Quiet => vec![],
    };
    candidates.retain(|(text, _)| {
        if text == "!" && next == Some('!') {
            return false;
        }
        !matches!(text.as_str(), ")" | "]" | "}")
            || next != text.chars().next()
            || after.starts_with(text.as_str())
    });
    (fragment, candidates, automatic, automatic_directives_only)
}
