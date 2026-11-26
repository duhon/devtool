//! What may follow what in PromQL: the completion slot before the cursor and the items that
//! fit it, with metric types from discovery deciding which metrics come first and how they
//! are wrapped.
use crate::discovery::Discovery;
use crate::language::{FUNCTIONS, Lexeme};
use services_sdk::{Check, Item, lang};

pub const AGGREGATIONS: &[&str] = &[
    "sum",
    "avg",
    "count",
    "min",
    "max",
    "group",
    "stddev",
    "stdvar",
    "topk",
    "bottomk",
    "count_values",
    "quantile",
    "limitk",
    "limit_ratio",
];
/// Functions whose (first) argument is a range of a counter.
const COUNTER_RANGE: &[&str] = &["rate", "irate", "increase", "resets"];
/// Functions whose first argument is a range of a gauge.
const GAUGE_RANGE: &[&str] = &[
    "delta",
    "idelta",
    "deriv",
    "predict_linear",
    "changes",
    "double_exponential_smoothing",
    "holt_winters",
];
/// Aggregations and functions whose first argument is a number.
const NUMBER_FIRST: &[&str] = &[
    "topk",
    "bottomk",
    "limitk",
    "limit_ratio",
    "quantile",
    "histogram_quantile",
];
const BINARY: &[&str] = &[
    "+", "-", "*", "/", "%", "^", "==", "!=", ">", "<", ">=", "<=", "and", "or", "unless", "atan2",
];
const COMPARISON: &[&str] = &["==", "!=", ">", "<", ">=", "<="];

#[derive(Debug, PartialEq, Eq)]
pub enum Slot {
    /// An expression goes here; `call` is the enclosing function and the argument index.
    Operand {
        call: Option<(String, usize)>,
        after: Option<String>,
    },
    /// After a complete expression: an operator, `offset`, `@`; `selector` takes a range.
    After { selector: bool, aggregated: bool },
    /// After an aggregation name: its grouping or its argument.
    Aggregation,
    /// After `sum by (…)`: the argument.
    Grouped,
}

fn range_function(name: &str) -> bool {
    COUNTER_RANGE.contains(&name) || GAUGE_RANGE.contains(&name) || name.ends_with("_over_time")
}

/// The innermost unclosed `(` before the end of `tokens`, with the number of commas after it.
fn enclosing(tokens: &[Lexeme<'_>]) -> Option<(usize, usize)> {
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut depth_other = 0usize;
    for (i, t) in tokens.iter().enumerate() {
        match t.text {
            "(" => stack.push((i, 0)),
            ")" => {
                stack.pop();
            }
            "{" | "[" => depth_other += 1,
            "}" | "]" => depth_other = depth_other.saturating_sub(1),
            "," if depth_other == 0 => {
                if let Some(last) = stack.last_mut() {
                    last.1 += 1;
                }
            }
            _ => {}
        }
    }
    stack.last().copied()
}

/// The `(` matching the `)` at `close`.
fn opening(tokens: &[Lexeme<'_>], close: usize) -> Option<usize> {
    let mut depth = 0;
    for i in (0..=close).rev() {
        match tokens[i].text {
            ")" => depth += 1,
            "(" => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The slot after `tokens`, the complete tokens before the word being typed.
pub(crate) fn slot(tokens: &[Lexeme<'_>]) -> Slot {
    let operand = |after: Option<&str>| {
        let call = enclosing(tokens).and_then(|(open, arg)| {
            let name = tokens.get(open.checked_sub(1)?)?;
            matches!(name.kind, "def" | "keyword").then(|| (name.text.to_owned(), arg))
        });
        Slot::Operand {
            call,
            after: after.map(str::to_owned),
        }
    };
    let Some(last) = tokens.last() else {
        return operand(None);
    };
    let before = |i: usize| i.checked_sub(1).and_then(|i| tokens.get(i)).map(|t| t.text);
    match last.text {
        "(" | "," | "bool" => operand(None),
        ")" => {
            let open = opening(tokens, tokens.len() - 1);
            let head = open.and_then(before);
            match head {
                Some("by" | "without") => {
                    let aggregation = open
                        .and_then(|i| i.checked_sub(1))
                        .and_then(before)
                        .is_some_and(|name| AGGREGATIONS.contains(&name));
                    if aggregation {
                        Slot::Grouped
                    } else {
                        Slot::After {
                            selector: false,
                            aggregated: false,
                        }
                    }
                }
                Some("on" | "ignoring") => operand(Some("on")),
                Some("group_left" | "group_right") => operand(None),
                head => Slot::After {
                    selector: false,
                    aggregated: head.is_some_and(|name| AGGREGATIONS.contains(&name)),
                },
            }
        }
        text if AGGREGATIONS.contains(&text) => Slot::Aggregation,
        "by" | "without" | "on" | "ignoring" | "group_left" | "group_right" => Slot::Grouped,
        text if BINARY.contains(&text) => operand(Some(text)),
        "}" => Slot::After {
            selector: true,
            aggregated: false,
        },
        _ if last.kind == "variable" => Slot::After {
            selector: true,
            aggregated: false,
        },
        _ if matches!(last.kind, "number" | "string") || last.text == "]" => Slot::After {
            selector: false,
            aggregated: false,
        },
        _ => operand(None),
    }
}

/// The type of metric `name`, by its own metadata or its histogram or summary family's.
pub fn kind(discovery: &Discovery, check: &Check, name: &str) -> Option<&'static str> {
    let of = |name: &str| discovery.metadata(check, name).map(|m| m.kind.as_str());
    let known = |kind: &str| match kind {
        "counter" => Some("counter"),
        "gauge" => Some("gauge"),
        "histogram" | "gaugehistogram" => Some("histogram"),
        "summary" => Some("summary"),
        _ => None,
    };
    if let Some(kind) = of(name).and_then(known) {
        return Some(kind);
    }
    for suffix in ["_bucket", "_count", "_sum", "_total", "_created"] {
        let Some(family) = name.strip_suffix(suffix) else {
            continue;
        };
        return match (of(family).and_then(known), suffix) {
            (Some("histogram"), "_bucket") => Some("bucket"),
            (Some("histogram" | "summary"), "_count" | "_sum") | (Some("counter"), _) => {
                Some("counter")
            }
            _ => None,
        };
    }
    None
}

fn snippet(insert: String, detail: &str, kind: &str) -> Item {
    Item {
        next: insert.contains("[$0]"),
        ..lang::item(insert, detail, kind)
    }
}

/// The items of `slot`; `metrics` with their types; `typed` is the word being typed.
pub fn items(slot: &Slot, metrics: &[(String, Option<&'static str>)], typed: &str) -> Vec<Item> {
    let mut items = Vec::new();
    let functions = || {
        FUNCTIONS
            .split_whitespace()
            .map(|s| lang::item(format!("{s}($0)"), "PromQL function", "function"))
    };
    let aggregations = || {
        AGGREGATIONS
            .iter()
            .map(|s| lang::item(*s, "Aggregation", "keyword"))
    };
    let detail = |kind: Option<&str>| kind.map_or("Metric".to_owned(), |k| format!("Metric · {k}"));
    match slot {
        Slot::Aggregation => {
            items.push(lang::item("by ($0)", "Group by labels", "keyword"));
            items.push(lang::item(
                "without ($0)",
                "Group without labels",
                "keyword",
            ));
            items.push(lang::item("($0)", "Aggregated expression", "operator"));
        }
        Slot::Grouped => items.push(lang::item("($0)", "Aggregated expression", "operator")),
        Slot::After {
            selector,
            aggregated,
        } => {
            if *selector {
                items.push(snippet("[$0]".into(), "Range of samples", "operator"));
            }
            if *aggregated {
                items.push(lang::item("by ($0)", "Group by labels", "keyword"));
                items.push(lang::item(
                    "without ($0)",
                    "Group without labels",
                    "keyword",
                ));
            }
            items.extend(
                BINARY
                    .iter()
                    .map(|s| lang::item(*s, "Operator", "operator")),
            );
            items.push(lang::item("offset", "Shift back in time", "keyword"));
            items.push(lang::item("@", "Evaluate at a time", "operator"));
        }
        Slot::Operand { call, after } => {
            let call = call.as_ref().map(|(name, arg)| (name.as_str(), *arg));
            match call {
                Some((name, 0)) if NUMBER_FIRST.contains(&name) => {
                    items.extend(
                        ["0.5", "0.9", "0.95", "0.99", "5", "10"]
                            .iter()
                            .filter(|n| {
                                (name == "histogram_quantile" || name == "quantile")
                                    == n.contains('.')
                            })
                            .map(|n| lang::item(*n, "Parameter", "number")),
                    );
                    return items;
                }
                Some(("count_values", 0)) => {
                    items.push(lang::item("\"value\"", "Output label", "string"));
                    return items;
                }
                _ => {}
            }
            let range = call.filter(|(name, arg)| *arg == 0 && range_function(name));
            let wanted: &[&str] = match call {
                Some((name, 0)) if COUNTER_RANGE.contains(&name) => &["counter"],
                Some((name, 0)) if range_function(name) => &["gauge"],
                Some(("histogram_quantile", 1)) => &["bucket"],
                _ => &[],
            };
            let mut ordered: Vec<_> = metrics.iter().collect();
            ordered.sort_by_key(|(_, kind)| !kind.is_some_and(|k| wanted.contains(&k)));
            for (name, kind) in ordered {
                if range.is_some() {
                    items.push(snippet(format!("{name}[$0]"), &detail(*kind), "variable"));
                    continue;
                }
                if call.is_some_and(|(name, arg)| name == "histogram_quantile" && arg == 1)
                    && *kind == Some("bucket")
                {
                    items.push(snippet(
                        format!("sum by (le) (rate({name}[$0]))"),
                        "Bucket rates by le",
                        "variable",
                    ));
                }
                items.push(lang::item(name.as_str(), &detail(*kind), "variable"));
                // Whole expressions a type is usually queried by, once the name is typed.
                if typed.is_empty() || call.is_some() {
                    continue;
                }
                match kind {
                    Some("counter") => items.push(snippet(
                        format!("rate({name}[$0])"),
                        "Counter per second",
                        "variable",
                    )),
                    Some("bucket") => items.push(snippet(
                        format!("histogram_quantile(0.95, sum by (le) (rate({name}[$0])))"),
                        "Histogram quantile",
                        "variable",
                    )),
                    _ => {}
                }
            }
            if after.as_deref().is_some_and(|op| COMPARISON.contains(&op)) {
                items.push(lang::item("bool", "Return 0 or 1", "keyword"));
            }
            if after.as_deref().is_some_and(|op| BINARY.contains(&op)) {
                items.push(lang::item("on ($0)", "Match on labels", "keyword"));
                items.push(lang::item(
                    "ignoring ($0)",
                    "Match ignoring labels",
                    "keyword",
                ));
            }
            if after.as_deref() == Some("on") {
                items.push(lang::item("group_left ($0)", "Many-to-one", "keyword"));
                items.push(lang::item("group_right ($0)", "One-to-many", "keyword"));
            }
            items.extend(functions());
            if range.is_none() {
                items.extend(aggregations());
            }
        }
    }
    items
}
