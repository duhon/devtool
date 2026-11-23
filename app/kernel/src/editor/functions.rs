//! jq functions of the Functions panel: `{{$name(args)}}` runs `name` with the variable tree
//! as its input `.`; the result is data (text), not resolved again.
use super::{Candidate, fuzzy_match};
use serde_json::{Map, Value};
use services_sdk::refs::{self, Root};
use std::collections::BTreeMap;

/// The jq definitions of each service; `*` holds the global ones.
pub type Library = BTreeMap<String, String>;

/// The definitions a call in `service` sees: the global ones, then those of `service` (of
/// every service without one), which win on a name clash.
pub fn scoped(library: &Library, service: Option<&str>) -> String {
    let mut parts: Vec<&str> = library.get("*").map(String::as_str).into_iter().collect();
    for (name, text) in library {
        if name != "*" && service.is_none_or(|service| service == name) {
            parts.push(text);
        }
    }
    parts.join("\n")
}

fn resolved(root: &Root, value: &Value) -> Value {
    match value {
        Value::String(text) if text.contains("{{") => {
            match refs::resolve(text, root, false, &mut |_, _| Err(String::new())) {
                Ok(resolution) => Value::String(resolution.text),
                Err(_) => value.clone(),
            }
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), resolved(root, value)))
                .collect::<Map<_, _>>(),
        ),
        other => other.clone(),
    }
}

/// The input of every function: the variable tree without comments, its references resolved
/// where they resolve (and kept as written where they do not).
pub fn input(root: &Root) -> Value {
    resolved(root, &refs::without_comments(root.tree()))
}

/// The outputs of `name(args)` among `defs` run on `input`.
pub fn run(defs: &str, name: &str, args: &[Value], input: &Value) -> Result<Vec<Value>, String> {
    if !services_jq::names(defs)
        .iter()
        .any(|(known, _)| known == name)
    {
        return Err(format!("Unknown function ${name}"));
    }
    services_jq::call(defs, name, args, input)
}

/// Runs the function calls of `resolve_with` among `defs`; every output lands in `outputs`.
pub fn runner<'a>(
    defs: &'a str,
    root: &'a Root,
    outputs: &'a mut Vec<Value>,
) -> impl FnMut(&str, &[Value]) -> Result<String, String> + 'a {
    let mut input = None;
    move |name, args| {
        let input = input.get_or_insert_with(|| self::input(root));
        let out = run(defs, name, args, input)?;
        let text = services_jq::text(&out);
        outputs.extend(out);
        Ok(text)
    }
}

/// `text` with its variables and function calls resolved (template calls fail).
pub fn resolve(text: &str, root: &Root, defs: &str) -> Result<refs::Resolution, String> {
    let mut outputs = Vec::new();
    refs::resolve_with(
        text,
        root,
        false,
        &mut |_, _| Err(String::new()),
        &mut runner(defs, root, &mut outputs),
    )
}

/// The options of a function call domain (`{{$name(args)}}`): the items of its single array
/// output, or each output.
pub fn options(reference: &str, root: &Root, defs: &str) -> Result<Vec<refs::ListItem>, String> {
    let mut outputs = Vec::new();
    refs::resolve_with(
        reference,
        root,
        false,
        &mut |_, _| Err(String::new()),
        &mut runner(defs, root, &mut outputs),
    )?;
    Ok(services_jq::list(&outputs)
        .into_iter()
        .map(|value| refs::ListItem { key: None, value })
        .collect())
}

/// `name(a; b)` as jq writes the definition.
fn signature(name: &str, params: &[String]) -> String {
    if params.is_empty() {
        name.to_owned()
    } else {
        format!("{name}({})", params.join("; "))
    }
}

/// The functions of `defs` whose name matches `query`: `{{$name(`, or `{{$name}}` for one
/// without parameters; `open` is `{{$` or `{{$!` for an eager call.
pub fn name_candidates(defs: &str, query: &str, open: &str) -> Vec<Candidate> {
    services_jq::names(defs)
        .into_iter()
        .filter(|(name, _)| fuzzy_match(name, query))
        .take(100)
        .map(|(name, params)| {
            let zero = params.is_empty();
            Candidate {
                text: if zero {
                    format!("{open}{name}}}}}")
                } else {
                    format!("{open}{name}(")
                },
                label: Some(signature(&name, &params)),
                detail: if open.ends_with('!') {
                    "jq function, its result inserted on Enter".into()
                } else {
                    "jq function".into()
                },
                badge: None,
                continue_hint: !zero,
                literal: true,
            }
        })
        .collect()
}

/// The index of the argument the cursor is in, `args` being the text of an unfinished call
/// after its `(`: commas count outside quoted literals and nested `{{…}}`; `None` once the
/// call's `)` closes it.
pub fn current_argument(args: &str) -> Option<usize> {
    let (mut index, mut depth) = (0, 0usize);
    let (mut quoted, mut escaped) = (false, false);
    let mut chars = args.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if quoted {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => quoted = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            '{' if args[at..].starts_with("{{") && !args[..at].ends_with('\\') => {
                depth += 1;
                chars.next();
            }
            '}' if depth > 0 && args[at..].starts_with("}}") => {
                depth -= 1;
                chars.next();
            }
            ',' if depth == 0 => index += 1,
            ')' if depth == 0 => return None,
            _ => {}
        }
    }
    Some(index)
}

/// The signature hint of the function `name` of `defs` while argument `index` is typed:
/// `name(a; «b»; c)`. `None` for an unknown function.
pub fn signature_hint(defs: &str, name: &str, index: usize) -> Option<String> {
    let (_, params) = services_jq::names(defs)
        .into_iter()
        .find(|(known, _)| known == name)?;
    let shown: Vec<String> = params
        .iter()
        .enumerate()
        .map(|(at, param)| {
            if at == index {
                format!("«{param}»")
            } else {
                param.clone()
            }
        })
        .collect();
    let extra = if index >= params.len() {
        format!(" — takes {} argument(s)", params.len())
    } else {
        String::new()
    };
    Some(format!("{name}({}){extra}", shown.join("; ")))
}

/// `text` with each eager call `{{$!name(args)}}` replaced by its result (a string as it is,
/// other values as compact JSON, outputs one per line), inserted verbatim: references in it
/// stay references. The first call that fails, or whose result holds a secret value, is the
/// error, and then nothing is replaced.
pub fn eager(text: &str, root: &Root, defs: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for found in refs::scan(text) {
        let refs::Ref::Function {
            name,
            args_text,
            eager: true,
        } = &found.reference
        else {
            continue;
        };
        let shown = found.reference.to_string();
        let resolved = resolve(&shown, root, defs)?;
        if resolved
            .secrets
            .iter()
            .any(|secret| !secret.is_empty() && resolved.text.contains(secret.as_str()))
        {
            let lazy = refs::Ref::Function {
                name: name.clone(),
                args_text: args_text.clone(),
                eager: false,
            };
            return Err(format!(
                "{shown}: the result holds a secret; use {lazy} to resolve it at send time"
            ));
        }
        out.push_str(&text[last..found.start]);
        out.push_str(&resolved.text);
        last = found.end;
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// Whether `text` holds an eager call `{{$!name(args)}}`.
pub fn has_eager(text: &str) -> bool {
    text.contains("{{$!")
        && refs::scan(text)
            .iter()
            .any(|found| matches!(found.reference, refs::Ref::Function { eager: true, .. }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_and_input() {
        let library = Library::from([
            ("*".into(), "def g: 1;".into()),
            ("alpha".into(), "def a: 2;".into()),
            ("beta".into(), "def b: 3;".into()),
        ]);
        assert_eq!(scoped(&library, Some("alpha")), "def g: 1;\ndef a: 2;");
        assert_eq!(scoped(&library, None), "def g: 1;\ndef a: 2;\ndef b: 3;");
        let parse = |text| refs::parse_lines(text).unwrap();
        let root = Root::new(vec![("var".into(), parse("# c\nx: one\ny: {{:x}}-two"))]);
        assert_eq!(
            input(&root),
            serde_json::json!({"var": {"x": "one", "y": "one-two"}})
        );
        let defs = "def pick(k): .var[k];";
        let out = resolve(r#"[{{$pick("y")}}]"#, &root, defs).unwrap();
        assert_eq!(out.text, "[one-two]");
        assert!(
            resolve("{{$nope()}}", &root, defs)
                .unwrap_err()
                .contains("Unknown function $nope")
        );
        let names = name_candidates("def pick(k): .; def all: .;", "", "{{$");
        let texts: Vec<_> = names
            .iter()
            .map(|c| (c.text.as_str(), c.label.as_deref()))
            .collect();
        assert_eq!(
            texts,
            [("{{$pick(", Some("pick(k)")), ("{{$all}}", Some("all"))]
        );
    }

    #[test]
    fn the_current_argument_skips_quotes_and_nested_references() {
        assert_eq!(current_argument(""), Some(0));
        assert_eq!(current_argument(r#""a, b", "#), Some(1));
        assert_eq!(current_argument(r#""a\", b", :x, "#), Some(2));
        assert_eq!(current_argument(r#""{{:a}}, {{$f(1, 2)}}", "#), Some(1));
        assert_eq!(current_argument(r#"{{.a.{{:b}}, x}}, "#), Some(1));
        assert_eq!(current_argument(r#""a")"#), None);
        assert_eq!(current_argument(r#""a)", "#), Some(1));
        let defs = "def target($s; $e; $n): .; def one: 1;";
        assert_eq!(
            signature_hint(defs, "target", 1).as_deref(),
            Some("target($s; «$e»; $n)")
        );
        assert_eq!(
            signature_hint(defs, "one", 0).as_deref(),
            Some("one() — takes 0 argument(s)")
        );
        assert!(signature_hint(defs, "nope", 0).is_none());
    }

    #[test]
    fn eager_calls_are_replaced_by_their_results() {
        let parse = |text| refs::parse_lines(text).unwrap();
        let root = Root::new(vec![(
            "tree".into(),
            parse("a:\n  x: one\n  y: two\ntoken: abc"),
        )]);
        let defs = r#"def pick(k): .tree.a[k]; def keys_of: .tree.a | keys; def ref: "{{.a.x}}"; def tok: .tree.token; def bad: error("boom");"#;
        let run = |text: &str| eager(text, &root, defs);
        assert_eq!(
            run(r#"[{{$!pick("x")}}] {{$pick("y")}} \{{$!ref}}"#).unwrap(),
            r#"[one] {{$pick("y")}} \{{$!ref}}"#
        );
        assert_eq!(run("{{$!keys_of}}").unwrap(), r#"["x","y"]"#);
        assert_eq!(run("<{{$!ref}}>").unwrap(), "<{{.a.x}}>");
        assert!(run("{{$!bad}}").unwrap_err().contains("boom"));
        assert!(
            run("{{$!pick(:nope)}}")
                .unwrap_err()
                .contains("Unknown variable")
        );
        assert!(
            run("{{$!nope}}")
                .unwrap_err()
                .contains("Unknown function $nope")
        );
        assert_eq!(
            run("x {{$!tok}}").unwrap_err(),
            "{{$!tok()}}: the result holds a secret; use {{$tok()}} to resolve it at send time"
        );
        assert!(has_eager("a {{$!pick(\"x\")}}"));
        assert!(!has_eager(r#"{{$pick("x")}} \{{$!pick()}}"#));
    }
}
