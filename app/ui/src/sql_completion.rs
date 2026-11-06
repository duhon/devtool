use serde_json::Value;

type Candidates = Vec<(String, String)>;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Word,
    Identifier,
    Value,
    Symbol,
}

struct Token {
    kind: Kind,
    text: String,
    start: usize,
    end: usize,
    depth: usize,
    closed: bool,
}

impl Token {
    fn is(&self, text: &str) -> bool {
        matches!(self.kind, Kind::Word | Kind::Symbol) && self.text.eq_ignore_ascii_case(text)
    }
    fn name(&self) -> bool {
        matches!(self.kind, Kind::Word | Kind::Identifier)
    }
}

struct Lexed {
    tokens: Vec<Token>,
    depth: usize,
    blocked: bool,
    opens: Vec<usize>,
}

fn lex(source: &str) -> Lexed {
    let mut tokens = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    let mut blocked = false;
    let mut opens = Vec::new();
    while i < source.len() {
        let c = source[i..].chars().next().unwrap();
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        let rest = &source[i..];
        if rest.starts_with('#')
            || rest.starts_with("--") && rest[2..].chars().next().is_none_or(char::is_whitespace)
        {
            if let Some(end) = rest.find('\n') {
                i += end + 1;
                continue;
            }
            blocked = true;
            break;
        }
        if let Some(comment) = rest.strip_prefix("/*") {
            if let Some(end) = comment.find("*/") {
                i += end + 4;
                continue;
            }
            blocked = true;
            break;
        }
        let start = i;
        let kind;
        let text;
        let mut closed = true;
        if matches!(c, '\'' | '"' | '`') {
            let quote = c;
            i += 1;
            let mut value = String::new();
            closed = false;
            while i < source.len() {
                let next = source[i..].chars().next().unwrap();
                i += next.len_utf8();
                if next == '\\' && i < source.len() {
                    let escaped = source[i..].chars().next().unwrap();
                    value.push(escaped);
                    i += escaped.len_utf8();
                } else if next == quote {
                    if source[i..].starts_with(quote) {
                        value.push(quote);
                        i += 1;
                    } else {
                        closed = true;
                        break;
                    }
                } else {
                    value.push(next);
                }
            }
            if quote != '`' && !closed {
                blocked = true;
                break;
            }
            kind = if quote == '`' {
                Kind::Identifier
            } else {
                Kind::Value
            };
            text = value;
        } else if rest.starts_with("{{") {
            if let Some(end) = rest.find("}}") {
                i += end + 2;
                kind = Kind::Value;
                text = source[start..i].into();
            } else {
                blocked = true;
                break;
            }
        } else if c.is_alphanumeric() || c == '_' || c == '$' {
            i += c.len_utf8();
            while let Some(next) = source[i..].chars().next() {
                if !(next.is_alphanumeric() || matches!(next, '_' | '$')) {
                    break;
                }
                i += next.len_utf8();
            }
            kind = if c.is_ascii_digit() {
                Kind::Value
            } else {
                Kind::Word
            };
            text = source[start..i].into();
        } else {
            i += c.len_utf8();
            if ["<=", ">=", "<>", "!=", "||", "&&", ":="]
                .iter()
                .any(|operator| source[start..].starts_with(operator))
            {
                i += 1;
            }
            kind = Kind::Symbol;
            text = source[start..i].into();
        }
        if text == ")" && kind == Kind::Symbol {
            depth = depth.saturating_sub(1);
            opens.pop();
        }
        tokens.push(Token {
            kind,
            text: text.clone(),
            start,
            end: i,
            depth,
            closed,
        });
        if text == "(" && kind == Kind::Symbol {
            opens.push(start);
            depth += 1;
        }
    }
    Lexed {
        tokens,
        depth,
        blocked,
        opens,
    }
}

#[derive(Clone, Copy)]
enum Clause {
    Projection,
    Filter,
    On,
    Group,
    Order,
    Assignment,
    Values,
}

#[derive(Clone, Copy)]
enum Position {
    Command,
    Operand(Clause),
    AfterOperand(Clause),
    Table(bool),
    AfterTable(bool),
    Alias(bool),
    AfterAlias(bool),
    ProjectionAlias,
    ProjectionDone,
    By(Clause),
    Direction,
    Limit,
    AfterLimit,
    Offset,
    AfterOffset,
    InsertInto,
    SetName,
    SetEquals,
    DeleteFrom,
    JoinModifier,
    Show,
    Describe,
    Union,
}

fn advance(position: Position, token: &Token) -> Position {
    use Clause::*;
    use Position::*;
    if token.kind == Kind::Word {
        match token.text.to_ascii_uppercase().as_str() {
            "SELECT" => return Operand(Projection),
            "INSERT" => return InsertInto,
            "UPDATE" => return Table(false),
            "DELETE" => return DeleteFrom,
            "SHOW" => return Show,
            "DESCRIBE" | "DESC" if matches!(position, Command) => return Describe,
            "EXPLAIN" => return Command,
            "FROM"
                if matches!(
                    position,
                    Operand(Projection) | AfterOperand(Projection) | ProjectionDone | DeleteFrom
                ) =>
            {
                return Table(false);
            }
            "INTO" if matches!(position, InsertInto) => return Table(false),
            "JOIN" | "STRAIGHT_JOIN" => return Table(true),
            "LEFT" | "RIGHT" | "INNER" | "CROSS" | "NATURAL"
                if matches!(position, AfterTable(_) | AfterAlias(_) | AfterOperand(On)) =>
            {
                return JoinModifier;
            }
            "OUTER" if matches!(position, JoinModifier) => return JoinModifier,
            "WHERE" | "HAVING" => return Operand(Filter),
            "ON" | "USING" => return Operand(On),
            "GROUP" => return By(Group),
            "ORDER" => return By(Order),
            "BY" if matches!(position, By(_)) => {
                if let By(clause) = position {
                    return Operand(clause);
                }
            }
            "LIMIT" => return Limit,
            "OFFSET" => return Offset,
            "ASC" | "DESC" if matches!(position, AfterOperand(Order)) => return Direction,
            "SET" => return SetName,
            "VALUES" | "VALUE" => return Operand(Values),
            "UNION" => return Union,
            "ALL" | "DISTINCT" if matches!(position, Union) => return Union,
            "AS" => {
                return match position {
                    AfterTable(join) | AfterAlias(join) => Alias(join),
                    _ => ProjectionAlias,
                };
            }
            "AND" | "OR" | "XOR" => {
                return match position {
                    Operand(clause) | AfterOperand(clause) => Operand(clause),
                    _ => position,
                };
            }
            "NOT" | "DISTINCT" | "ALL" if matches!(position, Operand(_)) => return position,
            "IN" | "LIKE" | "REGEXP" | "BETWEEN" | "IS" => {
                return match position {
                    AfterOperand(clause) => Operand(clause),
                    _ => position,
                };
            }
            _ => {}
        }
    }
    match position {
        Table(join) if token.name() || token.is(")") => AfterTable(join),
        AfterTable(join) if token.is(".") => Table(join),
        AfterTable(join) if token.name() => AfterAlias(join),
        Alias(join) if token.name() => AfterAlias(join),
        AfterTable(_) | AfterAlias(_) if token.is(",") => Table(false),
        Operand(clause)
            if token.is(")") || token.name() || token.kind == Kind::Value || token.is("*") =>
        {
            AfterOperand(clause)
        }
        AfterOperand(clause) if token.is(".") || token.is("(") => Operand(clause),
        Operand(_) if token.is("(") => position,
        AfterOperand(Projection) if token.is(",") => Operand(Projection),
        ProjectionDone if token.is(",") => Operand(Projection),
        AfterOperand(Group) if token.is(",") => Operand(Group),
        AfterOperand(Order) | Direction if token.is(",") => Operand(Order),
        AfterOperand(Assignment) if token.is(",") => SetName,
        AfterOperand(Values) if token.is(",") => Operand(Values),
        AfterOperand(clause)
            if matches!(
                token.text.as_str(),
                "=" | "!=" | "<>" | "<" | ">" | "<=" | ">=" | "+" | "-" | "*" | "/" | "%"
            ) =>
        {
            Operand(clause)
        }
        ProjectionAlias if token.name() => ProjectionDone,
        SetName if token.name() => SetEquals,
        SetEquals if token.is("=") => Operand(Assignment),
        Limit if token.kind == Kind::Value => AfterLimit,
        AfterLimit if token.is(",") => Offset,
        Offset if token.kind == Kind::Value => AfterOffset,
        _ => position,
    }
}

fn reserved(token: &Token) -> bool {
    token.kind == Kind::Word
        && [
            "WHERE",
            "JOIN",
            "LEFT",
            "RIGHT",
            "INNER",
            "OUTER",
            "CROSS",
            "NATURAL",
            "ON",
            "USING",
            "ORDER",
            "GROUP",
            "LIMIT",
            "OFFSET",
            "HAVING",
            "SET",
            "VALUES",
            "UNION",
            "FOR",
            "AS",
            "ASC",
            "DESC",
            "STRAIGHT_JOIN",
            "SELECT",
            "INSERT",
            "UPDATE",
            "DELETE",
        ]
        .iter()
        .any(|word| token.is(word))
}

fn references(tokens: &[&Token]) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let mut table_list = false;
    for (i, token) in tokens.iter().enumerate() {
        let introduces_table = ["FROM", "JOIN", "UPDATE", "INTO"]
            .iter()
            .any(|word| token.is(word));
        if reserved(token) && !token.is("JOIN") && !token.is("AS") {
            table_list = false;
        }
        if !(introduces_table || table_list && token.is(",")) {
            continue;
        }
        table_list = true;
        let Some(name) = tokens.get(i + 1).filter(|token| token.name()) else {
            continue;
        };
        let mut table = name.text.clone();
        let mut next = i + 2;
        if tokens.get(next).is_some_and(|token| token.is("."))
            && let Some(name) = tokens.get(next + 1).filter(|token| token.name())
        {
            table.push('.');
            table.push_str(&name.text);
            next += 2;
        }
        if tokens.get(next).is_some_and(|token| token.is("AS")) {
            next += 1;
        }
        let alias = tokens
            .get(next)
            .filter(|token| token.name() && !reserved(token))
            .map(|token| token.text.clone())
            .unwrap_or_else(|| table.rsplit('.').next().unwrap_or(&table).to_owned());
        result.push((table, alias));
    }
    result
}

pub(super) fn suggest(body: &str, whole: &str, discovery: &Value) -> (String, Candidates) {
    let mut parsed = lex(body);
    if parsed.blocked {
        return (String::new(), vec![]);
    }
    let fragment = if parsed.tokens.last().is_some_and(|token| {
        token.end == body.len()
            && (token.kind == Kind::Word || token.kind == Kind::Identifier && !token.closed)
    }) {
        let token = parsed.tokens.pop().unwrap();
        body[token.start..].to_owned()
    } else {
        String::new()
    };
    let statement_start = parsed
        .tokens
        .iter()
        .rev()
        .find(|token| token.is(";") && token.depth == 0)
        .map(|token| token.end)
        .unwrap_or(0);
    let tokens: Vec<_> = parsed
        .tokens
        .iter()
        .filter(|token| token.start >= statement_start)
        .collect();
    if tokens.is_empty() && statement_start > 0 && fragment.is_empty() {
        return (fragment, vec![]);
    }
    let depth = tokens
        .iter()
        .rfind(|token| {
            token.depth <= parsed.depth
                && ["SELECT", "UPDATE", "INSERT", "DELETE"]
                    .iter()
                    .any(|word| token.is(word))
        })
        .map(|token| token.depth)
        .unwrap_or(0);
    let container_start = if depth > 0 {
        parsed
            .opens
            .get(depth - 1)
            .map(|start| start + 1)
            .unwrap_or(statement_start)
    } else {
        statement_start
    };
    let scope_start = tokens
        .iter()
        .rev()
        .find(|token| {
            token.depth == depth
                && token.start >= container_start
                && ["SELECT", "UPDATE", "INSERT", "DELETE"]
                    .iter()
                    .any(|word| token.is(word))
        })
        .map(|token| token.start)
        .unwrap_or(container_start);
    let scope: Vec<_> = tokens
        .iter()
        .copied()
        .filter(|token| token.depth == depth && token.start >= scope_start)
        .collect();
    let mut position = Position::Command;
    for token in &scope {
        position = advance(position, token);
    }
    let qualifier = tokens
        .last()
        .filter(|token| token.is("."))
        .and_then(|_| tokens.iter().rev().nth(1))
        .filter(|token| token.name())
        .map(|token| token.text.as_str());
    let quoted = fragment.starts_with('`');
    let language_only =
        !quoted && qualifier.is_none() && fragment.chars().next().is_some_and(char::is_uppercase);
    let whole = lex(whole);
    let statement_end = whole
        .tokens
        .iter()
        .find(|token| token.start >= body.len() && token.is(";") && token.depth == 0)
        .map(|token| token.start)
        .unwrap_or(usize::MAX);
    let scope_end = whole
        .tokens
        .iter()
        .find(|token| {
            token.start >= body.len()
                && (token.is(")") && token.depth < depth
                    || token.is("UNION") && token.depth == depth)
        })
        .map(|token| token.start)
        .unwrap_or(statement_end)
        .min(statement_end);
    let whole_scope: Vec<_> = whole
        .tokens
        .iter()
        .filter(|token| {
            token.start >= scope_start && token.start < scope_end && token.depth == depth
        })
        .collect();
    let refs = references(&whole_scope);
    use Clause::*;
    use Position::*;
    let statement = scope
        .first()
        .map(|token| token.text.to_ascii_uppercase())
        .unwrap_or_default();
    let tail: &[&str] = match statement.as_str() {
        "UPDATE" => &["JOIN", "LEFT JOIN", "RIGHT JOIN", "SET"],
        "INSERT" => &["VALUES", "SELECT", "SET"],
        "DELETE" => &[
            "JOIN",
            "LEFT JOIN",
            "RIGHT JOIN",
            "WHERE",
            "ORDER BY",
            "LIMIT",
        ],
        _ => &[
            "JOIN",
            "INNER JOIN",
            "LEFT JOIN",
            "RIGHT JOIN",
            "CROSS JOIN",
            "WHERE",
            "GROUP BY",
            "HAVING",
            "ORDER BY",
            "LIMIT",
            "UNION",
        ],
    };
    let expressions = [
        "COUNT(",
        "SUM(",
        "AVG(",
        "MIN(",
        "MAX(",
        "COALESCE(",
        "CONCAT(",
        "NOW()",
        "CASE",
        "NULL",
        "TRUE",
        "FALSE",
    ];
    let keywords: Vec<&str> = match position {
        Command => vec![
            "SELECT",
            "INSERT INTO",
            "UPDATE",
            "DELETE FROM",
            "SHOW",
            "EXPLAIN",
            "DESCRIBE",
        ],
        Operand(Projection) => std::iter::once("*")
            .chain(["DISTINCT"])
            .chain(expressions)
            .collect(),
        Operand(Filter | On) => ["NOT", "EXISTS"].into_iter().chain(expressions).collect(),
        Operand(Group | Order | Assignment | Values) => expressions.to_vec(),
        AfterOperand(Projection) => {
            if scope.last().is_some_and(|token| token.is("*")) {
                vec!["FROM"]
            } else {
                vec![",", "FROM", "AS", "+", "-", "*", "/"]
            }
        }
        ProjectionDone => vec![",", "FROM"],
        AfterOperand(Filter) => vec![
            "=",
            "!=",
            "<>",
            "<",
            ">",
            "<=",
            ">=",
            "LIKE",
            "NOT LIKE",
            "IN",
            "NOT IN",
            "BETWEEN",
            "IS NULL",
            "IS NOT NULL",
            "AND",
            "OR",
            "GROUP BY",
            "HAVING",
            "ORDER BY",
            "LIMIT",
            "UNION",
        ],
        AfterOperand(On) => vec![
            "=",
            "!=",
            "<>",
            "<",
            ">",
            "<=",
            ">=",
            "LIKE",
            "IN",
            "IS NULL",
            "IS NOT NULL",
            "AND",
            "OR",
            "JOIN",
            "LEFT JOIN",
            "RIGHT JOIN",
            "WHERE",
            "GROUP BY",
            "ORDER BY",
            "LIMIT",
        ],
        AfterOperand(Group) => vec![",", "HAVING", "ORDER BY", "LIMIT"],
        AfterOperand(Order) => vec![",", "ASC", "DESC", "LIMIT"],
        Direction => vec![",", "LIMIT"],
        AfterOperand(Assignment) => vec![",", "WHERE", "ORDER BY", "LIMIT"],
        AfterOperand(Values) => vec![","],
        SetEquals => vec!["="],
        Table(_) | Alias(_) | ProjectionAlias | SetName | Limit | Offset | AfterOffset => vec![],
        AfterTable(true) => vec![
            "ON",
            "USING",
            "AS",
            "JOIN",
            "LEFT JOIN",
            "RIGHT JOIN",
            "WHERE",
            "GROUP BY",
            "ORDER BY",
            "LIMIT",
        ],
        AfterAlias(true) => vec![
            "ON",
            "USING",
            "JOIN",
            "LEFT JOIN",
            "RIGHT JOIN",
            "WHERE",
            "GROUP BY",
            "ORDER BY",
            "LIMIT",
        ],
        AfterTable(false) if statement == "INSERT" => vec!["VALUES", "SELECT", "SET"],
        AfterTable(false) => std::iter::once("AS")
            .chain(tail.iter().copied())
            .chain([","])
            .collect(),
        AfterAlias(false) => tail.iter().copied().chain([","]).collect(),
        By(_) => vec!["BY"],
        AfterLimit => vec![",", "OFFSET"],
        InsertInto => vec!["INTO"],
        DeleteFrom => vec!["FROM"],
        JoinModifier => vec!["JOIN", "OUTER JOIN"],
        Show => vec!["TABLES", "DATABASES", "COLUMNS", "CREATE TABLE"],
        Describe => vec![],
        Union => vec!["SELECT", "ALL", "DISTINCT"],
    };
    let search = fragment.to_ascii_uppercase();
    let mut candidates: Candidates = keywords
        .into_iter()
        .filter(|keyword| !keyword.eq_ignore_ascii_case(&fragment))
        .filter(|keyword| keyword.starts_with(&search))
        .filter(|keyword| *keyword != "DISTINCT" || !scope.iter().any(|token| token.is("DISTINCT")))
        .map(|keyword| (keyword.into(), "SQL".into()))
        .collect();
    if language_only {
        return (fragment, candidates);
    }
    let tables = matches!(position, Table(_) | Describe);
    let columns = matches!(position, Operand(_) | SetName) || qualifier.is_some();
    let identifier = |name: &str| {
        if quoted {
            format!("`{}`", name.replace('`', "``"))
        } else {
            name.to_owned()
        }
    };
    let mut column_groups = vec![];
    for table in discovery["tables"].as_array().into_iter().flatten() {
        let name = table["name"].as_str().unwrap_or("");
        if tables {
            candidates.push((identifier(name), "Table".into()));
        } else if columns {
            let reference = refs.iter().find(|(table, _)| {
                table.eq_ignore_ascii_case(name)
                    || table
                        .rsplit('.')
                        .next()
                        .is_some_and(|last| last.eq_ignore_ascii_case(name))
            });
            let alias = reference.map(|(_, alias)| alias.as_str()).unwrap_or(name);
            if qualifier
                .is_some_and(|q| !q.eq_ignore_ascii_case(name) && !q.eq_ignore_ascii_case(alias))
                || !refs.is_empty() && reference.is_none()
            {
                continue;
            }
            let mut table_columns = vec![];
            for column in table["columns"].as_array().into_iter().flatten() {
                if let Some(column) = column.as_str().or(column["name"].as_str()) {
                    let text = if refs.len() > 1 && qualifier.is_none() {
                        format!("{}.{}", identifier(alias), identifier(column))
                    } else {
                        identifier(column)
                    };
                    table_columns.push((text, name.into()));
                }
            }
            column_groups.push(table_columns.into_iter());
        }
    }
    loop {
        let before = candidates.len();
        for columns in &mut column_groups {
            candidates.extend(columns.next());
        }
        if candidates.len() == before {
            break;
        }
    }
    if qualifier.is_some() {
        candidates.retain(|(_, detail)| detail != "SQL");
    }
    if quoted {
        candidates.retain(|(_, detail)| detail != "SQL");
    }
    (fragment, candidates)
}
