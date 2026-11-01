//! `#action`: what an Artemis check does, a command, the queue or address it works on and
//! SQL-like clauses, on one line or several (a line break is a space).

use serde_json::{Map, Value, json};

pub const MAX_LIMIT: u64 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Queues,
    Addresses,
    Consumers,
    Browse,
    Receive,
    Send,
    Publish,
    Purge,
    Move,
    Retry,
    Pause,
    Resume,
    Jolokia,
}

pub const COMMANDS: [Command; 13] = [
    Command::Queues,
    Command::Addresses,
    Command::Consumers,
    Command::Browse,
    Command::Receive,
    Command::Send,
    Command::Publish,
    Command::Purge,
    Command::Move,
    Command::Retry,
    Command::Pause,
    Command::Resume,
    Command::Jolokia,
];

impl Command {
    pub fn name(self) -> &'static str {
        match self {
            Self::Queues => "QUEUES",
            Self::Addresses => "ADDRESSES",
            Self::Consumers => "CONSUMERS",
            Self::Browse => "BROWSE",
            Self::Receive => "RECEIVE",
            Self::Send => "SEND",
            Self::Publish => "PUBLISH",
            Self::Purge => "PURGE",
            Self::Move => "MOVE",
            Self::Retry => "RETRY",
            Self::Pause => "PAUSE",
            Self::Resume => "RESUME",
            Self::Jolokia => "JOLOKIA",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        COMMANDS
            .into_iter()
            .find(|command| command.name().eq_ignore_ascii_case(word))
    }

    /// QUEUES, ADDRESSES and CONSUMERS list what the broker has, filtered by one condition.
    pub fn lists(self) -> bool {
        matches!(self, Self::Queues | Self::Addresses | Self::Consumers)
    }

    fn default_limit(self) -> Option<u64> {
        match self {
            Self::Queues | Self::Addresses | Self::Consumers => Some(100),
            Self::Browse => Some(10),
            Self::Receive => Some(1),
            _ => None,
        }
    }

    /// What the word after the command names: `None` when it takes none.
    pub fn resource(self) -> Option<&'static str> {
        match self {
            Self::Queues | Self::Addresses => Some("name part"),
            Self::Publish => Some("address"),
            Self::Jolokia => None,
            _ => Some("queue"),
        }
    }
}

fn names(commands: &[Command]) -> String {
    let names: Vec<_> = commands.iter().map(|command| command.name()).collect();
    match names.split_last() {
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// One condition of a list, as `listQueues`, `listAddresses` and `listConsumers` take it.
#[derive(Debug, PartialEq)]
pub struct Condition {
    /// A result column (`messages`, any case) or a field of the broker (`messageCount`).
    pub column: String,
    /// `EQUALS`, `NOT_EQUALS`, `GREATER_THAN`, `LESS_THAN`, `CONTAINS` or `NOT_CONTAINS`.
    pub operation: &'static str,
    pub value: String,
}

#[derive(Debug, PartialEq)]
pub struct Action {
    pub command: Command,
    /// The queue, the address of PUBLISH, or the name part of a list; may be empty for a list.
    pub resource: String,
    /// The queue MOVE moves messages to.
    pub to: String,
    /// The condition of a list.
    pub condition: Option<Condition>,
    /// WHERE of BROWSE, RECEIVE, PURGE and MOVE: a JMS selector, as written.
    pub selector: String,
    /// 0 for a command without LIMIT.
    pub limit: u64,
    /// SET of SEND and PUBLISH: string properties of the message.
    pub properties: Map<String, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Word,
    Quoted,
    Symbol,
}

#[derive(Debug)]
struct Token {
    kind: Kind,
    /// Without the quotes of a quoted token.
    text: String,
    start: usize,
    end: usize,
}

impl Token {
    fn is(&self, keyword: &str) -> bool {
        self.kind == Kind::Word && self.text.eq_ignore_ascii_case(keyword)
    }

    fn symbol(&self, symbol: &str) -> bool {
        self.kind == Kind::Symbol && self.text == symbol
    }
}

const SYMBOLS: &str = "=<>!,()";

fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let mut tokens = vec![];
    let mut chars = text.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '\'' || c == '"' {
            let mut value = String::new();
            let mut end = None;
            while let Some((at, next)) = chars.next() {
                if next != c {
                    value.push(next);
                } else if chars.peek().is_some_and(|(_, after)| *after == c) {
                    chars.next();
                    value.push(c);
                } else {
                    end = Some(at + 1);
                    break;
                }
            }
            let end = end.ok_or_else(|| format!("A quote is not closed: {}", &text[start..]))?;
            tokens.push(Token {
                kind: Kind::Quoted,
                text: value,
                start,
                end,
            });
        } else if SYMBOLS.contains(c) {
            let pair = chars
                .peek()
                .map(|(_, next)| format!("{c}{next}"))
                .filter(|pair| ["<=", ">=", "<>", "!="].contains(&pair.as_str()));
            let text = match pair {
                Some(pair) => {
                    chars.next();
                    pair
                }
                None => c.to_string(),
            };
            tokens.push(Token {
                kind: Kind::Symbol,
                end: start + text.len(),
                text,
                start,
            });
        } else {
            let mut end = start + c.len_utf8();
            while let Some((at, next)) = chars.peek().copied() {
                if next.is_whitespace() || next == '\'' || next == '"' || SYMBOLS.contains(next) {
                    break;
                }
                chars.next();
                end = at + next.len_utf8();
            }
            tokens.push(Token {
                kind: Kind::Word,
                text: text[start..end].to_owned(),
                start,
                end,
            });
        }
    }
    Ok(tokens)
}

/// Splits `tokens` at the first `keyword`: before it, and after it (`None` without one).
fn split<'a>(tokens: &'a [Token], keyword: &str) -> (&'a [Token], Option<&'a [Token]>) {
    match tokens.iter().position(|token| token.is(keyword)) {
        Some(at) => (&tokens[..at], Some(&tokens[at + 1..])),
        None => (tokens, None),
    }
}

/// A queue or address: a word with an optional leading `/`, or a quoted name.
fn name(token: &Token) -> Result<String, String> {
    let name = match token.kind {
        Kind::Quoted => token.text.clone(),
        Kind::Word => token
            .text
            .strip_prefix('/')
            .unwrap_or(&token.text)
            .to_owned(),
        Kind::Symbol => String::new(),
    };
    if name.is_empty() {
        Err(format!("Expected a name, got {}", token.text))
    } else {
        Ok(name)
    }
}

pub fn parse(text: &str) -> Result<Action, String> {
    let tokens = tokenize(text)?;
    let usage = || {
        format!(
            "#action starts with a command: {}",
            names(&COMMANDS).replace(" or ", ", ")
        )
    };
    let command = match tokens.first() {
        None => Command::Queues,
        Some(first) if first.kind == Kind::Word => Command::parse(&first.text).ok_or_else(usage)?,
        Some(_) => return Err(usage()),
    };
    let name = command.name();
    let mut rest = tokens.get(1..).unwrap_or_default();

    let mut limit = None;
    if let [head @ .., keyword, number] = rest
        && keyword.is("LIMIT")
    {
        let invalid = || format!("LIMIT is a number from 1 to {MAX_LIMIT}");
        let value = number.text.parse::<u64>().map_err(|_| invalid())?;
        if number.kind != Kind::Word || !(1..=MAX_LIMIT).contains(&value) {
            return Err(invalid());
        }
        limit = Some(value);
        rest = head;
    }
    let (rest, mut condition) = split(rest, "WHERE");
    let (rest, set) = split(rest, "SET");
    let (head, mut to) = split(rest, "TO");
    // MOVE /dlq WHERE … TO /orders: TO may follow the selector.
    if command == Command::Move
        && to.is_none()
        && let Some([selector @ .., keyword, target]) = condition
        && keyword.is("TO")
    {
        condition = Some(selector);
        to = Some(std::slice::from_ref(target));
    }
    for token in head.iter().chain(to.into_iter().flatten()) {
        if token.is("LIMIT") {
            return Err(format!(
                "LIMIT goes last and takes a number: {name} … LIMIT 10"
            ));
        }
    }
    let clause = |present: bool, clause: &str, commands: &[Command]| {
        if present && !commands.contains(&command) {
            Err(format!(
                "{clause} applies to {}, not {name}",
                names(commands)
            ))
        } else {
            Ok(())
        }
    };
    use Command::*;
    clause(
        limit.is_some(),
        "LIMIT",
        &[Queues, Addresses, Consumers, Browse, Receive],
    )?;
    clause(
        condition.is_some(),
        "WHERE",
        &[Queues, Addresses, Consumers, Browse, Receive, Purge, Move],
    )?;
    clause(set.is_some(), "SET", &[Send, Publish])?;
    clause(to.is_some(), "TO", &[Move])?;

    let resource = match (head, command.resource()) {
        ([], _) => String::new(),
        ([token], Some(_)) => self::name(token)?,
        ([_], None) => return Err(format!("{name} takes no queue: its request is #body")),
        ([first, second, ..], _) => {
            return Err(format!(
                "Unexpected {} after {}{}",
                second.text,
                first.text,
                if command.lists() {
                    format!(": a list takes one name part or one condition, {name} WHERE …")
                } else {
                    String::new()
                }
            ));
        }
    };
    if resource.is_empty()
        && !command.lists()
        && let Some(what) = command.resource()
    {
        return Err(format!("Add the {what}: {name} /{what}"));
    }
    let to = match to {
        None if command == Move => {
            return Err("Add the queue to move to: MOVE /DLQ TO /orders".into());
        }
        None => String::new(),
        Some([target]) => self::name(target)?,
        Some(_) => return Err("TO takes one queue: MOVE /DLQ TO /orders".into()),
    };
    let condition = match condition {
        Some([]) => return Err(format!("WHERE needs a condition: {name} … WHERE …")),
        Some(tokens) => Some(tokens),
        None => None,
    };
    let (condition, selector) = if command.lists() {
        if condition.is_some() && !resource.is_empty() {
            return Err(format!(
                "{name} filters by one condition: drop {resource} or WHERE"
            ));
        }
        let condition = match condition {
            Some(tokens) => Some(list_condition(tokens)?),
            None if resource.is_empty() => None,
            None => Some(Condition {
                column: if command == Addresses {
                    "address"
                } else {
                    "queue"
                }
                .into(),
                operation: if command == Consumers {
                    "EQUALS"
                } else {
                    "CONTAINS"
                },
                value: resource.clone(),
            }),
        };
        (condition, String::new())
    } else {
        let selector = condition
            .map(|tokens| text[tokens[0].start..tokens[tokens.len() - 1].end].to_owned())
            .unwrap_or_default();
        (None, selector)
    };
    Ok(Action {
        command,
        resource: if command.lists() {
            String::new()
        } else {
            resource
        },
        to,
        condition,
        selector,
        limit: limit.or(command.default_limit()).unwrap_or(0),
        properties: match set {
            Some(tokens) => properties(tokens)?,
            None => Map::new(),
        },
    })
}

fn list_condition(tokens: &[Token]) -> Result<Condition, String> {
    let usage =
        "A list takes one condition: WHERE messages > 0 (=, !=, <, >, CONTAINS or NOT CONTAINS)";
    let (column, operation, value) = match tokens {
        [column, not, contains, value] if not.is("NOT") && contains.is("CONTAINS") => {
            (column, "NOT_CONTAINS", value)
        }
        [column, operation, value] => (
            column,
            match operation.text.to_ascii_uppercase().as_str() {
                "=" if operation.kind == Kind::Symbol => "EQUALS",
                "!=" | "<>" if operation.kind == Kind::Symbol => "NOT_EQUALS",
                ">" if operation.kind == Kind::Symbol => "GREATER_THAN",
                "<" if operation.kind == Kind::Symbol => "LESS_THAN",
                "CONTAINS" if operation.kind == Kind::Word => "CONTAINS",
                _ => return Err(usage.into()),
            },
            value,
        ),
        _ => return Err(usage.into()),
    };
    if column.kind != Kind::Word || value.kind == Kind::Symbol {
        return Err(usage.into());
    }
    Ok(Condition {
        column: column.text.clone(),
        operation,
        value: value.text.clone(),
    })
}

fn properties(tokens: &[Token]) -> Result<Map<String, Value>, String> {
    let usage = "SET takes name = 'value' pairs separated by commas";
    let mut properties = Map::new();
    for pair in tokens.split(|token| token.symbol(",")) {
        let [name, equals, value] = pair else {
            return Err(usage.into());
        };
        if name.kind == Kind::Symbol || !equals.symbol("=") {
            return Err(usage.into());
        }
        if value.kind != Kind::Quoted {
            return Err(format!(
                "SET values are text in quotes: {} = '{}'. Artemis management sends message properties as strings",
                name.text, value.text
            ));
        }
        if properties
            .insert(name.text.clone(), json!(value.text))
            .is_some()
        {
            return Err(format!("SET names {} twice", name.text));
        }
    }
    Ok(properties)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(text: &str) -> String {
        parse(text).unwrap_err()
    }

    #[test]
    fn a_send_reads_the_same_on_one_line_or_several() {
        let one =
            parse("send /orders SET tenant = '{{tenant}}', note = 'it''s', \"x\" = \"y\"").unwrap();
        let several = parse(
            "SEND orders\n  SET tenant = '{{tenant}}',\n      note = 'it''s',\n  \"x\" = \"y\"",
        )
        .unwrap();
        assert_eq!(one, several);
        assert_eq!(one.command, Command::Send);
        assert_eq!(one.resource, "orders");
        assert_eq!(
            Value::Object(one.properties),
            json!({"tenant":"{{tenant}}","note":"it's","x":"y"})
        );
        assert_eq!(
            parse("PUBLISH /events SET k = 'v'").unwrap().command,
            Command::Publish
        );
    }

    #[test]
    fn a_selector_is_kept_as_written_up_to_limit_and_to() {
        let browse =
            parse("BROWSE /DLQ WHERE _AMQ_ORIG_ADDRESS = 'orders'\n AND n > 4 LIMIT 20").unwrap();
        assert_eq!(
            (browse.selector.as_str(), browse.limit),
            ("_AMQ_ORIG_ADDRESS = 'orders'\n AND n > 4", 20)
        );
        assert_eq!(parse("RECEIVE 'my queue'").unwrap().limit, 1);
        assert_eq!(parse("RECEIVE 'my queue'").unwrap().resource, "my queue");
        let moved = parse("MOVE /DLQ TO /orders WHERE tenant = 't1'").unwrap();
        assert_eq!(
            moved,
            parse("MOVE /DLQ WHERE tenant = 't1' TO /orders").unwrap()
        );
        assert_eq!(
            (moved.resource.as_str(), moved.to.as_str(), moved.limit),
            ("DLQ", "orders", 0)
        );
        assert_eq!(parse("PURGE /orders").unwrap().selector, "");
    }

    #[test]
    fn a_list_takes_a_name_part_or_one_condition() {
        assert_eq!(parse("").unwrap(), parse("QUEUES").unwrap());
        assert_eq!(parse("QUEUES").unwrap().limit, 100);
        let condition = |text| parse(text).unwrap().condition.unwrap();
        assert_eq!(
            condition("QUEUES orders LIMIT 5"),
            Condition {
                column: "queue".into(),
                operation: "CONTAINS",
                value: "orders".into()
            }
        );
        assert_eq!(condition("CONSUMERS /orders").operation, "EQUALS");
        assert_eq!(condition("ADDRESSES WHERE Queues > 1").column, "Queues");
        assert_eq!(
            condition("QUEUES WHERE queue NOT CONTAINS '$'").operation,
            "NOT_CONTAINS"
        );
        assert!(error("QUEUES WHERE messages > 0 AND consumers = 0").contains("one condition"));
        assert!(error("QUEUES orders WHERE messages > 0").contains("one condition"));
    }

    #[test]
    fn misplaced_clauses_say_where_they_belong() {
        assert_eq!(
            error("PEEK /orders"),
            "#action starts with a command: QUEUES, ADDRESSES, CONSUMERS, BROWSE, RECEIVE, SEND, PUBLISH, PURGE, MOVE, RETRY, PAUSE, RESUME, JOLOKIA"
        );
        assert_eq!(
            error("SEND /orders LIMIT 2"),
            "LIMIT applies to QUEUES, ADDRESSES, CONSUMERS, BROWSE or RECEIVE, not SEND"
        );
        assert_eq!(
            error("BROWSE /orders SET a = 'b'"),
            "SET applies to SEND or PUBLISH, not BROWSE"
        );
        assert!(
            error("SEND /orders SET priority = 5")
                .starts_with("SET values are text in quotes: priority = '5'")
        );
        assert_eq!(error("SEND /q SET a = 'x', a = 'y'"), "SET names a twice");
        assert_eq!(error("BROWSE"), "Add the queue: BROWSE /queue");
        assert_eq!(
            error("MOVE /DLQ"),
            "Add the queue to move to: MOVE /DLQ TO /orders"
        );
        assert_eq!(
            error("BROWSE /orders LIMIT 0"),
            "LIMIT is a number from 1 to 1000"
        );
        assert!(error("BROWSE /orders LIMIT 5 WHERE a = 'b'").starts_with("LIMIT goes last"));
        assert_eq!(error("BROWSE /q WHERE a = 'b"), "A quote is not closed: 'b");
    }
}
