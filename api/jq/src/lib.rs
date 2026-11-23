//! jq functions of the Functions panel: definitions (`def name(a; b): …;`) called by
//! `{{$name(args)}}` references with the whole variable tree as input.

use jaq_core::limit::{self, Tripped};
use jaq_core::load::{Arena, File, Loader, span};
use jaq_core::{Compiler, Ctx, Vars, data, unwrap_valr};
use jaq_json::Val;
use serde_json::Value;

/// At most this many outputs of one call; a call with more fails.
pub const MAX_OUTPUTS: usize = 10_000;

/// Bounds of one evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// At most this many evaluation steps, so that an endless loop fails instead of
    /// hanging the page.
    pub steps: u64,
    /// At most this deep evaluation nesting, so that endless recursion fails instead of
    /// overflowing the stack (and killing the kernel).
    pub depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            steps: 2_000_000,
            depth: 512,
        }
    }
}

fn line_of(code: &str, part: &str) -> String {
    let at = span(code, part).start.min(code.len());
    let line = code[..at].matches('\n').count() + 1;
    let near: String = part.chars().take(20).collect();
    if near.trim().is_empty() {
        format!("Line {line}")
    } else {
        format!("Line {line} near `{}`", near.trim())
    }
}

fn compile(
    code: &str,
) -> Result<jaq_core::compile::Filter<jaq_core::Native<data::JustLut<Val>>>, String> {
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs());
    let funs = jaq_core::funs()
        .chain(jaq_std::funs())
        .chain(jaq_json::funs());
    let loader = Loader::new(defs);
    let arena = Arena::default();
    let modules = loader
        .load(&arena, File { code, path: () })
        .map_err(|errors| {
            let mut messages = Vec::new();
            for (_, error) in errors {
                match error {
                    jaq_core::load::Error::Io(items) => {
                        messages.extend(items.into_iter().map(|(_, error)| error));
                    }
                    jaq_core::load::Error::Lex(items) => {
                        messages.extend(items.into_iter().map(|(expect, part)| {
                            let part = match expect {
                                jaq_core::load::lex::Expect::Delim(open) => open,
                                _ => part,
                            };
                            format!("{}: expected {}", line_of(code, part), expect.as_str())
                        }));
                    }
                    jaq_core::load::Error::Parse(items) => {
                        messages.extend(items.into_iter().map(|(expect, part)| {
                            format!("{}: expected {}", line_of(code, part), expect.as_str())
                        }));
                    }
                }
            }
            messages.join("; ")
        })?;
    Compiler::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|errors| {
            let mut messages = Vec::new();
            for (_, items) in errors {
                for (part, undefined) in items {
                    messages.push(format!(
                        "{}: undefined {} {part}",
                        line_of(code, part),
                        undefined.as_str()
                    ));
                }
            }
            messages.join("; ")
        })
}

/// Whether `defs` compile; the error names the line.
pub fn check(defs: &str) -> Result<(), String> {
    compile(&format!("{defs}\n.")).map(|_| ())
}

/// The top-level functions `defs` defines, with their parameters (`$x` keeps its `$`), in
/// order; a name defined again keeps its first place.
pub fn names(defs: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    // The bracket depth at which each open definition's body ends with `;`.
    let (mut open, mut brackets): (Vec<usize>, usize) = (Vec::new(), 0);
    let mut rest = defs;
    while let Some(c) = rest.chars().next() {
        if c == '#' {
            rest = rest.find('\n').map_or("", |n| &rest[n..]);
            continue;
        }
        if c == '"' {
            let mut escaped = false;
            let end = rest[1..]
                .char_indices()
                .find(|&(_, ch)| {
                    let close = ch == '"' && !escaped;
                    escaped = ch == '\\' && !escaped;
                    close
                })
                .map_or(rest.len(), |(index, _)| index + 2);
            rest = &rest[end..];
            continue;
        }
        let word_end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        if word_end > 0 {
            let word = &rest[..word_end];
            rest = &rest[word_end..];
            if word == "def" {
                if open.is_empty() && brackets == 0 {
                    let after = rest.trim_start();
                    let end = after
                        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .unwrap_or(after.len());
                    let name = &after[..end];
                    let params = after[end..]
                        .trim_start()
                        .strip_prefix('(')
                        .and_then(|tail| tail.split_once(')'))
                        .map(|(inside, _)| {
                            inside
                                .split(';')
                                .map(|param| param.trim().to_owned())
                                .filter(|param| !param.is_empty())
                                .collect()
                        })
                        .unwrap_or_default();
                    if !name.is_empty() && !out.iter().any(|(known, _)| known == name) {
                        out.push((name.to_owned(), params));
                    }
                }
                open.push(brackets);
            }
            continue;
        }
        match c {
            ';' if open.last() == Some(&brackets) => {
                open.pop();
            }
            '(' | '[' | '{' => brackets += 1,
            ')' | ']' | '}' => brackets = brackets.saturating_sub(1),
            _ => {}
        }
        rest = &rest[c.len_utf8()..];
    }
    out
}

fn to_val(value: &Value) -> Result<Val, String> {
    jaq_json::read::parse_single(value.to_string().as_bytes()).map_err(|error| error.to_string())
}

fn from_val(value: &Val) -> Value {
    let text = value.to_string();
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}

/// The outputs of `name(args…)` (each argument a JSON value) defined in `defs`, run on
/// `input`; a jq error, more than [`MAX_OUTPUTS`] outputs, or exceeding the default
/// [`Limits`] fails the call.
pub fn call(defs: &str, name: &str, args: &[Value], input: &Value) -> Result<Vec<Value>, String> {
    call_with_limits(defs, name, args, input, Limits::default())
}

/// [`call`] bounded by `limits`.
pub fn call_with_limits(
    defs: &str,
    name: &str,
    args: &[Value],
    input: &Value,
    limits: Limits,
) -> Result<Vec<Value>, String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':')
    {
        return Err(format!("invalid function name `{name}`"));
    }
    let main = if args.is_empty() {
        name.to_owned()
    } else {
        let args: Vec<String> = args.iter().map(Value::to_string).collect();
        format!("{name}({})", args.join("; "))
    };
    let filter = compile(&format!("{defs}\n{main}"))?;
    let ctx = Ctx::<data::JustLut<Val>>::new(&filter.lut, Vars::new([]));
    let input = to_val(input)?;
    limit::set(limits.steps, limits.depth);
    let out = collect(filter.id.run((ctx, input)).map(unwrap_valr), name);
    let tripped = limit::tripped();
    limit::clear();
    match tripped {
        Some(Tripped::Steps(steps)) => Err(format!(
            "${name}: evaluation stopped: more than {steps} steps (infinite loop?)"
        )),
        Some(Tripped::Depth(depth)) => Err(format!(
            "${name}: evaluation stopped: recursion deeper than {depth}"
        )),
        None => out,
    }
}

fn collect(
    values: impl Iterator<Item = jaq_core::ValR<Val>>,
    name: &str,
) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for value in values {
        if out.len() >= MAX_OUTPUTS {
            return Err(format!("${name} gave more than {MAX_OUTPUTS} outputs"));
        }
        out.push(from_val(&value.map_err(|error| error.to_string())?));
    }
    Ok(out)
}

fn item(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The outputs as text: a string as it is, any other value as compact JSON, one per line.
pub fn text(outputs: &[Value]) -> String {
    outputs.iter().map(item).collect::<Vec<_>>().join("\n")
}

/// The outputs as a list of options: the items of a single array output, else every output.
pub fn list(outputs: &[Value]) -> Vec<String> {
    match outputs {
        [Value::Array(items)] => items.iter().map(item).collect(),
        outputs => outputs.iter().map(item).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DEFS: &str = "# helpers\ndef keys_of(s): .tree[s] | keys;\ndef pair($a; $b): \"\\($a)-\\($b)\";\ndef all: def inner: .; .tree | inner;\ndef many: .tree.alpha[];";

    fn input() -> Value {
        json!({"tree": {"alpha": {"one": "x", "two": 2}, "beta": {}}})
    }

    #[test]
    fn calls_with_json_arguments_and_collects_outputs() {
        assert_eq!(
            call(DEFS, "keys_of", &[json!("alpha")], &input()).unwrap(),
            [json!(["one", "two"])]
        );
        assert_eq!(
            call(DEFS, "pair", &[json!("a"), json!(1)], &input()).unwrap(),
            [json!("a-1")]
        );
        let many = call(DEFS, "many", &[], &input()).unwrap();
        assert_eq!(many, [json!("x"), json!(2)]);
        assert_eq!(text(&many), "x\n2");
        assert_eq!(list(&many), ["x", "2"]);
        assert_eq!(list(&[json!(["one", {"a": 1}])]), ["one", r#"{"a":1}"#]);
        assert_eq!(text(&[json!({"a": [1]})]), r#"{"a":[1]}"#);
    }

    #[test]
    fn reports_runtime_and_compile_errors() {
        let error = call(
            DEFS,
            "keys_of",
            &[json!("one")],
            &json!({"tree": {"one": 1}}),
        );
        let error = error.unwrap_err();
        assert!(!error.is_empty(), "{error}");
        assert!(
            call(DEFS, "nope", &[], &input())
                .unwrap_err()
                .contains("nope")
        );
        assert!(
            call("def f: range(100000);", "f", &[], &input())
                .unwrap_err()
                .contains("more than")
        );
        assert_eq!(check(DEFS), Ok(()));
        let error = check("def a: .;\ndef b: (.;").unwrap_err();
        assert!(error.starts_with("Line 2"), "{error}");
        let error = check("def a: .;\n\ndef b: missing;").unwrap_err();
        assert!(
            error.starts_with("Line 3") && error.contains("missing"),
            "{error}"
        );
    }

    #[test]
    fn endless_evaluation_fails_quickly() {
        let steps = [
            "def a: a;",
            "def a: repeat(1) | select(false);",
            "def a: until(false; .);",
            "def a: [range(1e12)] | length;",
            "def a: last(range(1e12));",
            "def a: [.[]?] | a;",
            "def a: def r: (.x | r), .; [path(r)] | length;",
        ];
        let depth = [
            "def a: [a];",
            "def a: 1 + a;",
            "def a: {x: a};",
            "def a: try [a] catch [a];",
            "def a: try a catch a;",
            "def a: (a)?; def x: a, 1;",
            "def a: def r: try error catch r; r;",
            "def a: (a | a);",
            "def a: def r: (.x | r) // .; {} | r |= 1;",
            "def a: def r: 1, (r | . + 1); [limit(1e6; r)] | length;",
            "def a: def r(n): if n == 0 then 0 else 1 + r(n - 1) end; r(1e5);",
        ];
        let cases = steps
            .into_iter()
            .map(|defs| (defs, "steps (infinite loop?)"))
            .chain(depth.map(|defs| (defs, "recursion deeper than")));
        // Debug builds have far larger frames than the release build the page runs.
        let run = std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(move || {
                for (defs, stop) in cases {
                    let name = if defs.contains("def x") { "x" } else { "a" };
                    let start = std::time::Instant::now();
                    let error = call(defs, name, &[], &json!({"tree": 1})).unwrap_err();
                    assert!(
                        error.starts_with(&format!("${name}: evaluation stopped: "))
                            && error.contains(stop),
                        "{defs}: {error}"
                    );
                    assert!(
                        start.elapsed().as_secs() < 2,
                        "{defs}: {:?}",
                        start.elapsed()
                    );
                }
            });
        run.unwrap().join().unwrap();
        let fine = [
            ("def a: [range(1e5)] | length;", json!(100000)),
            ("def a: [limit(100; recurse([.]))] | length;", json!(100)),
            (
                "def a: def r(n): if n == 0 then 0 else 1 + r(n - 1) end; r(50);",
                json!(50),
            ),
        ];
        for (defs, out) in fine {
            assert_eq!(call(defs, "a", &[], &input()).unwrap(), [out], "{defs}");
        }
    }

    #[test]
    fn names_are_the_top_level_definitions() {
        assert_eq!(
            names(DEFS),
            [
                ("keys_of".to_owned(), vec!["s".to_owned()]),
                ("pair".to_owned(), vec!["$a".to_owned(), "$b".to_owned()]),
                ("all".to_owned(), vec![]),
                ("many".to_owned(), vec![]),
            ]
        );
        assert_eq!(names("def x: \"def y: 1;\"; def z: 2;").len(), 2);
    }
}
