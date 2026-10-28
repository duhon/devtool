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

#[cfg(any(test, target_arch = "wasm32"))]
pub(super) fn operation_fold(source: &str) -> Option<(usize, usize)> {
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

#[cfg(any(test, target_arch = "wasm32"))]
pub(crate) fn object_fields(
    source: &str,
    schema: &Value,
    console: bool,
) -> Vec<(usize, usize, usize)> {
    let mut sections = vec![];
    if console {
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
            if request || crate::console::markers("graphql").contains(&marker.as_str()) {
                if let Some(body) = query.take() {
                    sections.push((start, body.join("\n")));
                }
                if marker == "query" {
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

#[cfg(test)]
mod tests {
    use super::{object_fields, operation_fold};
    use crate::completion;
    use serde_json::{Value, json};

    fn named(name: &str) -> Value {
        json!({"kind":"SCALAR","name":name})
    }
    #[test]
    fn operation_folds_include_multiline_headers_but_not_input_defaults_alone() {
        let reference: Value =
            serde_json::from_str(include_str!("../tests/fixtures/graphql-explore.json")).unwrap();
        let document = reference["query"]
            .as_str()
            .unwrap()
            .replace("query ExploreCountries(", "query ExploreCountries (")
            .replace(
                "$filter: CountryFilterInput = { continent: { eq: \"EU\" } }",
                "$filter: CountryFilterInput = {\n    continent: {\n      eq: \"EU\"\n    }\n  }",
            );
        let source = &document[document.find("query ExploreCountries").unwrap()..];
        let (from, to) = operation_fold(source).unwrap();
        assert_eq!(&source[..from], "query ExploreCountries");
        assert_eq!(to, source.trim_end().len());
        assert!(source[from..to].contains("$filter: CountryFilterInput"));
        assert!(source[from..to].contains("...LanguageDetails"));
        for kind in ["query", "mutation", "subscription"] {
            let source = format!(
                "{kind} Q(\n  $input: Input = {{ nested: {{ value: \"🦀 }}\" }} }}\n) {{\n  field {{ name }}\n}}\nfragment Later on User {{ name }}"
            );
            let (from, to) = operation_fold(&source).unwrap();
            assert_eq!(&source[..from], format!("{kind} Q"));
            assert_eq!(&source[to..], "\nfragment Later on User { name }");
        }
        let source = "query Q($input: Input = { nested: { value: \"🦀 }\" } }) { viewer { name } }";
        let (from, to) = operation_fold(source).unwrap();
        assert_eq!(&source[from..to], " viewer { name } ");
        assert_eq!(
            operation_fold("query Q($input: Input = {\n nested: {}\n})"),
            None
        );
        assert_eq!(
            operation_fold("query Q\nfragment Later on User { name }"),
            None
        );
        assert_eq!(operation_fold("query Q {\n viewer { name }"), None);
        assert_eq!(operation_fold("fragment U on User {\n name\n}"), None);
    }
    #[test]
    fn operation_folds_ignore_comments_block_strings_and_directive_inputs() {
        let source = "query Q(\n  $text: String = \"\"\"🦀 ) { }\nquery Fake { }\n\"\"\"\n  # ) { }\n) @trace(input: { label: \"}\" }) {\n  # }\n  viewer(text: \"}\") { name }\n}\nquery Next { viewer { name } }";
        let (from, to) = operation_fold(source).unwrap();
        assert_eq!(&source[..from], "query Q");
        assert_eq!(&source[to..], "\nquery Next { viewer { name } }");
        assert_eq!(source[from..to].chars().last(), Some('}'));
        let source = "query\nQ($input: Input = {}) {\n viewer { name }\n}";
        let (from, to) = operation_fold(source).unwrap();
        assert_eq!(&source[..from], "query");
        assert_eq!(to, source.len());
    }
    fn required(ty: Value) -> Value {
        json!({"kind":"NON_NULL","ofType":ty})
    }
    fn list(ty: Value) -> Value {
        json!({"kind":"LIST","ofType":ty})
    }
    fn schema() -> Value {
        json!({"queryType":"Query","mutationType":"Mutation","subscriptionType":"Subscription","types":{
            "Query":{"kind":"OBJECT","fields":[
                {"name":"viewer","type":named("User"),"isObject":true},
                {"name":"user","type":named("User"),"isObject":true,"args":[
                    {"name":"id","type":required(named("ID"))},
                    {"name":"filter","type":named("Filter")},
                    {"name":"state","type":named("State")},
                    {"name":"active","type":named("Boolean")},
                    {"name":"ids","type":required(list(required(named("ID"))))},
                    {"name":"optionalId","type":required(named("ID")),"defaultValue":"\"default\""}
                ]}
            ]},
            "Mutation":{"kind":"OBJECT","fields":[{"name":"updateUser","type":named("User"),"isObject":true,"args":[{"name":"input","type":required(named("Filter"))}]}]},
            "Subscription":{"kind":"OBJECT","fields":[{"name":"userChanged","type":named("User"),"isObject":true,"args":[{"name":"id","type":named("ID")}]}]},
            "User":{"kind":"OBJECT","fields":[{"name":"id","type":named("ID")},{"name":"name","type":named("String")},{"name":"address","type":named("Address"),"isObject":true}]},
            "Address":{"kind":"OBJECT","fields":[{"name":"city","type":named("String")}]},
            "Filter":{"kind":"INPUT_OBJECT","inputFields":[
                {"name":"state","type":named("State")},
                {"name":"tags","type":list(required(named("String")))},
                {"name":"nested","type":named("Nested")}
            ]},
            "Nested":{"kind":"INPUT_OBJECT","inputFields":[{"name":"active","type":required(named("Boolean"))}]},
            "State":{"kind":"ENUM","enumValues":["OPEN","CLOSED"]},
            "ID":{"kind":"SCALAR"},"String":{"kind":"SCALAR"},"Boolean":{"kind":"SCALAR"}
        }})
    }
    fn complete(source: &str) -> Vec<String> {
        at(source, "query", &schema())
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect()
    }
    fn at(source: &str, field: &str, schema: &Value) -> completion::Completion {
        let lines: Vec<_> = source.split('\n').collect();
        completion::suggest(
            "graphql",
            field,
            source,
            lines.len() - 1,
            lines.last().unwrap().encode_utf16().count(),
            &[],
            schema,
        )
    }
    fn marked_at(source: &str, field: &str, schema: &Value) -> completion::Completion {
        let (before, after) = source.split_once('|').unwrap();
        let lines: Vec<_> = before.split('\n').collect();
        completion::suggest(
            "graphql",
            field,
            &format!("{before}{after}"),
            lines.len() - 1,
            lines.last().unwrap().encode_utf16().count(),
            &[],
            schema,
        )
    }
    fn explore_schema() -> Value {
        let mut schema = countries_schema();
        schema["types"]["Query"]["fields"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "name":"countries","type":list(required(named("Country"))),
                "args":[{"name":"filter","type":named("CountryFilterInput")}]
            }));
        schema["types"]["Country"]["fields"]
            .as_array_mut()
            .unwrap()
            .extend([
                json!({"name":"continent","type":required(named("Continent"))}),
                json!({"name":"languages","type":required(list(required(named("Language"))))}),
            ]);
        schema["types"]["Continent"] = json!({"kind":"OBJECT","fields":[{"name":"code","type":"ID"},{"name":"name","type":"String"}]});
        schema["types"]["Language"] = json!({"kind":"OBJECT","fields":[{"name":"code","type":"ID"},{"name":"name","type":"String"},{"name":"native","type":"String"}]});
        schema["types"]["CountryFilterInput"] = json!({"kind":"INPUT_OBJECT","inputFields":[{"name":"code","type":"StringQueryOperatorInput"},{"name":"continent","type":"StringQueryOperatorInput"}]});
        schema["types"]["StringQueryOperatorInput"] = json!({"kind":"INPUT_OBJECT","inputFields":[{"name":"eq","type":"String"},{"name":"in","type":list(named("String"))}]});
        schema
    }
    #[test]
    fn explore_reference_retains_all_scopes_object_colors_and_fragment_choices() {
        let reference: Value =
            serde_json::from_str(include_str!("../tests/fixtures/graphql-explore.json")).unwrap();
        let source = reference["query"].as_str().unwrap();
        let schema = explore_schema();
        assert_eq!(
            object_names(source, &schema, false),
            [
                "continent",
                "languages",
                "country",
                "country",
                "countries",
                "languages"
            ]
        );
        for (needle, prefix, expected) in [
            ("  continent {", "  cont", "continent"),
            ("    languages {", "    lang", "languages"),
            ("    ...CountryDetails", "    ...Cou", "CountryDetails"),
            (
                "      ...LanguageDetails",
                "      ...Lang",
                "LanguageDetails",
            ),
            (
                "  $filter: CountryFilterInput = { continent: { eq:",
                "  $filter: CountryFilterInput = { continent: { e",
                "eq",
            ),
        ] {
            let at = source.find(needle).unwrap() + prefix.len();
            let marked = format!("{}|{}", &source[..at], &source[at..]);
            assert!(
                marked_at(&marked, "query", &schema)
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text == expected),
                "{needle}"
            );
        }
        let console = format!(
            "#endpoint\nhttps://countries.fixture.invalid/graphql\n#variables\n{}\n#query\n{source}",
            reference["variables"]
        );
        assert_eq!(
            object_names(&console, &schema, true),
            object_names(source, &schema, false)
        );
        let marked = console.replacen("$hideCurrency)", "$|hideCurrency)", 1);
        let marked = format!(
            "{marked}\n#check other\n#query\nquery Other($outside: Boolean!) {{ country(code: \"PL\") {{ code }} }}"
        );
        let names: Vec<_> = marked_at(&marked, "", &schema)
            .candidates
            .into_iter()
            .map(|candidate| candidate.text)
            .collect();
        assert!(
            names.contains(&"$hideCurrency".into()) && names.contains(&"$withLanguages".into())
        );
        assert!(!names.contains(&"$outside".into()) && !names.contains(&"$primaryCode".into()));
    }
    #[test]
    fn directives_are_advertised_only_at_valid_attachment_sites() {
        for source in [
            "{ viewer ",
            "{ viewer { name ",
            "{ user(id: \"1\", ids: []) ",
            "{ viewer { ... on User ",
            "fragment U on User { name }\nquery { viewer { ...U ",
        ] {
            let result = at(source, "query", &schema());
            for name in ["@include()", "@skip()"] {
                assert!(
                    result
                        .candidates
                        .iter()
                        .any(|candidate| candidate.text == name),
                    "{source}: {name}"
                );
            }
        }
        for source in [
            "{ ",
            "{ viewer { ",
            "{ viewer { rename: ",
            "{ viewer { name } ",
            "query Q ",
            "fragment U on User ",
            "{ user ",
            "{ viewer @include ",
        ] {
            assert!(
                !at(source, "query", &schema())
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text.starts_with('@')),
                "{source}"
            );
        }
        let result = at("{ viewer { name @include(if: true) ", "query", &schema());
        assert!(result.automatic && result.automatic_directives_only);
        let directives: Vec<_> = result
            .candidates
            .iter()
            .filter(|candidate| candidate.text.starts_with('@'))
            .map(|candidate| candidate.text.as_str())
            .collect();
        assert_eq!(directives, ["@skip()"]);
        assert!(
            at("{ viewer { name ", "query", &schema())
                .candidates
                .iter()
                .any(|candidate| candidate.text == "address"),
            "Manual completion must retain sibling fields while automatic menus show only directives"
        );
        for candidate in ["@include()", "@skip()", "include()", "skip()"] {
            assert_eq!(
                completion::prepare_insertion("graphql", candidate, false),
                (candidate.into(), Some(candidate.len() - 1))
            );
        }
    }
    #[test]
    fn directive_names_arguments_values_and_tail_syntax_are_contextual() {
        for source in [
            "{ viewer { name @",
            "{ viewer { ... on User @",
            "{ viewer { ... @",
            "fragment U on User { name }\nquery { viewer { ...U @",
        ] {
            assert_eq!(complete(source), ["include()", "skip()"], "{source}");
        }
        for source in [
            "query Q @",
            "fragment U on User @",
            "query Q($yes: Boolean @",
        ] {
            assert!(complete(source).is_empty(), "{source}");
        }
        assert_eq!(complete("{ viewer { name @inc"), ["include()"]);
        assert_eq!(complete("{ viewer { name @include "), ["()"]);
        assert_eq!(complete("{ viewer { name @include("), ["if: "]);
        assert_eq!(complete("{ viewer { name @include(if: "), ["true", "false"]);
        let names = complete(
            "query Q($yes: Boolean!, $default: Boolean = true, $nullable: Boolean, $id: ID!) { viewer { name @include(if: $",
        );
        assert!(names.contains(&"$yes".into()) && names.contains(&"$default".into()));
        assert!(!names.contains(&"$nullable".into()) && !names.contains(&"$id".into()));
        assert_eq!(complete("{ viewer { name @include(if: true) @"), ["skip()"]);
        assert!(complete("{ viewer { name @include(if: true) @skip(if: false) @").is_empty());
        assert_eq!(
            complete("{ viewer @include(if: true) "),
            ["{\n  \n}", "@skip()"]
        );
        assert!(
            complete("{ viewer { ... on User @include(if: true) { na").contains(&"name".into())
        );
        assert!(complete("{ viewer { ... @skip(if: false) { na").contains(&"name".into()));
        assert!(
            complete("fragment U on User { name }\nquery { viewer { ...U @include(if: true) na")
                .contains(&"name".into())
        );
        assert!(
            !marked_at(
                "{ viewer @inc|lude(if: true) { name } }",
                "query",
                &schema()
            )
            .candidates
            .iter()
            .any(|candidate| candidate.text == "()")
        );
    }
    #[test]
    fn standalone_dollar_is_not_a_completion_but_variable_names_remain_available() {
        for source in [
            "query Q(",
            "query Q($id: ID!, ",
            "{ user(id: ",
            "{ user(filter: { tags: [",
            "{ viewer { name @skip(if: ",
            "query Q($yes: Boolean!, $id: ID!) { viewer { name @skip(if: ",
        ] {
            assert!(!complete(source).iter().any(|text| text == "$"), "{source}");
        }
        let source = "query Q($yes: Boolean!, $id: ID!) { viewer { name @skip(if: ";
        assert_eq!(complete(source), ["$yes", "true", "false"]);
        assert_eq!(complete(&format!("{source}$")), ["$yes"]);
        assert_eq!(complete(&format!("{source}$ye")), ["$yes"]);
    }
    #[test]
    fn value_variable_prefix_uses_request_names_only_without_declarations() {
        let mut schema = schema();
        schema["requestVariables"] =
            json!({"hideCurrency": false, "primaryCode": "PL", "bad-name": true});
        let names = |source: &str| {
            at(source, "query", &schema)
                .candidates
                .into_iter()
                .map(|candidate| candidate.text)
                .collect::<Vec<_>>()
        };
        let value = "fragment U on User { name @skip(if: ";
        assert_eq!(names(value), ["true", "false"]);
        assert_eq!(
            names(&format!("{value}$")),
            ["$hideCurrency", "$primaryCode"]
        );
        assert_eq!(names(&format!("{value}$hide")), ["$hideCurrency"]);
        assert_eq!(
            names("query Q($id: ID!) { viewer { name @skip(if: $"),
            Vec::<String>::new()
        );
        assert_eq!(names("query Q($flag: Boolean! = "), ["true", "false"]);
        assert!(names("query Q($flag: Boolean! = $").is_empty());
        let marked = "fragment U on User { name @skip(if: $|) }\nquery Q($hideCurrency: Boolean!, $id: ID!) { viewer { ...U } }";
        assert_eq!(
            marked_at(marked, "query", &schema)
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            ["$hideCurrency"]
        );
        let console = "#check first\n#variables\n{\"hideCurrency\":false}\n#query\nfragment U on User { name @skip(if: $|) }\n#check other\n#variables\n{\"outside\":true}\n#query\nquery Other($outside: Boolean!) { viewer { name } }";
        assert_eq!(
            marked_at(console, "", &schema)
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            ["$hideCurrency"]
        );
    }
    #[test]
    fn leading_fragments_use_later_operation_declarations_without_guessing_variable_types() {
        let reference: Value =
            serde_json::from_str(include_str!("../tests/fixtures/graphql-explore.json")).unwrap();
        let source = reference["query"].as_str().unwrap();
        let marked = source.replacen("$hideCurrency)", "$|hideCurrency)", 1);
        let result = marked_at(&marked, "query", &explore_schema());
        let names: Vec<_> = result
            .candidates
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect();
        assert_eq!(names, ["$withLanguages", "$hideCurrency"]);
        let multiple = format!(
            "{marked}\nquery Other($withLanguages: Boolean!) {{ country(code: \"PL\") {{ ...CountrySummary }} }}"
        );
        assert_eq!(
            marked_at(&multiple, "query", &explore_schema())
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            ["$withLanguages"]
        );
    }
    #[test]
    fn schema_directives_respect_locations_repeatability_and_argument_types() {
        let mut schema = schema();
        let mut directives = super::directive_definitions(&Value::Null).to_vec();
        directives.extend([
            json!({"name":"trace","locations":["FIELD"],"isRepeatable":true,
                "args":[{"name":"label","type":required(named("String"))}]}),
            json!({"name":"deprecated","locations":["FIELD_DEFINITION"],"args":[]}),
            json!({"name":"operation","locations":["QUERY"],"args":[]}),
            json!({"name":"fragmentOnly","locations":["FRAGMENT_DEFINITION"],"args":[]}),
        ]);
        schema["directives"] = json!(directives);
        let names = |source| {
            at(source, "query", &schema)
                .candidates
                .into_iter()
                .map(|candidate| candidate.text)
                .collect::<Vec<_>>()
        };
        assert_eq!(names("{ viewer @"), ["include()", "skip()", "trace()"]);
        assert_eq!(names("query Q @"), ["operation"]);
        assert_eq!(names("fragment U on User @"), ["fragmentOnly"]);
        assert_eq!(names("{ viewer @trace("), ["label: "]);
        assert_eq!(names("{ viewer @trace(label: "), ["\"\""]);
        assert!(names("{ viewer @trace(label: \"one\") @").contains(&"trace()".into()));
        assert!(names("query Q @operation { viewer { na").contains(&"name".into()));
        assert!(names("fragment U on User @fragmentOnly { na").contains(&"name".into()));
    }
    #[test]
    fn operation_hints_are_single_per_document_but_fragments_and_requests_are_repeatable() {
        for operation in [
            "query Q { viewer { name } }",
            "mutation M { updateUser(input: {}) { name } }",
            "subscription S { userChanged { id } }",
            "{ viewer { name } }",
        ] {
            assert!(
                at(&format!("{operation}\n"), "query", &schema())
                    .candidates
                    .is_empty(),
                "{operation}"
            );
            let marked = format!("fragment U on User {{ name }}\n|\n{operation}");
            assert_eq!(
                marked_at(&marked, "query", &schema())
                    .candidates
                    .iter()
                    .map(|candidate| candidate.text.as_str())
                    .collect::<Vec<_>>(),
                ["fragment"]
            );
            let marked = format!("fragment U on User {{ name }}\nmu|\n{operation}");
            assert!(
                marked_at(&marked, "query", &schema()).candidates.is_empty(),
                "{operation}"
            );
            let console = format!("#check first\n#query\n{operation}\n#check second\n#query\n");
            assert!(
                at(&console, "", &schema())
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text == "query")
            );
        }
        let source = "# query Phantom { }\nfragment query on User { name }\nfragment mutation on Query { user(id: \"subscription { mutation }\") { name } }\n";
        let result = at(source, "query", &schema());
        for name in ["query", "mutation", "subscription", "fragment", "{\n  \n}"] {
            assert!(
                result
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text == name)
            );
        }
        assert!(
            marked_at("que|ry Q { viewer { name } }", "query", &schema())
                .candidates
                .iter()
                .any(|candidate| candidate.text == "query")
        );
        assert_eq!(
            marked_at("|\nquery Q($id:", "query", &schema())
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            ["fragment"]
        );
    }
    #[test]
    fn document_starts_with_operations_even_without_discovery() {
        for (source, field) in [
            ("", "query"),
            (
                "#check g\n#endpoint\nhttps://fixture.invalid/graphql\n#query\n",
                "",
            ),
        ] {
            let result = at(source, field, &Value::Null);
            let items: Vec<_> = result.candidates.iter().map(|c| c.text.as_str()).collect();
            assert_eq!(&items[..3], ["query", "mutation", "subscription"]);
            assert_eq!(items[3], "fragment");
            assert_eq!(items[4], "{\n  \n}");
        }
        assert_eq!(complete("qu"), vec!["query"]);
        assert!(complete("query").is_empty());
        assert!(
            complete("query ")
                .iter()
                .all(|text| text.starts_with(['(', '{']))
        );
        assert!(
            complete("query GetUser ")
                .iter()
                .all(|text| text.starts_with(['(', '{']))
        );
        assert!(complete("query GetUser").is_empty());
    }
    fn object_names(source: &str, schema: &Value, console: bool) -> Vec<String> {
        let lines: Vec<_> = source.split('\n').collect();
        object_fields(source, schema, console)
            .into_iter()
            .map(|(line, from, to)| {
                String::from_utf16(
                    &lines[line]
                        .encode_utf16()
                        .skip(from)
                        .take(to - from)
                        .collect::<Vec<_>>(),
                )
                .unwrap()
            })
            .collect()
    }
    #[test]
    fn object_field_ranges_follow_schema_wrappers_aliases_and_fragments() {
        let mut schema = fragment_schema();
        schema["types"]["Query"]["fields"][0]["type"] = required(list(required(named("Product"))));
        schema["types"]["Product"]["fields"][2]["type"] = list(required(named("Image")));
        let source = "query ($images: Int) { p: products(skus: [\"images\"]) { sku i: images(limit: $images) { url } thumbnail { url } __typename } }";
        assert_eq!(
            object_names(source, &schema, false),
            ["products", "images", "thumbnail"]
        );
        let source = "fragment ProductFields on Product { sku images { url } }\nquery { products { ...ProductFields ... on SimpleProductView { price { amount } } } }";
        assert_eq!(
            object_names(source, &schema, false),
            ["images", "products", "price"]
        );
        schema["types"]["Query"]["fields"]
            .as_array_mut()
            .unwrap()
            .extend([
                json!({"name":"tags","type":list(named("String"))}),
                json!({"name":"states","type":list(named("State"))}),
                json!({"name":"quantity","type":"TierCondition"}),
            ]);
        schema["types"]["State"] = json!({"kind":"ENUM","enumValues":["OPEN"]});
        assert_eq!(
            object_names("{ tags states quantity { __typename } }", &schema, false),
            ["quantity"]
        );
        assert!(object_fields(source, &Value::Null, false).is_empty());
    }
    #[test]
    fn object_field_ranges_survive_incomplete_tokens_and_use_utf16_columns() {
        let schema = product_schema();
        for source in [
            "{ products(skus: [\"unfinished",
            "{ products(skus: [\"escape\\",
            "{ products # comment at EOF",
            "{ products(skus: [\"\"\"unfinished\nblock",
        ] {
            assert_eq!(
                object_names(source, &schema, false),
                ["products"],
                "{source}"
            );
        }
        let source = "{ products(skus: [\"🦀\"]) { sku alias: images";
        assert_eq!(object_names(source, &schema, false), ["products", "images"]);
        let start = source[..source.rfind("images").unwrap()]
            .encode_utf16()
            .count();
        assert_eq!(
            object_fields(source, &schema, false).last(),
            Some(&(0, start, start + 6))
        );
    }
    #[test]
    fn console_object_fields_exclude_variables_headers_and_response_sections() {
        let source = "#check first\n#variables\n{\"images\":{},\"products\":{}}\n#query\nfragment Fields on Product { images { url } }\nquery { products { sku ...Fields } }\n#headers\nimages: products\n#response OK\n{ products { images } }\n#check second\n#query\n{ products { # GraphQL comment\n images\n} }";
        assert_eq!(
            object_names(source, &product_schema(), true),
            ["images", "products", "products", "images"]
        );
        assert_eq!(
            object_fields(source, &product_schema(), true)
                .iter()
                .map(|range| range.0)
                .collect::<Vec<_>>(),
            [4, 5, 12, 13]
        );
    }
    #[test]
    fn operation_roots_and_anonymous_queries_use_the_right_fields() {
        assert!(complete("{ ").contains(&"viewer".into()));
        assert!(complete("query Get { ").iter().any(|text| text == "user"));
        let mutation = complete("mutation Update { ");
        assert!(mutation.iter().any(|text| text == "updateUser"));
        assert!(!mutation.iter().any(|text| text.starts_with("viewer")));
        let subscription = complete("subscription Watch { ");
        assert!(subscription.iter().any(|text| text == "userChanged"));
        assert!(
            !subscription
                .iter()
                .any(|text| text.starts_with("updateUser"))
        );
    }
    fn countries_schema() -> Value {
        let mut schema = schema();
        schema["types"]["Query"]["fields"].as_array_mut().unwrap().push(json!({
            "name":"country","type":"Country","args":[{"name":"code","type":required(named("ID"))}]
        }));
        schema["types"]["Country"] = json!({"kind":"OBJECT","fields":[
            {"name":"code","type":required(named("ID"))},
            {"name":"name","type":required(named("String"))},
            {"name":"capital","type":"String"},{"name":"currency","type":"String"}
        ]});
        schema["requestVariables"] = json!({"first":"PL","second":"DE"});
        schema
    }
    #[test]
    fn named_fragment_definitions_precede_operation_suggestions() {
        let schema = countries_schema();
        let names = |source: &str| {
            at(source, "query", &schema)
                .candidates
                .into_iter()
                .map(|c| c.text)
                .collect::<Vec<_>>()
        };
        assert_eq!(names("frag"), ["fragment"]);
        assert!(names("fragment ").is_empty());
        assert_eq!(names("fragment CountryFields "), ["on "]);
        assert_eq!(names("fragment CountryFields o"), ["on "]);
        let types = names("fragment CountryFields on ");
        assert!(types.contains(&"Country".into()) && types.contains(&"User".into()));
        assert!(!types.contains(&"Filter".into()) && !types.contains(&"ID".into()));
        assert_eq!(names("fragment CountryFields on Coun"), ["Country"]);
        assert_eq!(names("fragment CountryFields on Country "), ["{\n  \n}"]);
        assert_eq!(
            names("fragment CountryFields on Country {\n  cap"),
            ["capital"]
        );
        assert!(names("fragment CountryFields on ID ").is_empty());
        assert!(names("fragment on ").is_empty());
        let prefix = "fragment CountryFields on Country { code name capital currency }\n";
        assert!(names(prefix).contains(&"fragment".into()));
        assert!(names(prefix).contains(&"query".into()));
        assert_eq!(
            names(&format!("{prefix}fragment CountryFields ")),
            Vec::<String>::new()
        );
        assert!(
            !names(&format!(
                "{prefix}query {{ country(code: \"PL\") {{ ...CountryFields }} }}\n"
            ))
            .contains(&"fragment".into())
        );
        let vars = names(&format!("{prefix}query CompareCountries($"));
        assert!(vars.contains(&"$first: ".into()) && vars.contains(&"$second: ".into()));
    }
    #[test]
    fn field_suggestions_include_complete_compatible_fragment_spreads() {
        let schema = countries_schema();
        let definitions = "fragment UserFields on User { id name }\nfragment CountryFields on Country { code name capital currency }\n";
        let query = "query { firstCountry: country(code: \"PL\") { ";
        let source = format!("{definitions}{query}");
        let result = at(&source, "query", &schema);
        assert!(result.candidates.iter().any(|candidate| {
            candidate.text == "...CountryFields" && candidate.detail == "Fragment on Country"
        }));
        for name in ["code", "name"] {
            assert!(
                result
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text == name)
            );
        }
        assert!(
            !result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "... on ")
        );
        assert!(
            !result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "...UserFields")
        );
        for prefix in ["Cou", ".", ".."] {
            assert!(
                at(&format!("{source}{prefix}"), "query", &schema)
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text == "...CountryFields"),
                "{prefix}"
            );
        }
        assert!(
            !at(&format!("{source}...CountryFields\n  "), "query", &schema)
                .candidates
                .iter()
                .any(|candidate| candidate.text == "...CountryFields")
        );
        assert!(
            at(
                &format!("{source}...CountryFields }} secondCountry: country(code: \"DE\") {{ "),
                "query",
                &schema
            )
            .candidates
            .iter()
            .any(|candidate| candidate.text == "...CountryFields")
        );
        let console = format!("#check countries\n#query\n{source}");
        assert!(
            at(&console, "", &schema)
                .candidates
                .iter()
                .any(|candidate| candidate.text == "...CountryFields")
        );
        let other =
            format!("{console}}} }}\n#check other\n#query\nquery {{ country(code: \"DE\") {{ ");
        assert!(
            !at(&other, "", &schema)
                .candidates
                .iter()
                .any(|candidate| candidate.text == "...CountryFields")
        );
    }
    #[test]
    fn direct_fragment_spreads_replace_whole_prefixes_and_respect_existing_syntax() {
        let schema = countries_schema();
        let definitions = "fragment CountryFields on Country { code }\n";
        for selection in ["|", ".|", "..|", "..|.", "Cou|ntryFields"] {
            let source =
                format!("{definitions}query {{ country(code: \"PL\") {{ {selection} }} }}");
            let (before, after) = source.split_once('|').unwrap();
            let lines: Vec<_> = before.split('\n').collect();
            let original = format!("{before}{after}");
            let result = completion::suggest(
                "graphql",
                "query",
                &original,
                lines.len() - 1,
                lines.last().unwrap().encode_utf16().count(),
                &[],
                &schema,
            );
            let candidate = result
                .candidates
                .iter()
                .find(|candidate| candidate.text == "...CountryFields")
                .unwrap();
            let mut edited = lines.last().unwrap().to_string() + after;
            edited.replace_range(result.from..result.to, &candidate.text);
            assert_eq!(
                edited, "query { country(code: \"PL\") { ...CountryFields } }",
                "{selection}"
            );
        }
        for selection in [
            "alias: |",
            "Cou|ntryFields { code }",
            "Cou|ntryFields(code: \"PL\")",
            "Cou|ntryFields: code",
        ] {
            let source =
                format!("{definitions}query {{ country(code: \"PL\") {{ {selection} }} }}");
            let (before, after) = source.split_once('|').unwrap();
            let lines: Vec<_> = before.split('\n').collect();
            let original = format!("{before}{after}");
            let result = completion::suggest(
                "graphql",
                "query",
                &original,
                lines.len() - 1,
                lines.last().unwrap().encode_utf16().count(),
                &[],
                &schema,
            );
            assert!(
                !result
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text == "...CountryFields"),
                "{selection}"
            );
        }
    }
    #[test]
    fn direct_fragment_spreads_preserve_dependency_and_subscription_guards() {
        let schema = countries_schema();
        for source in [
            "fragment A on Country { ",
            "fragment A on Country { ...B }\nfragment B on Country { ",
            "fragment A on Country { ...B }\nfragment B on Country { ...A }\nquery { country(code: \"PL\") { ",
            "fragment A on Country { ...Missing }\nquery { country(code: \"PL\") { ",
        ] {
            assert!(
                !at(source, "query", &schema)
                    .candidates
                    .iter()
                    .any(|candidate| matches!(candidate.text.as_str(), "...A" | "...B")),
                "{source}"
            );
        }
        let source = "fragment Meta on Subscription { kind: __typename }\nfragment Wrapped on Subscription { ... on Subscription { ...Meta } }\nfragment Safe on Subscription { userChanged { __typename } }\nsubscription { ";
        let result = at(source, "query", &schema);
        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "...Safe")
        );
        assert!(
            !result
                .candidates
                .iter()
                .any(|candidate| matches!(candidate.text.as_str(), "...Meta" | "...Wrapped"))
        );
        let source = "fragment Inline on Product { sku }\nfragment Simple on SimpleProductView { price { amount } }\nquery { products { ";
        let result = at(source, "query", &fragment_schema());
        for name in ["...Inline", "...Simple"] {
            assert!(
                result
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text == name)
            );
        }
    }
    #[test]
    fn spreads_offer_preceding_compatible_fragments_and_can_be_reused_in_aliases() {
        let schema = countries_schema();
        let definitions = "fragment UserFields on User { id name }\nfragment CountryFields on Country { code name capital currency }\n";
        let query = "query CompareCountries($first: ID!, $second: ID!) { firstCountry: country(code: $first) { ...";
        let source = format!("{definitions}{query}");
        let result = at(&source, "query", &schema);
        assert!(
            result
                .candidates
                .iter()
                .any(|c| c.text == "CountryFields" && c.detail == "Fragment on Country")
        );
        assert!(!result.candidates.iter().any(|c| c.text == "UserFields"));
        assert!(!result.candidates.iter().any(|c| c.text == " on "));
        assert_eq!(
            at(&format!("{source}Cou"), "query", &schema).candidates[0].text,
            "CountryFields"
        );
        assert!(!at(&format!("{source}CountryFields"), "query", &schema).automatic);
        let used = at(&format!("{source}CountryFields ..."), "query", &schema);
        assert!(!used.candidates.iter().any(|c| c.text == "CountryFields"));
        let reused = at(
            &format!("{source}CountryFields }} secondCountry: country(code: $second) {{ ..."),
            "query",
            &schema,
        );
        assert!(reused.candidates.iter().any(|c| c.text == "CountryFields"));
        let future = format!("{source}|\n}} }}\nfragment Future on Country {{ code }}");
        let (before, after) = future.split_once('|').unwrap();
        let full = format!("{before}{after}");
        let lines: Vec<_> = before.split('\n').collect();
        let result = completion::suggest(
            "graphql",
            "query",
            &full,
            lines.len() - 1,
            lines.last().unwrap().encode_utf16().count(),
            &[],
            &schema,
        );
        assert!(result.candidates.iter().any(|c| c.text == "CountryFields"));
        assert!(!result.candidates.iter().any(|c| c.text == "Future"));
        let console = format!(
            "#check countries\n#variables\n{{\"first\":\"PL\",\"second\":\"DE\"}}\n#query\n{definitions}{query}"
        );
        assert!(
            at(&console, "", &schema)
                .candidates
                .iter()
                .any(|c| c.text == "CountryFields")
        );
        let other = format!(
            "{console}CountryFields }} }}\n#check other\n#query\nquery {{ country(code: \"PL\") {{ ..."
        );
        assert!(
            !at(&other, "", &schema)
                .candidates
                .iter()
                .any(|c| c.text == "CountryFields")
        );
    }
    #[test]
    fn named_fragment_graphs_exclude_cycles_and_respect_subscription_typename() {
        let schema = countries_schema();
        for source in [
            "fragment A on Country { ...",
            "fragment A on Country { ...B }\nfragment B on Country { ...",
            "fragment A on Country { ...B }\nfragment B on Country { ...A }\nquery { country(code: \"PL\") { ...",
            "fragment A on Country { ...Missing }\nquery { country(code: \"PL\") { ...",
        ] {
            let result = at(source, "query", &schema);
            assert!(
                !result
                    .candidates
                    .iter()
                    .any(|c| c.text == "A" || c.text == "B"),
                "{source}"
            );
        }
        let forward = "fragment A on Country { ...B }\nfragment B on Country { code }\nquery { country(code: \"PL\") { ...";
        assert!(
            at(forward, "query", &schema)
                .candidates
                .iter()
                .any(|c| c.text == "A")
        );
        let source = "fragment Meta on Subscription { kind: __typename }\nfragment Wrapped on Subscription { ... on Subscription { ...Meta } }\nfragment Safe on Subscription { userChanged { __typename } }\nsubscription { ...";
        let result = at(source, "query", &schema);
        assert!(result.candidates.iter().any(|c| c.text == "Safe"));
        assert!(
            !result
                .candidates
                .iter()
                .any(|c| c.text == "Meta" || c.text == "Wrapped")
        );
        let source = "fragment Shared on Country { code }\nfragment A on Country { ...Shared }\nfragment B on Country { ...Shared }\nfragment Both on Country { ...A ...B }\nquery { country(code: \"PL\") { ...";
        assert!(
            at(source, "query", &schema)
                .candidates
                .iter()
                .any(|c| c.text == "Both")
        );
        let source = "fragment Inline on Product { sku }\nfragment Simple on SimpleProductView { price { amount } }\nquery { products { ...";
        let result = at(source, "query", &fragment_schema());
        assert!(result.candidates.iter().any(|c| c.text == "Inline"));
        assert!(result.candidates.iter().any(|c| c.text == "Simple"));
    }
    #[test]
    fn named_fragment_replacement_preserves_spread_suffixes_and_existing_syntax() {
        let schema = countries_schema();
        for (source, expected) in [
            ("fragment CountryFields on Cou|ntry { code }", "Country"),
            (
                "fragment CountryFields on Country { code }\nquery { country(code: \"PL\") { ...Cou|ntryFields } }",
                "CountryFields",
            ),
        ] {
            let (before, after) = source.split_once('|').unwrap();
            let original = format!("{before}{after}");
            let lines: Vec<_> = before.split('\n').collect();
            let result = completion::suggest(
                "graphql",
                "query",
                &original,
                lines.len() - 1,
                lines.last().unwrap().encode_utf16().count(),
                &[],
                &schema,
            );
            let candidate = result
                .candidates
                .iter()
                .find(|c| c.text == expected)
                .unwrap();
            let mut edited = lines.last().unwrap().to_string() + after.split('\n').next().unwrap();
            let line = edited.clone();
            edited.replace_range(result.from..result.to, &candidate.text);
            assert_eq!(edited, line, "{source}");
        }
        for source in [
            "fragment CountryFields |on Country { code }",
            "fragment CountryFields on Country |{ code }",
        ] {
            let (before, after) = source.split_once('|').unwrap();
            let original = format!("{before}{after}");
            assert!(
                completion::suggest("graphql", "query", &original, 0, before.len(), &[], &schema)
                    .candidates
                    .is_empty(),
                "{source}"
            );
        }
    }
    #[test]
    fn variable_definitions_offer_only_input_types_and_correct_wrappers() {
        let types = complete("query Get($id: ");
        for ty in ["ID", "Boolean", "Filter", "State", "["] {
            assert!(types.contains(&ty.into()), "{ty}");
        }
        for ty in ["User", "Query", "Mutation", "Subscription", "viewer"] {
            assert!(!types.contains(&ty.into()), "{ty}");
        }
        assert!(!types.contains(&"Int".into()) && !types.contains(&"Float".into()));
        assert_eq!(complete("query Get($id: ID!) "), vec!["{\n  \n}"]);
        assert!(complete("query Get($id: [ID ").contains(&"]".into()));
        assert!(!complete("query Get($id: [ID! ").contains(&"!".into()));
        assert!(complete("query Get($id: [ID!]! ").contains(&"=".into()));
        assert!(!complete("query Get($id: [ID!]! ").contains(&"!".into()));
        assert_eq!(complete("query Get($"), Vec::<String>::new());
        let tail = complete("query Get($id: ID = null ");
        assert!(!tail.contains(&"=".into()) && !tail.contains(&"!".into()));
    }
    #[test]
    fn alias_fields_automatically_open_after_colon_space_but_not_after_completed_scalars() {
        for source in [
            "query { viewer { displayName: ",
            "fragment Summary on User { displayName: ",
            "query { viewer { name otherName: ",
        ] {
            let result = at(source, "query", &schema());
            assert!(result.automatic, "{source}");
            assert!(
                result
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text == "name"),
                "{source}"
            );
            assert!(
                !result
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text.starts_with("...")),
                "{source}"
            );
            let console = format!("#check aliases\n#query\n{source}");
            assert!(at(&console, "", &schema()).automatic, "{source}");
        }
        for source in [
            "query { viewer { displayName:",
            "query { viewer { displayName: name",
        ] {
            assert!(!at(source, "query", &schema()).automatic, "{source}");
        }
        for source in [
            "query { viewer { displayName: name ",
            "fragment Summary on User { displayName: name ",
        ] {
            let result = at(source, "query", &schema());
            assert!(
                result.automatic && result.automatic_directives_only,
                "{source}"
            );
        }
        for source in ["fragment Summary on ", "fragment Summary on C"] {
            let result = at(source, "query", &countries_schema());
            assert!(
                result.automatic
                    && result
                        .candidates
                        .iter()
                        .any(|candidate| candidate.text == "Country")
            );
        }
    }
    #[test]
    fn field_arguments_are_isolated_and_used_names_are_hidden() {
        let args = complete("{ user(");
        assert!(args.contains(&"id: ".into()) && args.contains(&"filter: ".into()));
        assert!(!args.contains(&")".into()) && !args.contains(&"name".into()));
        let partial = complete("{ user(i");
        assert!(partial.contains(&"id: ".into()) && partial.contains(&"ids: ".into()));
        assert!(!partial.contains(&"name".into()));
        let used = complete("{ user(id: \"x\", ");
        assert!(!used.contains(&"id: ".into()));
        assert!(used.contains(&"filter: ".into()));
        assert!(!used.contains(&")".into()), "ids is still required");
        assert!(complete("{ user(id: \"x\", ids: [] ").contains(&")".into()));
        assert_eq!(
            complete("{ user ").len(),
            1,
            "required arguments come before a selection set"
        );
    }
    #[test]
    fn variables_are_scoped_and_filtered_by_input_type_and_nullability() {
        let header =
            "query Get($id: ID!, $text: String, $other: ID, $ids: [ID!]!, $fallback: ID = \"x\")";
        let vars = complete(&format!("{header} {{ user(id: $"));
        assert_eq!(vars, vec!["$id", "$fallback"]);
        assert_eq!(complete(&format!("{header} {{ user(ids: $")), vec!["$ids"]);
        assert_eq!(
            complete(&format!("{header} {{ user(optionalId: $")),
            vec!["$id", "$other", "$fallback"]
        );
        assert!(
            !complete("query Get($id: ID = $")
                .iter()
                .any(|text| text.starts_with('$'))
        );
        assert!(
            !complete("query Old($id: ID!) { viewer { id } }\nquery New { user(id: $")
                .contains(&"$id".into())
        );
    }
    #[test]
    fn input_objects_enums_booleans_and_lists_follow_argument_types() {
        assert_eq!(complete("{ user(id: "), vec!["\"\""]);
        assert_eq!(complete("{ user(state: "), vec!["null", "OPEN", "CLOSED"]);
        assert_eq!(complete("{ user(active: "), vec!["null", "true", "false"]);
        let input = complete("{ user(filter: { ");
        assert!(input.contains(&"state: ".into()) && input.contains(&"tags: ".into()));
        assert!(!input.contains(&"viewer".into()) && !input.contains(&"name".into()));
        assert_eq!(complete("{ user(filter: { state: O"), vec!["OPEN"]);
        assert_eq!(
            complete("{ user(filter: { nested: { active: "),
            vec!["true", "false"]
        );
        let list = complete("{ user(filter: { tags: [");
        assert!(list.contains(&"\"\"".into()) && list.contains(&"]".into()));
        assert!(!list.contains(&"null".into()), "list elements are non-null");
        let next = complete("{ user(filter: { tags: [\"one\", ");
        assert!(next.contains(&"\"\"".into()) && next.contains(&"]".into()));
    }
    #[test]
    fn aliases_arguments_and_string_delimiters_preserve_selection_scope() {
        for source in [
            "{ person: user(id: \"{ ) # false\", ids: []) { na",
            "query Get($id: ID!) { user(id: $id, ids: []) { na",
            "{ user(id: \"\"\"{ ) # multiline\n string }\"\"\", ids: []) { na",
        ] {
            assert_eq!(complete(source), vec!["name"], "{source}");
        }
        assert_eq!(complete("{ viewer { id na"), vec!["name"]);
        let alias = complete("{ viewer { primary: name ");
        assert!(
            alias.contains(&"name".into()),
            "an alias does not reserve the unaliased response name"
        );
        assert_eq!(
            complete("{ viewer { name ")
                .iter()
                .filter(|text| *text == "name")
                .count(),
            0
        );
        assert!(complete("{ user(id: \"unfinished").is_empty());
        assert!(complete("{ viewer { # name").is_empty());
        assert_eq!(complete("{ viewer { # name\n na"), vec!["name"]);
        assert_eq!(complete("\u{feff}{ viewer { # name\r na"), vec!["name"]);
    }
    #[test]
    fn snippets_keep_the_cursor_inside_arguments_and_inputs() {
        let candidate = complete("{ user ")
            .into_iter()
            .find(|text| text == "()")
            .unwrap();
        let (text, cursor) = completion::prepare_insertion("graphql", &candidate, false);
        assert_eq!(&text[..cursor.unwrap()], "(");
        let source = "{ user(filter: ";
        let candidate = complete(source)
            .into_iter()
            .find(|text| text.starts_with('{'))
            .unwrap();
        let (text, cursor) = completion::prepare_insertion("graphql", &candidate, false);
        assert_eq!(&text[..cursor.unwrap()], "{\n  ");
    }
    fn product_schema() -> Value {
        json!({"queryType":"Query","types":{
            "Query":{"fields":[{"name":"products","type":"Product","args":[
                {"name":"skus","type":list(named("String"))}
            ]},{"name":"lookup","type":"Product","args":[
                {"name":"id","type":required(named("ID"))}
            ]},{"name":"total","type":"Int","args":[{"name":"precision","type":named("Int")}]}]},
            "Product":{"fields":[{"name":"sku","type":"String"},{"name":"skuLabel","type":"String"},
                {"name":"images","type":"Image","args":[{"name":"limit","type":named("Int")}]},
                {"name":"thumbnail","type":"Image"}]},
            "Image":{"fields":[{"name":"url","type":"String"}]},
            "String":{"kind":"SCALAR"},"Int":{"kind":"SCALAR"}
        }})
    }

    fn cursor(source: &str) -> completion::Completion {
        let (before, after) = source.split_once('|').unwrap();
        let source = format!("{before}{after}");
        completion::suggest(
            "graphql",
            "query",
            &source,
            before.split('\n').count() - 1,
            before
                .split('\n')
                .next_back()
                .unwrap()
                .encode_utf16()
                .count(),
            &[],
            &product_schema(),
        )
    }

    #[test]
    fn fields_offer_arguments_or_selection_after_inserting_the_name() {
        for (candidate, cursor) in [("()", 1), (" ()", 2), ("products()", 9)] {
            let (text, target) = completion::prepare_insertion("graphql", candidate, false);
            assert_eq!(text, candidate);
            assert_eq!(target, Some(cursor));
            assert!(!text.contains('\n'));
        }
        assert_eq!(
            completion::prepare_insertion("mysql", "NOW()", false),
            ("NOW()".into(), None)
        );
        assert_eq!(
            completion::prepare_insertion("graphql", "https://fixture.invalid/query()", false),
            ("https://fixture.invalid/query()".into(), None)
        );
        let schema = product_schema();
        let names = |source: &str| {
            at(source, "query", &schema)
                .candidates
                .into_iter()
                .map(|c| c.text)
                .collect::<Vec<_>>()
        };
        assert_eq!(names("query "), vec!["{\n  \n}", "()"]);
        assert_eq!(names("query ($skus: [String])"), vec![" {\n  \n}"]);
        let products = names("query ($skus: [String]) {\n  pro");
        assert_eq!(products, vec!["products"]);
        assert_eq!(
            names("query ($skus: [String]) {\n  products "),
            vec!["{\n    \n  }", "()", "@include()", "@skip()"]
        );
        assert_eq!(names("{ look"), vec!["lookup"]);
        assert_eq!(names("{ lookup "), vec!["()"]);
        assert_eq!(
            names("query ($skus: [String]) { products(skus: $s"),
            vec!["$skus"]
        );
        assert_eq!(
            names("query ($skus: [String]) {\n  products(skus: $skus)"),
            vec![" {\n    \n  }", "@include()", "@skip()"]
        );
        let images = names("query ($skus: [String]) {\n  products(skus: $skus) {\n    im");
        assert_eq!(images, vec!["images"]);
        assert_eq!(
            names("query ($skus: [String]) {\n  products(skus: $skus) {\n    images "),
            vec!["{\n      \n    }", "()", "@include()", "@skip()"]
        );
        assert_eq!(names("{ products { th"), vec!["thumbnail"]);
        assert_eq!(
            names("{ products { thumbnail "),
            vec!["{\n  \n}", "@include()", "@skip()"]
        );
        assert!(images.iter().all(|text| !text.contains("(\n")));
        let nested = names(
            "query ($skus: [String]) {\n  products(skus: $skus) {\n    sku\n    images {\n      ",
        );
        assert_eq!(nested, vec!["url", "__typename"]);
    }
    #[test]
    fn automatic_field_hints_wait_for_space_or_newline() {
        for source in [
            "query { products|",
            "query { products { images|",
            "query { products { sku|",
            "query { products { sku } }|",
        ] {
            assert!(!cursor(source).automatic, "{source}");
        }
        let scalar = cursor("query { products { sku |");
        assert!(scalar.automatic && scalar.automatic_directives_only);
        for source in [
            "query |",
            "query { products |",
            "query { products { images |",
            "query { products { sku\n  |",
            "query { products { sk|",
        ] {
            assert!(cursor(source).automatic, "{source}");
        }
        let next = cursor("query { products { sku\n  |");
        assert!(!next.candidates.iter().any(|c| c.text == "sku"));
        assert!(next.candidates.iter().any(|c| c.text == "images"));
        let scalar_args = cursor("query { total |");
        assert_eq!(
            scalar_args
                .candidates
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            ["()", "@include()", "@skip()"]
        );
    }
    #[test]
    fn existing_arguments_and_selections_are_never_duplicated() {
        for source in [
            "query { products |(skus: []) { sku } }",
            "query { products | # existing args\n (skus: []) { sku } }",
            "query |($skus: [String]) { products(skus: $skus) { sku } }",
            "query ($skus: [String]) |{ products(skus: $skus) { sku } }",
            "query { total |(precision: 1) }",
            "query { products(skus: |[]) { sku } }",
        ] {
            assert!(cursor(source).candidates.is_empty(), "{source}");
        }
        for source in [
            "query { products(skus: []) |{ sku } }",
            "query { products(skus: []) | # existing body\n { sku } }",
        ] {
            assert_eq!(
                cursor(source)
                    .candidates
                    .iter()
                    .map(|candidate| candidate.text.as_str())
                    .collect::<Vec<_>>(),
                ["@include()", "@skip()"]
            );
        }
        for source in [
            "query ($skus: [String]) { products(skus: $skus) |",
            "# 😀\n\nquery { products(skus: []) |",
        ] {
            let result = cursor(source);
            assert_eq!(result.candidates.len(), 3, "{source}");
            assert_eq!(result.candidates[0].detail, "Selection set", "{source}");
        }
        for source in ["query |{ products { sku } }", "query { products |{ sku } }"] {
            let result = cursor(source);
            assert_eq!(
                result
                    .candidates
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>(),
                if source.starts_with("query |") {
                    vec!["()"]
                } else {
                    vec!["()", "@include()", "@skip()"]
                }
            );
        }
    }
    #[test]
    fn replacing_partial_names_preserves_existing_suffixes_and_colons() {
        for (source, expected) in [
            ("query { pro|ducts(skus: []) { sku } }", "products"),
            ("query { products(sk|us: []) { sku } }", "skus"),
            (
                "query ($skus: [String]) { products(skus: $sk|us) { sku } }",
                "$skus",
            ),
            ("que|ry { products { sku } }", "query"),
        ] {
            let result = cursor(source);
            let candidate = result
                .candidates
                .iter()
                .find(|c| c.text == expected)
                .unwrap();
            let original = source.replace('|', "");
            let mut edited = original.clone();
            edited.replace_range(result.from..result.to, &candidate.text);
            assert_eq!(edited, original, "{source}");
        }
    }
    #[test]
    fn right_context_preserves_existing_types_and_delimiters() {
        for source in [
            "query ($skus: | [String]) { products { sku } }",
            "query ($skus: | String) { products { sku } }",
        ] {
            assert!(cursor(source).candidates.is_empty(), "{source}");
        }
        for (source, duplicate) in [
            ("query ($skus: String |!) { products { sku } }", "!"),
            ("query ($skus: String | ) { products { sku } }", ")"),
            ("query ($skus: [String | ]) { products { sku } }", "]"),
            ("query { products(skus: [] | ) { sku } }", ")"),
            ("query { products(skus: [\"one\" | ]) { sku } }", "]"),
        ] {
            assert!(
                !cursor(source)
                    .candidates
                    .iter()
                    .any(|c| c.text == duplicate),
                "{source}"
            );
        }
        assert!(
            cursor("query ($skus: String |) { products { sku } }")
                .candidates
                .iter()
                .any(|c| c.text == ")")
        );
    }
    #[test]
    fn typename_is_an_implicit_scalar_in_selection_sets() {
        for source in [
            "query {\n  __ty",
            "mutation {\n  __ty",
            "{ viewer {\n  __ty",
            "subscription { userChanged {\n  __ty",
            "#check g\n#query\n{ viewer {\n  __ty",
        ] {
            let result = at(
                source,
                if source.starts_with('#') { "" } else { "query" },
                &schema(),
            );
            assert_eq!(result.candidates.len(), 1, "{source}");
            assert_eq!(result.candidates[0].text, "__typename", "{source}");
            assert_eq!(result.candidates[0].detail, "String!", "{source}");
            assert_eq!(
                completion::prepare_insertion("graphql", "__typename", false),
                ("__typename".into(), None)
            );
        }
        for source in [
            "subscription { __ty",
            "{ viewer { __typename\n  __ty",
            "query __ty",
            "query ($value: __ty",
            "{ user(__ty",
            "{ user(filter: { __ty",
        ] {
            assert!(!complete(source).contains(&"__typename".into()), "{source}");
        }
        assert_eq!(
            complete("{ viewer { kind: __typename\n  __ty"),
            ["__typename"]
        );
        assert!(!at("{ viewer { __typename", "query", &schema()).automatic);
        assert!(at("{ viewer { __typename ", "query", &schema()).automatic_directives_only);
        assert!(at("{ viewer { __typename\n  ", "query", &schema()).automatic);
    }

    fn fragment_schema() -> Value {
        let mut schema = product_schema();
        schema["types"]["Product"]["kind"] = json!("INTERFACE");
        schema["types"]["Product"]["possibleTypes"] =
            json!(["SimpleProductView", "ComplexProductView"]);
        for (name, ty) in [
            (
                "SimpleProductView",
                json!({"kind":"OBJECT","fields":[
                    {"name":"sku","type":"String"}, {"name":"price","type":"Price"}]}),
            ),
            (
                "ComplexProductView",
                json!({"kind":"OBJECT","fields":[
                    {"name":"sku","type":"String"}, {"name":"options","type":"Option"}]}),
            ),
            (
                "Priced",
                json!({"kind":"INTERFACE","possibleTypes":["SimpleProductView","Unrelated"],
                    "fields":[{"name":"price","type":"Price"}]}),
            ),
            (
                "Price",
                json!({"kind":"OBJECT","fields":[
                    {"name":"amount","type":"Int"}, {"name":"quantity","type":"TierCondition"}]}),
            ),
            (
                "TierCondition",
                json!({"kind":"UNION","possibleTypes":["ProductViewTierRangeCondition","FixedQuantity"]}),
            ),
            (
                "ProductViewTierRangeCondition",
                json!({"kind":"OBJECT","fields":[
                    {"name":"lt","type":"Int"}, {"name":"gte","type":"Int"}]}),
            ),
            (
                "FixedQuantity",
                json!({"kind":"OBJECT","fields":[{"name":"value","type":"Int"}]}),
            ),
            (
                "Option",
                json!({"kind":"OBJECT","fields":[{"name":"values","type":"OptionValue"}]}),
            ),
            (
                "OptionValue",
                json!({"kind":"INTERFACE","possibleTypes":["ProductViewOptionValueProduct"],
                    "fields":[{"name":"title","type":"String"}]}),
            ),
            (
                "ProductViewOptionValueProduct",
                json!({"kind":"OBJECT","fields":[
                    {"name":"product","type":"SimpleProductView"}, {"name":"quantity","type":"Int"}]}),
            ),
            (
                "Unrelated",
                json!({"kind":"OBJECT","fields":[{"name":"secret","type":"String"}]}),
            ),
        ] {
            schema["types"][name] = ty;
        }
        schema
    }
    #[test]
    fn type_condition_hints_require_another_compatible_composite_type() {
        let mut abstract_without_alternatives = fragment_schema();
        abstract_without_alternatives["types"]["Product"]["possibleTypes"] = json!([]);
        for (schema, body, available) in [
            (json!({"queryType":"Missing","types":{}}), "{ ", false),
            (countries_schema(), "{ country(code: \"PL\") { ", false),
            (product_schema(), "{ products { images { ", false),
            (abstract_without_alternatives, "{ products { ", false),
            (fragment_schema(), "{ products { ", true),
            (
                fragment_schema(),
                "{ products { ... on SimpleProductView { ",
                true,
            ),
            (
                fragment_schema(),
                "{ products { ... on SimpleProductView { price { quantity { ",
                true,
            ),
        ] {
            for (field, prefix) in [("query", ""), ("", "#check types\n#query\n")] {
                for tail in ["", ".", "..", "... ", "... o"] {
                    let result = at(&format!("{prefix}{body}{tail}"), field, &schema);
                    assert_eq!(
                        result
                            .candidates
                            .iter()
                            .any(|candidate| matches!(candidate.text.trim(), "... on" | "on")),
                        available,
                        "{prefix}{body}{tail}: {:?}",
                        result.candidates
                    );
                }
            }
        }
        let schema = countries_schema();
        assert_eq!(
            at("{ country(code: \"PL\") { ... on ", "query", &schema)
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            ["Country"]
        );
        assert_eq!(
            at("fragment New ", "query", &schema)
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            ["on "]
        );
    }
    #[test]
    fn inline_fragment_steps_offer_only_compatible_composite_types() {
        let schema = fragment_schema();
        let names = |source| {
            let mut names = at(source, "query", &schema)
                .candidates
                .into_iter()
                .map(|c| c.text)
                .collect::<Vec<_>>();
            names.sort();
            names
        };
        for source in ["{ products { .", "{ products { .."] {
            assert_eq!(names(source), ["... on "], "{source}");
        }
        assert_eq!(names("{ products { ..."), [" on ", "@include()", "@skip()"]);
        for source in ["{ products { ... ", "{ products { ... o"] {
            let expected = if source.ends_with('o') {
                vec!["on "]
            } else {
                vec!["@include()", "@skip()", "on "]
            };
            assert_eq!(names(source), expected, "{source}");
        }
        assert_eq!(
            names("{ products { ... on "),
            [
                "ComplexProductView",
                "Priced",
                "Product",
                "SimpleProductView"
            ]
        );
        assert_eq!(names("{ products { ... on Sim"), ["SimpleProductView"]);
        assert!(!at("{ products { ... on SimpleProductView", "query", &schema).automatic);
        assert_eq!(
            names("{ products { ... on SimpleProductView "),
            ["@include()", "@skip()", "{\n  \n}"]
        );
        assert_eq!(
            names("{ products { ... on SimpleProductView { pr"),
            ["price"]
        );
        assert!(!names("{ products { ... on SimpleProductView {\n  ").contains(&"options".into()));
        assert_eq!(
            names("{ products { ... on SimpleProductView { price { quantity { ... on "),
            [
                "FixedQuantity",
                "ProductViewTierRangeCondition",
                "TierCondition"
            ]
        );
        assert_eq!(
            names(
                "{ products { ... on SimpleProductView { price { quantity { ... on ProductViewTierRangeCondition { g"
            ),
            ["gte"]
        );
        assert_eq!(
            names("{ products { ... on ComplexProductView { options { values { ... on "),
            ["OptionValue", "ProductViewOptionValueProduct"]
        );
        assert_eq!(
            names(
                "{ products { ... on ComplexProductView { options { values { ... on ProductViewOptionValueProduct { product { pr"
            ),
            ["price"]
        );
        assert_eq!(
            names(
                "{ products { ... on SimpleProductView { price { amount } } ... on ComplexProductView { op"
            ),
            ["options"]
        );
        assert_eq!(
            names("{ products { ... on SimpleProductView { price { amount } }\n im"),
            ["images"]
        );
        assert_eq!(names("{ products { ... { sk"), ["sku", "skuLabel"]);
        assert_eq!(names("{ products { ...Shared\n im"), ["images"]);
    }
    #[test]
    fn typename_and_fragment_conditions_obey_selection_scope() {
        let schema = fragment_schema();
        assert_eq!(
            at("{ products { ", "query", &schema)
                .candidates
                .iter()
                .find(|candidate| candidate.text == "... on ")
                .unwrap()
                .detail,
            "Type condition"
        );
        for source in [
            "{ products { __ty",
            "{ products { ... on SimpleProductView { __ty",
            "{ products { ... on SimpleProductView { price { quantity { __ty",
            "{ products { ... on SimpleProductView { price { quantity { ... on ProductViewTierRangeCondition { __ty",
        ] {
            assert_eq!(
                at(source, "query", &schema)
                    .candidates
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>(),
                ["__typename"],
                "{source}"
            );
        }
        assert!(complete("subscription { ... on Subscription {\n __ty").is_empty());
        assert_eq!(
            complete("subscription { ... on Subscription { userChanged { __ty"),
            ["__typename"]
        );
        let prefix = "#check fragment\n#query\n";
        let source = format!("{prefix}{{ products {{ ... on Sim");
        assert_eq!(
            at(&source, "", &schema).candidates[0].text,
            "SimpleProductView"
        );
        for source in ["query ($value: ... on ", "{ products(skus: ... on "] {
            assert!(
                at(source, "query", &schema).candidates.is_empty(),
                "{source}"
            );
        }
    }
    #[test]
    fn fragment_completion_preserves_existing_spreads_and_selection_sets() {
        let schema = fragment_schema();
        for (source, candidate) in [
            (
                "{ products { .|.. on SimpleProductView { sku } }",
                "... on ",
            ),
            (
                "{ products { ..|. on SimpleProductView { sku } }",
                "... on ",
            ),
            (
                "{ products { ... on Sim|pleProductView { sku } }",
                "SimpleProductView",
            ),
        ] {
            let (before, after) = source.split_once('|').unwrap();
            let original = format!("{before}{after}");
            let result =
                completion::suggest("graphql", "query", &original, 0, before.len(), &[], &schema);
            let text = &result
                .candidates
                .iter()
                .find(|c| c.text == candidate)
                .unwrap()
                .text;
            let mut edited = original.clone();
            edited.replace_range(result.from..result.to, text);
            assert_eq!(edited, original, "{source}");
        }
        let source = "{ products { ... on SimpleProductView  { sku } }";
        let result = completion::suggest(
            "graphql",
            "query",
            source,
            0,
            "{ products { ... on SimpleProductView ".len(),
            &[],
            &schema,
        );
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            ["@include()", "@skip()"]
        );
    }
    #[test]
    fn declaration_names_come_from_request_json_without_exposing_values() {
        let prefix = "#variables\n{\"skus\":[\"private-fixture\"],\"locale\":\"en\",\"nested\":{\"hidden\":1},\"bad-key\":1,\"1bad\":2}\n#query\n";
        for fragment in ["query (", "query ($", "query ($s"] {
            let source = format!("{prefix}{fragment}");
            let result = at(&source, "", &Value::Null);
            assert!(result.candidates.iter().any(|c| c.text == "$skus: "));
            assert!(
                result
                    .candidates
                    .iter()
                    .all(|c| !c.text.contains("private-fixture")
                        && !c.detail.contains("private-fixture")
                        && !c.text.contains("hidden")
                        && !c.text.contains("bad"))
            );
            if fragment.ends_with("$s") {
                assert_eq!(result.from, "query (".len());
                assert_eq!(result.candidates[0].text, "$skus: ");
            }
        }
        let used = at(
            &format!("{prefix}query ($skus: [String], $"),
            "",
            &Value::Null,
        );
        assert!(!used.candidates.iter().any(|c| c.text == "$skus: "));
        assert!(used.candidates.iter().any(|c| c.text == "$locale: "));
        let next = at(
            &format!("{prefix}query ($skus: [String] "),
            "",
            &Value::Null,
        );
        assert!(next.candidates.iter().any(|c| c.text == "$locale: "));
        let mut schema = schema();
        schema["requestVariables"] = json!({"skus":["fixture"]});
        assert_eq!(
            at("query ($s", "query", &schema).candidates[0].text,
            "$skus: "
        );
        schema["requestVariables"] = json!("{\"formOnly\":true}");
        assert_eq!(
            at("query ($f", "query", &schema).candidates[0].text,
            "$formOnly: "
        );
        for bad in [json!("unfinished {"), json!([1, 2]), Value::Null] {
            schema["requestVariables"] = bad;
            assert!(at("query ($", "query", &schema).candidates.is_empty());
        }
        schema["requestVariables"] = json!({"skus":["fixture"]});
        let source = "query ($skus: [String]) { products(skus: $skus) { sku } }";
        let result = completion::suggest(
            "graphql",
            "query",
            source,
            0,
            "query ($s".len(),
            &[],
            &schema,
        );
        assert_eq!(result.from, "query (".len());
        assert_eq!(result.to, "query ($skus".len());
        assert_eq!(result.candidates[0].text, "$skus");
        let replaced = format!(
            "{}{}{}",
            &source[..result.from],
            result.candidates[0].text,
            &source[result.to..]
        );
        assert_eq!(
            replaced, source,
            "editing an existing declaration must not duplicate its name or colon"
        );
    }
    #[test]
    fn console_variable_names_are_scoped_to_the_current_request() {
        for first in ["#check first\n", ""] {
            let prefix = format!("{first}#variables\n{{\"firstOnly\":1}}\n#query\nquery ($");
            let source = format!(
                "{prefix}\n#check second\n#variables\n{{\"secondOnly\":2}}\n#query\nquery ($"
            );
            let line = prefix.split('\n').count() - 1;
            let first = completion::suggest(
                "graphql",
                "",
                &source,
                line,
                "query ($".len(),
                &[],
                &Value::Null,
            );
            assert_eq!(
                first
                    .candidates
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>(),
                ["$firstOnly: "]
            );
            let second = at(&source, "", &Value::Null);
            assert_eq!(
                second
                    .candidates
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>(),
                ["$secondOnly: "]
            );
        }
        let source = "#check after\n#query\nquery ($\n#variables\n{\"after\":true}";
        let result = completion::suggest(
            "graphql",
            "",
            source,
            2,
            "query ($".len(),
            &[],
            &Value::Null,
        );
        assert_eq!(result.candidates[0].text, "$after: ");
    }
    #[test]
    fn replacement_offsets_are_utf16_and_do_not_eat_variable_dollars() {
        let source = "query Get($id: ID!) { user(id: \"😀\", optionalId: $i";
        let result = at(source, "query", &schema());
        assert_eq!(
            result.from,
            source[..source.rfind("$i").unwrap()].encode_utf16().count()
        );
        assert_eq!(result.to, source.encode_utf16().count());
        assert_eq!(result.candidates[0].text, "$id");
    }
}
