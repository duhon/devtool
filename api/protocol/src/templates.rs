//! Templates are named functions: `@name(param: domain = "default") description` headers
//! followed by a body. See `expand` for the call semantics.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type Library = BTreeMap<String, BTreeMap<String, String>>;

pub const MAX_DEPTH: usize = 16;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Domain {
    /// The full `{{…}}` text, evaluated by the kernel in list context.
    Reference(String),
    Inline(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    #[serde(default)]
    pub domain: Option<Domain>,
    #[serde(default)]
    pub default: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Template {
    pub name: String,
    #[serde(default)]
    pub params: Vec<Param>,
    pub description: String,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arg {
    Literal(String),
    Param(String),
    Variable { absolute: bool, path: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub name: String,
    pub args: Vec<Arg>,
}

fn is_name_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.')
}

fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '-')
}

fn is_path_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '*' | '~')
}

fn is_ident(text: &str) -> bool {
    !text.is_empty() && text.chars().all(is_ident_char)
}

/// Splits on `sep` outside of quoted literals and `{{ }}` references.
fn split_top(text: &str, sep: char) -> Vec<String> {
    let mut parts = vec![String::new()];
    let (mut quoted, mut escaped, mut depth) = (false, false, 0usize);
    for ch in text.chars() {
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
        } else if ch == '"' {
            quoted = true;
        } else if ch == '{' {
            depth += 1;
        } else if ch == '}' {
            depth = depth.saturating_sub(1);
        } else if ch == sep && depth == 0 {
            parts.push(String::new());
            continue;
        }
        parts.last_mut().unwrap().push(ch);
    }
    parts
}

/// Parses a whole `"…"` literal with `\"` and `\\` escapes.
fn parse_quoted(text: &str) -> Result<String, String> {
    let text = text.trim();
    let inner = text
        .strip_prefix('"')
        .ok_or_else(|| format!("expected a quoted literal, got `{text}`"))?;
    let mut value = String::new();
    let mut chars = inner.chars();
    loop {
        match chars.next() {
            None => return Err(format!("unterminated literal {text}")),
            Some('"') => {
                return if chars.next().is_none() {
                    Ok(value)
                } else {
                    Err(format!("unexpected text after the literal in `{text}`"))
                };
            }
            Some('\\') => match chars.next() {
                Some(ch @ ('"' | '\\')) => value.push(ch),
                _ => return Err(format!("only \\\" and \\\\ escapes are allowed in {text}")),
            },
            Some(ch) => value.push(ch),
        }
    }
}

fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn parse_domain(text: &str) -> Result<Domain, String> {
    let text = text.trim();
    if let Some(tail) = text.strip_prefix("{{") {
        return if text.len() > 4 && closing(tail) == Some(text.len() - 4) {
            Ok(Domain::Reference(text.into()))
        } else {
            Err(format!(
                "a domain must be exactly one {{{{…}}}} reference, got `{text}`"
            ))
        };
    }
    if text.is_empty() {
        return Err("empty domain".into());
    }
    split_top(text, '|')
        .iter()
        .map(|item| parse_quoted(item))
        .collect::<Result<Vec<_>, _>>()
        .map(Domain::Inline)
        .map_err(|error| format!("a domain is a {{{{…}}}} reference or \"a\" | \"b\": {error}"))
}

fn parse_param(text: &str) -> Result<Param, String> {
    let text = text.trim();
    let end = text.find(|ch| !is_ident_char(ch)).unwrap_or(text.len());
    let name = &text[..end];
    if name.is_empty() {
        return Err(format!("expected a parameter name in `{text}`"));
    }
    let rest = text[end..].trim();
    let (domain_text, default_text) = if let Some(rest) = rest.strip_prefix(':') {
        let mut parts = split_top(rest, '=').into_iter();
        let domain = parts.next().unwrap_or_default();
        let default: Vec<_> = parts.collect();
        match default.len() {
            0 => (Some(domain), None),
            1 => (Some(domain), Some(default[0].clone())),
            _ => return Err(format!("parameter {name} has more than one default")),
        }
    } else if let Some(rest) = rest.strip_prefix('=') {
        (None, Some(rest.to_owned()))
    } else if rest.is_empty() {
        (None, None)
    } else {
        return Err(format!(
            "unexpected `{rest}` after parameter {name}; use name: domain = \"default\""
        ));
    };
    let domain = domain_text
        .map(|text| parse_domain(&text).map_err(|error| format!("parameter {name}: {error}")))
        .transpose()?;
    let default = default_text
        .map(|text| {
            parse_quoted(&text)
                .map_err(|_| format!("parameter {name}: the default must be a quoted literal"))
        })
        .transpose()?;
    Ok(Param {
        name: name.into(),
        domain,
        default,
    })
}

fn parse_params(text: &str) -> Result<Vec<Param>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut params: Vec<Param> = Vec::new();
    for piece in split_top(text, ',') {
        let param = parse_param(&piece)?;
        if params.iter().any(|known| known.name == param.name) {
            return Err(format!("duplicate parameter {}", param.name));
        }
        params.push(param);
    }
    Ok(params)
}

/// Index of the `)` closing the `(` at the start of `text`, outside literals and references.
fn closing_paren(text: &str) -> Option<usize> {
    let (mut quoted, mut escaped, mut depth) = (false, false, 0usize);
    for (index, ch) in text.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
        } else if ch == '"' {
            quoted = true;
        } else if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn parse_header(header: &str) -> Result<(String, Vec<Param>, String), String> {
    let end = header.find(|ch| !is_name_char(ch)).unwrap_or(header.len());
    let name = &header[..end];
    let rest = &header[end..];
    if name.is_empty()
        || name.chars().count() > 80
        || !(rest.is_empty() || rest.starts_with(char::is_whitespace) || rest.starts_with('('))
    {
        return Err("expected @name or @name(params) followed by an optional description. Use \\@ for a literal leading @ in a body.".into());
    }
    if rest.starts_with('(') {
        let close = closing_paren(rest)
            .ok_or_else(|| format!("missing `)` in the signature of @{name}"))?;
        let params = parse_params(&rest[1..close]).map_err(|error| format!("@{name}: {error}"))?;
        Ok((name.into(), params, rest[close + 1..].trim().into()))
    } else {
        Ok((name.into(), Vec::new(), rest.trim().into()))
    }
}

/// Whether a section text may be stored at all; its syntax may still be wrong.
pub fn check_limits(text: &str) -> Result<(), String> {
    if text.len() > 256 * 1024 || text.contains('\0') {
        Err("Templates must be at most 256 KiB per section and cannot contain NUL.".into())
    } else {
        Ok(())
    }
}

/// Every template of `text`, or the first error.
pub fn parse(text: &str) -> Result<Vec<Template>, String> {
    let (templates, errors) = parse_partial(text);
    match errors.into_iter().next() {
        Some(error) => Err(error),
        None => Ok(templates),
    }
}

/// The templates of `text` that parse, and the errors of the rest in line order: a
/// definition with an error is skipped with its body, a repeated name keeps the first one.
pub fn parse_partial(text: &str) -> (Vec<Template>, Vec<String>) {
    if let Err(error) = check_limits(text) {
        return (Vec::new(), vec![error]);
    }
    let mut result: Vec<(usize, Template)> = Vec::new();
    let mut errors: Vec<(usize, String)> = Vec::new();
    let mut names = BTreeSet::new();
    let (mut skipping, mut stray) = (false, false);
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        if let Some(header) = line.strip_prefix('@') {
            skipping = true;
            match parse_header(header) {
                Err(error) => errors.push((number, format!("Line {number}: {error}"))),
                Ok((name, _, _)) if names.contains(&name) => errors.push((
                    number,
                    format!("Line {number}: duplicate template @{name}."),
                )),
                Ok((name, params, description)) => {
                    names.insert(name.clone());
                    skipping = false;
                    let body = String::new();
                    result.push((
                        number,
                        Template {
                            name,
                            params,
                            description,
                            body,
                        },
                    ));
                }
            }
        } else if skipping {
            continue;
        } else if let Some((_, template)) = result.last_mut() {
            template.body.push_str(if line.starts_with("\\@") {
                &line[1..]
            } else {
                line
            });
            template.body.push('\n');
        } else if !line.trim().is_empty() && !stray {
            stray = true;
            errors.push((
                number,
                format!("Line {number}: start a template with @name description."),
            ));
        }
    }
    let mut templates = Vec::new();
    for (number, mut template) in result {
        let lines: Vec<_> = template.body.lines().collect();
        let start = lines.iter().position(|line| !line.trim().is_empty());
        let end = lines.iter().rposition(|line| !line.trim().is_empty());
        match (start, end) {
            (Some(start), Some(end)) => {
                template.body = lines[start..=end].join("\n");
                templates.push(template);
            }
            _ => errors.push((
                number,
                format!("Line {number}: template @{} has no content.", template.name),
            )),
        }
    }
    errors.sort_by_key(|(number, _)| *number);
    (
        templates,
        errors.into_iter().map(|(_, error)| error).collect(),
    )
}

fn parse_arg(text: &str) -> Result<Arg, String> {
    let text = text.trim();
    if text.starts_with('"') {
        return parse_quoted(text).map(Arg::Literal);
    }
    if let Some((absolute, path)) = text
        .strip_prefix(':')
        .map(|path| (false, path))
        .or_else(|| text.strip_prefix('.').map(|path| (true, path)))
    {
        return if !path.is_empty() && path.chars().all(is_path_char) {
            Ok(Arg::Variable {
                absolute,
                path: path.into(),
            })
        } else {
            Err(format!("invalid variable path in `{text}`"))
        };
    }
    if is_ident(text) {
        Ok(Arg::Param(text.into()))
    } else {
        Err(format!(
            "invalid argument `{text}`: use \"literal\", a parameter name, :path or .path"
        ))
    }
}

/// Parses `name(args)` or `name` (no parentheses = no arguments).
pub fn parse_call(text: &str) -> Result<Call, String> {
    let text = text.trim();
    let end = text.find(|ch| !is_name_char(ch)).unwrap_or(text.len());
    let name = &text[..end];
    let rest = &text[end..];
    if name.is_empty() {
        return Err(format!("expected a template name in `{text}`"));
    }
    if rest.is_empty() {
        return Ok(Call {
            name: name.into(),
            args: Vec::new(),
        });
    }
    let inner = rest
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .filter(|_| closing_paren(rest) == Some(rest.len() - 1))
        .ok_or_else(|| format!("expected @{name}(args), got `{text}`"))?;
    let args = if inner.trim().is_empty() {
        Vec::new()
    } else {
        split_top(inner, ',')
            .iter()
            .map(|arg| parse_arg(arg))
            .collect::<Result<_, _>>()
            .map_err(|error| format!("@{name}: {error}"))?
    };
    Ok(Call {
        name: name.into(),
        args,
    })
}

/// Parses the call on a section line: `@name(args)` or `@name`.
pub fn parse_section_call(text: &str) -> Result<Call, String> {
    let text = text.trim();
    parse_call(
        text.strip_prefix('@')
            .ok_or_else(|| format!("a call starts with @, got `{text}`"))?,
    )
}

fn render_arg(arg: &Arg) -> String {
    match arg {
        Arg::Literal(value) => quote(value),
        Arg::Param(name) => name.clone(),
        Arg::Variable { absolute, path } => {
            format!("{}{path}", if *absolute { '.' } else { ':' })
        }
    }
}

/// `name(args)` without the leading `@`; no parentheses when there are no arguments.
pub fn render_call(call: &Call) -> String {
    if call.args.is_empty() {
        return call.name.clone();
    }
    let args: Vec<_> = call.args.iter().map(render_arg).collect();
    format!("{}({})", call.name, args.join(", "))
}

/// The call as written on a section line (`@name(args)`).
pub fn render_section_call(call: &Call) -> String {
    format!("@{}", render_call(call))
}

/// Expands a call. `outer_params` are the values of the parameters visible to the
/// call's arguments (empty for a section-line call).
pub fn expand(
    library_view: &dyn Fn(&str) -> Option<Template>,
    call: &Call,
    outer_params: &BTreeMap<String, String>,
) -> Result<String, String> {
    expand_inner(library_view, call, outer_params, &mut Vec::new())
}

fn expand_inner(
    view: &dyn Fn(&str) -> Option<Template>,
    call: &Call,
    outer: &BTreeMap<String, String>,
    stack: &mut Vec<String>,
) -> Result<String, String> {
    if stack.contains(&call.name) {
        let mut chain = stack.clone();
        chain.push(call.name.clone());
        return Err(format!("Template cycle: {}", chain.join(" -> ")));
    }
    if stack.len() >= MAX_DEPTH {
        return Err(format!(
            "Template nesting is deeper than {MAX_DEPTH}: {} -> {}",
            stack.join(" -> "),
            call.name
        ));
    }
    let template = view(&call.name).ok_or_else(|| format!("Unknown template @{}", call.name))?;
    if call.args.len() > template.params.len() {
        return Err(format!(
            "@{} takes {} argument(s), got {}",
            template.name,
            template.params.len(),
            call.args.len()
        ));
    }
    let mut values = BTreeMap::new();
    for (index, param) in template.params.iter().enumerate() {
        let value = match call.args.get(index) {
            Some(Arg::Literal(value)) => value.clone(),
            Some(Arg::Param(name)) => outer.get(name).cloned().ok_or_else(|| {
                format!(
                    "@{}: `{name}` is not a parameter in scope (use \"{name}\" for text or :{name} for a variable)",
                    template.name
                )
            })?,
            Some(Arg::Variable { absolute, path }) => {
                format!("{{{{{}{path}}}}}", if *absolute { '.' } else { ':' })
            }
            None => param.default.clone().ok_or_else(|| {
                format!("@{}: missing argument `{}`", template.name, param.name)
            })?,
        };
        values.insert(param.name.clone(), value);
    }
    stack.push(call.name.clone());
    let output = substitute(view, &template.name, &template.body, &values, stack)?;
    stack.pop();
    Ok(output)
}

/// `text` of the template `name` with its parameters `{{param}}` replaced by `values`, also
/// inside other references (`{{.a.{{param}}}}`), and its calls `{{@other(...)}}` expanded.
fn substitute(
    view: &dyn Fn(&str) -> Option<Template>,
    name: &str,
    text: &str,
    values: &BTreeMap<String, String>,
    stack: &mut Vec<String>,
) -> Result<String, String> {
    let mut output = String::new();
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        if rest[..open].ends_with('\\') {
            output.push_str(&rest[..open + 2]);
            rest = &rest[open + 2..];
            continue;
        }
        let after = &rest[open + 2..];
        let call = after.trim_start().starts_with(['@', '$']);
        let close = if call {
            reference_end(after)
        } else {
            closing(after)
        };
        let Some(close) = close else {
            break;
        };
        output.push_str(&rest[..open]);
        let inner = after[..close].trim();
        if let Some(call_text) = inner.strip_prefix('@') {
            let nested = parse_call(call_text).map_err(|error| format!("@{name}: {error}"))?;
            output.push_str(&expand_inner(view, &nested, values, stack)?);
        } else if let Some(call_text) = inner.strip_prefix('$') {
            let bound = bind_function(call_text, &|param| {
                values.get(param).map(|value| as_argument(value))
            })
            .map_err(|error| match error {
                Ok(missing) => format!(
                    "@{name}: `{}` is not a parameter in scope (use \"{0}\" for text or :{0} for a variable)",
                    missing[0]
                ),
                Err(error) => format!("@{name}: {error}"),
            })?;
            output.push_str(&bound);
        } else if let Some(value) = values.get(inner) {
            output.push_str(value);
        } else {
            let inner = substitute(view, name, &after[..close], values, stack)?;
            output.push_str(&format!("{{{{{inner}}}}}"));
        }
        rest = &after[close + 2..];
    }
    output.push_str(rest);
    Ok(output)
}

/// A parameter's value as an argument of a function call: a whole variable reference stays
/// a variable, anything else is a literal.
fn as_argument(value: &str) -> Arg {
    value
        .strip_prefix("{{")
        .and_then(|inner| inner.strip_suffix("}}"))
        .and_then(|inner| match parse_arg(inner) {
            Ok(arg @ Arg::Variable { .. }) => Some(arg),
            _ => None,
        })
        .unwrap_or_else(|| Arg::Literal(value.to_owned()))
}

/// The function call `name(args)` (after `$`, or `!name(args)` for an eager one) as
/// `{{$name(args)}}` with each bare parameter replaced by `lookup` of it; the names `lookup`
/// has no value for, or a syntax error.
fn bind_function(
    call_text: &str,
    lookup: &dyn Fn(&str) -> Option<Arg>,
) -> Result<String, Result<Vec<String>, String>> {
    let (bang, call_text) = match call_text.strip_prefix('!') {
        Some(rest) => ("!", rest),
        None => ("", call_text),
    };
    let mut call = parse_call(call_text).map_err(Err)?;
    let mut missing = Vec::new();
    for arg in &mut call.args {
        if let Arg::Param(name) = arg {
            match lookup(name) {
                Some(value) => *arg = value,
                None => missing.push(name.clone()),
            }
        }
    }
    if missing.is_empty() {
        Ok(format!("{{{{${bang}{}}}}}", render_call(&call)))
    } else {
        Err(Ok(missing))
    }
}

/// The reference domain `text` with its parameters `{{param}}` (and those passed to a
/// function, `{{$name(param)}}`) replaced by `values`, or the names of those that have no
/// value yet.
pub fn fill_domain(text: &str, values: &BTreeMap<String, String>) -> Result<String, Vec<String>> {
    let (mut output, mut missing) = (String::new(), Vec::new());
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        output.push_str(&rest[..open]);
        let close = match closing(after) {
            Some(close) if !rest[..open].ends_with('\\') => close,
            _ => {
                output.push_str("{{");
                rest = after;
                continue;
            }
        };
        let inner = after[..close].trim();
        if let Some(call_text) = inner.strip_prefix('$') {
            let lookup = |name: &str| values.get(name).cloned().map(Arg::Literal);
            match bind_function(call_text, &lookup) {
                Ok(bound) => output.push_str(&bound),
                Err(Ok(names)) => missing.extend(names),
                Err(Err(_)) => output.push_str(&rest[open..open + 2 + close + 2]),
            }
        } else if is_ident(inner) {
            match values.get(inner) {
                Some(value) => output.push_str(value),
                None => missing.push(inner.to_owned()),
            }
        } else {
            match fill_domain(&after[..close], values) {
                Ok(inner) => output.push_str(&format!("{{{{{inner}}}}}")),
                Err(names) => missing.extend(names),
            }
        }
        rest = &after[close + 2..];
    }
    output.push_str(rest);
    if missing.is_empty() {
        Ok(output)
    } else {
        let mut seen = std::collections::BTreeSet::new();
        missing.retain(|name| seen.insert(name.clone()));
        Err(missing)
    }
}

/// The offset in `tail` (just after `{{`) of the `}}` that closes it, past nested `{{…}}`.
fn closing(tail: &str) -> Option<usize> {
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

/// Offset of the closing `}}` of a reference; a call's quoted literals may contain `}}`.
fn reference_end(text: &str) -> Option<usize> {
    let is_call = text.trim_start().starts_with(['@', '$']);
    let (mut quoted, mut escaped) = (false, false);
    let mut previous_brace = None;
    for (index, ch) in text.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        if ch == '"' && is_call {
            quoted = true;
        }
        if ch == '}' {
            if let Some(start) = previous_brace {
                return Some(start);
            }
            previous_brace = Some(index);
        } else {
            previous_brace = None;
        }
    }
    None
}

/// The templates a section line (`#section @name(...)`, `#check @`) offers: only those of
/// `service`/`section`.
pub fn own(library: &Library, service: &str, section: &str) -> Vec<Template> {
    let mut templates = library
        .get(service)
        .and_then(|entries| entries.get(section))
        .map(|text| parse_partial(text).0)
        .unwrap_or_default();
    templates.sort_by(|a, b| a.name.cmp(&b.name));
    templates
}

/// The templates a live call (`{{@name(...)}}`) in `service`/`section` may name: the global
/// ones (`*`/`*`) and those of the section, which win on a name clash. Texts that do not
/// parse are skipped.
pub fn visible(library: &Library, service: &str, section: &str) -> Vec<Template> {
    let mut merged: BTreeMap<String, Template> = BTreeMap::new();
    for (service, section) in [("*", "*"), (service, section)] {
        for template in own(library, service, section) {
            merged.insert(template.name.clone(), template);
        }
    }
    merged.into_values().collect()
}

/// Checks that every template text may be stored; a syntax error only skips its definition
/// (the editor shows it). Sections of services or sections no loaded module has are kept as
/// they are: a module may be removed, fail to load or come back in another version.
pub fn validate(library: &Library) -> Result<(), String> {
    for entries in library.values() {
        for text in entries.values() {
            check_limits(text)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_of(text: &str) -> impl Fn(&str) -> Option<Template> {
        let templates = parse(text).unwrap();
        move |name| templates.iter().find(|t| t.name == name).cloned()
    }

    fn run(text: &str, call: &str) -> Result<String, String> {
        expand(&view_of(text), &parse_section_call(call)?, &BTreeMap::new())
    }

    const SPEC: &str = "@host(service: {{:services}}) Host name\n{{service}}.{{:base_domain}}\n\n@url_shared(service: {{:services}}, type: {{.*.type}} = \"qa\") Service URL\nhttps://{{@host(service)}}.{{.var.region}}.{{type}}.example\n\n@order Zero-parameter template\n{ \"x\": 1 }";

    #[test]
    fn templates_of_unloaded_services_and_sections_are_kept_but_still_checked() {
        let library = BTreeMap::from([
            (
                "splunk".into(),
                BTreeMap::from([("from".into(), "@old Existing template\nvalue".into())]),
            ),
            (
                "redis".into(),
                BTreeMap::from([("check".into(), "@x\nGET\n@bad(\nx".into())]),
            ),
        ]);
        assert_eq!(validate(&library), Ok(()));
        let broken = BTreeMap::from([(
            "redis".into(),
            BTreeMap::from([("check".into(), "x\0".into())]),
        )]);
        assert!(validate(&broken).unwrap_err().contains("NUL"));
    }

    #[test]
    fn a_broken_definition_skips_only_itself() {
        let text = "stray\n@a\none\n@b(\ntwo\n@a\nagain\n@c\nthree\n@e\n";
        let (templates, errors) = parse_partial(text);
        let names: Vec<_> = templates.iter().map(|t| (&*t.name, &*t.body)).collect();
        assert_eq!(names, [("a", "one"), ("c", "three")]);
        let lines: Vec<_> = errors
            .iter()
            .map(|e| e.split(':').next().unwrap())
            .collect();
        assert_eq!(lines, ["Line 1", "Line 4", "Line 6", "Line 10"]);
        assert_eq!(parse(text).unwrap_err(), errors[0]);
    }

    #[test]
    fn parses_literal_multiline_bodies_without_headers_or_descriptions() {
        let entries = parse("@my_create_book comment here\n{\n  \"id\": 123\n}\n\n@другой second\n  SELECT 1;\n\n\\@counter\n\\\\@x\n").unwrap();
        assert_eq!(entries[0].name, "my_create_book");
        assert_eq!(entries[0].description, "comment here");
        assert!(entries[0].params.is_empty());
        assert_eq!(entries[0].body, "{\n  \"id\": 123\n}");
        assert_eq!(entries[1].body, "  SELECT 1;\n\n@counter\n\\\\@x");
    }

    #[test]
    fn rejects_invalid_definitions_but_does_not_validate_body_languages() {
        for text in [
            "@",
            "@a",
            "body\n@a\nx",
            "@a\none\n@a\ntwo",
            "@bad/name\nx",
            "@a\nx\n@@b",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
        assert!(parse("@a\n{\"trailing\":1,}\n").is_ok());
        assert!(parse("").unwrap().is_empty());
    }

    #[test]
    fn parses_signatures() {
        let templates = parse(SPEC).unwrap();
        assert_eq!(
            templates[1].params,
            vec![
                Param {
                    name: "service".into(),
                    domain: Some(Domain::Reference("{{:services}}".into())),
                    default: None
                },
                Param {
                    name: "type".into(),
                    domain: Some(Domain::Reference("{{.*.type}}".into())),
                    default: Some("qa".into())
                },
            ]
        );
        assert_eq!(templates[1].description, "Service URL");
        let inline = parse("@a(x: \"a\" | \"b,c\" = \"b,c\", y, z = \"q\\\"\") d\nbody").unwrap();
        assert_eq!(
            inline[0].params[0].domain,
            Some(Domain::Inline(vec!["a".into(), "b,c".into()]))
        );
        assert_eq!(inline[0].params[0].default.as_deref(), Some("b,c"));
        assert_eq!(inline[0].params[1].domain, None);
        assert_eq!(inline[0].params[2].default.as_deref(), Some("q\""));
        assert!(parse("@a() d\nx").unwrap()[0].params.is_empty());
    }

    #[test]
    fn reports_malformed_signatures_with_line_numbers() {
        for (text, needle) in [
            ("\n@a(x d\nbody", "Line 2"),
            ("@a(x, x) d\nb", "duplicate parameter x"),
            ("@a(x = qa) d\nb", "quoted literal"),
            ("@a(x: {{:a}} extra) d\nb", "Line 1"),
            ("@a(: {{:a}}) d\nb", "parameter name"),
            ("@a(x: foo) d\nb", "domain"),
            ("@a(x = \"1\" = \"2\") d\nb", "Line 1"),
        ] {
            let error = parse(text).unwrap_err();
            assert!(error.contains(needle), "{text}: {error}");
            assert!(error.starts_with("Line"), "{error}");
        }
    }

    #[test]
    fn parses_and_renders_calls() {
        let call = parse_section_call("@u(\"a\\\"b\\\\\", p, :x.y, .var.*.z, :e~)").unwrap();
        assert_eq!(
            call.args,
            vec![
                Arg::Literal("a\"b\\".into()),
                Arg::Param("p".into()),
                Arg::Variable {
                    absolute: false,
                    path: "x.y".into()
                },
                Arg::Variable {
                    absolute: true,
                    path: "var.*.z".into()
                },
                Arg::Variable {
                    absolute: false,
                    path: "e~".into()
                },
            ]
        );
        assert_eq!(
            parse_section_call(&render_section_call(&call)).unwrap(),
            call
        );
        assert_eq!(parse_section_call("@order").unwrap().args.len(), 0);
        assert_eq!(parse_section_call("@order()").unwrap().args.len(), 0);
        assert_eq!(render_call(&parse_call("order()").unwrap()), "order");
        assert!(parse_section_call("order").is_err());
        assert!(parse_section_call("@a(\"x\"").is_err());
        assert!(parse_section_call("@a(\"x\") tail").is_err());
        assert!(parse_section_call("@a(:)").is_err());
        assert!(parse_section_call("@a(\"x)").is_err());
    }

    #[test]
    fn expands_the_nested_example_keeping_variables_live() {
        assert_eq!(
            run(SPEC, "@url_shared(\"checkout\")").unwrap(),
            "https://checkout.{{:base_domain}}.{{.var.region}}.qa.example"
        );
        assert_eq!(
            run(SPEC, "@url_shared(:svc, \"stage\")").unwrap(),
            "https://{{:svc}}.{{:base_domain}}.{{.var.region}}.stage.example"
        );
        assert_eq!(run(SPEC, "@order").unwrap(), "{ \"x\": 1 }");
    }

    #[test]
    fn distinguishes_literal_parameter_and_variable_arguments() {
        let text = "@label(type) d\n[{{type}}]\n\n@outer(type) o\n{{@label(type)}}|{{@label(\"type\")}}|{{@label(:type)}}|{{@label(\"a}}b\")}}";
        assert_eq!(run(text, "@label(\"type\")").unwrap(), "[type]");
        assert_eq!(run(text, "@label(:type)").unwrap(), "[{{:type}}]");
        assert_eq!(
            run(text, "@outer(\"prod\")").unwrap(),
            "[prod]|[type]|[{{:type}}]|[a}}b]"
        );
        let outer = BTreeMap::from([("type".to_owned(), "prod".to_owned())]);
        let call = parse_call("label(type)").unwrap();
        assert_eq!(expand(&view_of(text), &call, &outer).unwrap(), "[prod]");
        let escaped = "@label(kind) d\n[\\{{kind}}] {{kind}}";
        assert_eq!(run(escaped, "@label(\"a\")").unwrap(), "[\\{{kind}}] a");
        let nested = "@item(group, name, n = \"1\") Item\n{{.tree.{{name}}.{{group}}.{{n}}}}";
        assert_eq!(
            run(nested, "@item(\"one\", \"alpha\")").unwrap(),
            "{{.tree.alpha.one.1}}"
        );
        let dependent = parse("@t(a: {{.tree~}}, b: {{.tree.{{a}}~}}) d\nx").unwrap();
        assert_eq!(
            dependent[0].params[1].domain,
            Some(Domain::Reference("{{.tree.{{a}}~}}".into()))
        );
        let values = BTreeMap::from([("a".to_owned(), "x".to_owned())]);
        assert_eq!(
            fill_domain("{{.t.{{a}}.{{b}}~}}", &values),
            Err(vec!["b".to_owned()])
        );
        assert_eq!(fill_domain("{{.t.{{a}}~}}", &values).unwrap(), "{{.t.x~}}");
    }

    #[test]
    fn parameters_passed_to_functions_are_bound() {
        let text = "@a(x, y) d\n{{$f(x, \"x\", y, :z)}} {{$g}}\n\n@b(x) d\n{{$f(q)}}";
        assert_eq!(
            run(text, "@a(\"o\\\"ne\", :w.v)").unwrap(),
            "{{$f(\"o\\\"ne\", \"x\", :w.v, :z)}} {{$g}}"
        );
        assert_eq!(
            run("@a(x) d\n{{$!f(x)}} {{$!g}}", "@a(\"one\")").unwrap(),
            "{{$!f(\"one\")}} {{$!g}}"
        );
        assert!(
            run(text, "@b(\"1\")")
                .unwrap_err()
                .contains("`q` is not a parameter in scope")
        );
        let values = BTreeMap::from([("a".to_owned(), "one".to_owned())]);
        assert_eq!(
            fill_domain("{{$f(a, \"b\")}}", &values).unwrap(),
            "{{$f(\"one\", \"b\")}}"
        );
        assert_eq!(
            fill_domain("{{$f(a, b)}}", &values),
            Err(vec!["b".to_owned()])
        );
        assert_eq!(
            parse("@t(a: {{$f(x)}}) d\nx").unwrap()[0].params[0].domain,
            Some(Domain::Reference("{{$f(x)}}".into()))
        );
    }

    #[test]
    fn only_declared_bare_parameters_are_substituted() {
        let text = "@a(x) d\n{{x}} {{ x }} {{y}} {{.x}} {{:x}} {{x.y}} {{unterminated";
        assert_eq!(
            run(text, "@a(\"1\")").unwrap(),
            "1 1 {{y}} {{.x}} {{:x}} {{x.y}} {{unterminated"
        );
    }

    #[test]
    fn reports_binding_errors() {
        let text = "@a(x, y = \"d\") d\n{{x}}{{y}}\n\n@b d\n{{@a(z)}}\n\n@c d\n{{@nope}}";
        assert_eq!(run(text, "@a(\"1\")").unwrap(), "1d");
        assert!(
            run(text, "@a")
                .unwrap_err()
                .contains("missing argument `x`")
        );
        assert!(
            run(text, "@a(\"1\", \"2\", \"3\")")
                .unwrap_err()
                .contains("takes 2")
        );
        assert!(
            run(text, "@b")
                .unwrap_err()
                .contains("not a parameter in scope")
        );
        assert_eq!(run(text, "@c").unwrap_err(), "Unknown template @nope");
    }

    #[test]
    fn detects_cycles_and_deep_nesting() {
        let cyclic = "@a d\n{{@b}}\n\n@b d\n{{@a}}";
        assert_eq!(
            run(cyclic, "@a").unwrap_err(),
            "Template cycle: a -> b -> a"
        );
        let mut chain = String::new();
        for index in 0..20 {
            chain.push_str(&format!("@t{index} d\n{{{{@t{}}}}}\n\n", index + 1));
        }
        chain.push_str("@t20 d\nend");
        assert!(run(&chain, "@t0").unwrap_err().contains("deeper than 16"));
        assert_eq!(run(&chain, "@t10").unwrap(), "end");
    }

    #[test]
    fn section_lines_see_their_section_and_live_calls_also_the_global_ones() {
        let library: Library = BTreeMap::from([
            (
                "*".into(),
                BTreeMap::from([("*".into(), "@n global\nGG\n\n@only_gg d\nx".into())]),
            ),
            (
                "http".into(),
                BTreeMap::from([
                    ("body".into(), "@n exact\nEE".into()),
                    ("url".into(), "@url_only d\nz".into()),
                ]),
            ),
        ]);
        let names = |templates: Vec<Template>| -> Vec<(String, String)> {
            templates.into_iter().map(|t| (t.name, t.body)).collect()
        };
        assert_eq!(
            names(visible(&library, "http", "body")),
            vec![("n".into(), "EE".into()), ("only_gg".into(), "x".into())]
        );
        assert_eq!(
            names(own(&library, "http", "body")),
            vec![("n".into(), "EE".into())]
        );
        assert!(own(&library, "http", "check").is_empty());
        assert_eq!(
            names(visible(&library, "redis", "check"))[0],
            ("n".into(), "GG".into())
        );
        assert_eq!(validate(&library), Ok(()));
    }
}
