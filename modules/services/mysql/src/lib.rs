//! The MySQL service: a `#target` connection string (`mysql://…`), an optional `#options`
//! section and a `#payload` sent through the Host `mysql` driver. Results print as the MySQL
//! client prints them; a query ending in `\G` prints vertical records. Schema discovery
//! feeds SQL completion with tables and columns.

use serde_json::{Map, Value, json};
use services_sdk::lang::check::At;
use services_sdk::lang::resolve::Resolver;
use services_sdk::lang::table::{self, Column, Format, Kind, Options};
use services_sdk::lang::{endpoint, finish, item, key_value, marker, sql};
use services_sdk::mysql::{self, Request, Response, ResultSet};
use services_sdk::run::{self, Answer, OneCall};
use services_sdk::{
    Check, Cmd, ColumnType, Completions, DriverCall, Event, Failure, Item, Line, LineColumns,
    Module, Rendered, Subscriptions, Token, cmd,
};
use std::collections::BTreeMap;

pub const SECTIONS: [&str; 3] = ["target", "options", "payload"];

pub const DISCOVERY_SQL: &str = "SELECT table_name AS tableName, column_name AS columnName, column_type AS columnType, column_key AS columnKey FROM information_schema.columns WHERE table_schema = DATABASE() ORDER BY table_name, ordinal_position";

const COLUMNS_TYPE: &str = "Columns-Type";
const COLUMNS_SIZE: &str = "Columns-Size";
const COLUMNS_TYPE_DETAIL: &str =
    "column=json column=datetime column=number column=boolean column=text";
const COLUMNS_SIZE_DETAIL: &str = "column=100 (characters)";
/// Schemas kept, the most recent last.
const SCHEMAS: usize = 16;
const DISCOVER: &str = "discover:";

/// The field of a driver request with how its answer prints (`vertical`, `columnsType`, `columnsSize`):
/// the parts of a check the driver does not take, and ignores.
pub const CONSOLE: &str = "console";

/// A parsed `#payload`: the SQL to send and whether `\G` asked for vertical records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub sql: String,
    pub vertical: bool,
}

/// A query; a final `\G` or `\g` (with an optional `;` on either side) asks for vertical
/// results and is not sent.
pub fn parse_sql(source: &str) -> Result<Query, String> {
    let mut sql = source.trim();
    if sql.is_empty() {
        return Err("SQL query is required".into());
    }
    let tail = sql.strip_suffix(';').unwrap_or(sql).trim_end();
    let marker = tail
        .strip_suffix("\\G")
        .or_else(|| tail.strip_suffix("\\g"));
    if let Some(query) = marker {
        let query = query.trim_end();
        sql = query.strip_suffix(';').unwrap_or(query).trim();
    }
    Ok(Query {
        sql: sql.into(),
        vertical: marker.is_some(),
    })
}

/// The `Columns-Type` of the `#options` section.
fn option(check: &Check, name: &str) -> Option<String> {
    key_value::pairs(check.text_of("options"))
        .into_iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}

/// The driver request of a check: `{url, sql, console: {vertical, columnsType, columnsSize}}`; `{{…}}`
/// placeholders stay for the App to resolve.
pub fn request(check: &Check) -> Result<Value, String> {
    let url = check.text_of("target").trim();
    if !url.to_ascii_lowercase().starts_with("mysql://") {
        return Err("MySQL request needs a #target block (a mysql:// connection string)".into());
    }
    let query = parse_sql(check.text_of("payload"))?;
    let types = option(check, COLUMNS_TYPE);
    let sizes = option(check, COLUMNS_SIZE);
    Options::parse(types.as_deref(), sizes.as_deref())?;
    let format = option(check, "Format");
    if let Some(format) = &format {
        Format::parse(format)?;
    }
    let mut value = serde_json::to_value(Request::new(url, &query.sql)).expect("a request");
    let mut console = json!({"vertical": query.vertical});
    if let Some(types) = types {
        console["columnsType"] = json!(types);
    }
    if let Some(sizes) = sizes {
        console["columnsSize"] = json!(sizes);
    }
    if let Some(format) = format {
        console["format"] = json!(format);
    }
    value[CONSOLE] = console;
    Ok(value)
}

/// The kind a column's protocol type prints as.
pub fn kind(column: &mysql::Column) -> Kind {
    match column.kind.as_str() {
        "tiny" if column.length == 1 => Kind::Boolean,
        "decimal" | "newdecimal" | "tiny" | "short" | "long" | "longlong" | "int24" | "float"
        | "double" | "year" => Kind::Number,
        "date" | "newdate" | "datetime" | "timestamp" | "time" => Kind::Datetime,
        "json" => Kind::Json,
        _ => Kind::Text,
    }
}

/// A cell as the console prints it: binary data as a Node.js `Buffer`.
fn cell(column: &mysql::Column, value: &Value) -> Value {
    match mysql::binary(value) {
        Some(bytes) if column.binary() => json!({"type": "Buffer", "data": bytes}),
        _ => value.clone(),
    }
}

/// A result set as a table: a statement without rows shows its `affectedRows`.
fn set_table(set: &ResultSet) -> (Vec<Column>, Vec<Vec<Value>>) {
    if set.columns.is_empty() {
        return (
            vec![Column::new("affectedRows", Kind::Number)],
            vec![vec![json!(set.summary.affected_rows)]],
        );
    }
    let columns = set
        .columns
        .iter()
        .map(|column| Column::new(&column.name, kind(column)))
        .collect();
    let rows = set
        .rows
        .iter()
        .map(|row| {
            set.columns
                .iter()
                .zip(row)
                .map(|(column, value)| cell(column, value))
                .collect()
        })
        .collect();
    (columns, rows)
}

/// A driver answer as text: every result set as a table, then the server's error.
pub fn render(call: Option<&Value>, response: &Value) -> Result<Rendered, String> {
    let response = Response::parse(response)?;
    let console = call.map_or(Value::Null, |call| call[CONSOLE].clone());
    let vertical = console["vertical"] == true;
    let options = Options::parse(
        console["columnsType"].as_str(),
        console["columnsSize"].as_str(),
    )?;
    let format = console["format"].as_str().map(Format::parse).transpose()?;
    let mut tables: Vec<_> = response.sets.iter().map(set_table).collect();
    for name in options.types.keys() {
        if !tables
            .iter()
            .any(|(columns, _)| columns.iter().any(|column| &column.name == name))
        {
            return Err(format!(
                "Column options refer to an unknown result column: {name}"
            ));
        }
    }
    for (columns, _) in &mut tables {
        for column in columns.iter_mut() {
            if let Some(kind) = options.types.get(&column.name) {
                column.kind = *kind;
            }
        }
    }
    let mut parts: Vec<String> = tables
        .iter()
        .zip(&response.sets)
        .map(|((columns, rows), set)| {
            let mut text = table::show(columns, rows, format, vertical);
            if set.summary.truncated {
                text.push_str(&format!(
                    "\n{} rows shown; the rest were dropped",
                    rows.len()
                ));
            }
            text
        })
        .collect();
    if parts.is_empty() && response.error.is_none() {
        parts.push(table::render(&[], &[], false));
    }
    let mut types: Vec<ColumnType> = Vec::new();
    for (columns, _) in &tables {
        for column in table::types(columns) {
            if !types.iter().any(|known| known.name == column.name) {
                types.push(column);
            }
        }
    }
    if let Some(error) = &response.error {
        parts.push(if error.state.is_empty() {
            format!("ERROR {}: {}", error.code, error.message)
        } else {
            format!("ERROR {} ({}): {}", error.code, error.state, error.message)
        });
    }
    Ok(Rendered {
        text: parts.join("\n\n"),
        language: "mysql".into(),
        outcome: if response.error.is_some() {
            "ERROR"
        } else {
            "OK"
        }
        .into(),
        columns: types,
    })
}

/// Discovery rows (`tableName`, `columnName`, `columnType`, `columnKey`) as the schema SQL
/// completion reads: `{"tables":[{"name","columns":[{"name","type","primaryKey"}]}]}`.
/// The run's status: the rows of the sets that have columns, like the MySQL client counts.
pub fn rows(response: &Value) -> Option<String> {
    let response = Response::parse(response).ok()?;
    let sets: Vec<_> = response
        .sets
        .iter()
        .filter(|set| !set.columns.is_empty())
        .collect();
    if sets.is_empty() {
        return None;
    }
    let n: usize = sets.iter().map(|set| set.rows.len()).sum();
    Some(format!("{n} {}", if n == 1 { "row" } else { "rows" }))
}

pub fn schema(response: &Response) -> Result<Value, String> {
    if let Some(error) = &response.error {
        return Err(error.message.clone());
    }
    let set = response
        .sets
        .first()
        .ok_or("Invalid MySQL discovery rows")?;
    let index = |name: &str| {
        set.columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("Invalid MySQL discovery column: {name}"))
    };
    let at = [
        index("tableName")?,
        index("columnName")?,
        index("columnType")?,
        index("columnKey")?,
    ];
    let mut tables: Map<String, Value> = Map::new();
    for row in &set.rows {
        let field = |i: usize| row.get(at[i]).and_then(Value::as_str).unwrap_or("");
        tables
            .entry(field(0).to_owned())
            .or_insert(json!({"name": field(0), "columns": []}))["columns"]
            .as_array_mut()
            .expect("columns")
            .push(json!({"name": field(1), "type": field(2), "primaryKey": field(3) == "PRI"}));
    }
    Ok(json!({"tables": tables.into_values().collect::<Vec<_>>()}))
}

/// Why an endpoint with variables is being resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Purpose {
    Remember,
    Hints,
    Manual,
}

#[derive(Debug, Default)]
pub struct Mysql {
    /// `(url, schema)` of discovered connections, the most recent last.
    schemas: Vec<(String, Value)>,
    /// URLs discovered or tried, so hints do not retry them.
    attempted: Vec<String>,
    /// The discovery in flight: its id and URL.
    run: Option<(u64, String)>,
    runs: u64,
    /// A URL hints asked for while another ran.
    pending: Option<String>,
    resolver: Resolver<Purpose>,
    resolving: BTreeMap<String, Check>,
    calls: OneCall,
}

const RESOLVE: &str = ":resolve";

/// A connection string discovery can use: `mysql://` with a host.
fn discoverable(url: &str) -> bool {
    url.get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("mysql://"))
        && url[8..].split(['/', '?']).next().is_some_and(|authority| {
            let host = authority.rsplit('@').next().unwrap_or("");
            !host.is_empty() && !host.ends_with(':')
        })
        && !url.contains(char::is_whitespace)
}

impl Mysql {
    /// The schema of the check's connection; `null` when not discovered.
    pub fn schema(&self, check: &Check) -> Value {
        let raw = check.text_of("target").trim();
        self.resolver
            .get(raw)
            .and_then(|url| self.schemas.iter().rev().find(|(known, _)| known == url))
            .map_or(Value::Null, |(_, schema)| schema.clone())
    }

    fn known(&self, url: &str) -> bool {
        self.attempted.iter().any(|tried| tried == url)
    }

    fn document(&mut self, check: &Check) -> Vec<Cmd> {
        let raw = check.text_of("target").trim();
        if raw.contains("{{") && self.resolver.get(raw).is_none() && !self.resolver.pending(raw) {
            return vec![self.resolver.request(raw, Purpose::Remember)];
        }
        vec![]
    }

    fn hints(&mut self, check: &Check) -> Vec<Cmd> {
        let raw = check.text_of("target").trim();
        if raw.contains("{{") {
            return vec![self.resolver.request(raw, Purpose::Hints)];
        }
        self.hinted(raw.to_owned())
    }

    fn hinted(&mut self, url: String) -> Vec<Cmd> {
        if !discoverable(&url) || self.known(&url) {
            return vec![];
        }
        match &self.run {
            Some((_, running)) if *running == url => vec![],
            Some(_) => {
                self.pending = Some(url);
                vec![]
            }
            None => self.start(url),
        }
    }

    fn manual(&mut self, check: &Check) -> Vec<Cmd> {
        let raw = check.text_of("target").trim();
        if raw.contains("{{") {
            return vec![self.resolver.request(raw, Purpose::Manual)];
        }
        self.discover(raw.to_owned())
    }

    fn discover(&mut self, url: String) -> Vec<Cmd> {
        if !discoverable(&url) {
            return vec![cmd::status(
                "Discovery · WARNING · Invalid or incomplete service address for discovery",
                "warning",
            )];
        }
        self.start(url)
    }

    fn start(&mut self, url: String) -> Vec<Cmd> {
        self.runs += 1;
        self.pending = None;
        let call = Request::new(&url, DISCOVERY_SQL).call();
        self.run = Some((self.runs, url));
        vec![
            cmd::status("Discovery…", "busy"),
            cmd::driver(format!("{DISCOVER}{}", self.runs), &call),
        ]
    }

    fn discovered(&mut self, id: u64, result: Result<Value, Failure>) -> Vec<Cmd> {
        let Some((_, url)) = self.run.take_if(|(run, _)| *run == id) else {
            return vec![];
        };
        self.attempted.retain(|tried| *tried != url);
        self.attempted.push(url.clone());
        let found = result
            .map_err(|failure| failure.message)
            .and_then(|value| schema(&Response::parse(&value)?));
        let mut cmds = match found {
            Ok(schema) => {
                let tables = schema["tables"].as_array().map_or(0, Vec::len);
                self.schemas.retain(|(known, _)| *known != url);
                self.schemas.push((url, schema));
                if self.schemas.len() > SCHEMAS {
                    self.schemas.remove(0);
                }
                vec![
                    cmd::status_with("Discovery · OK", "ok", &json!({"tables": tables})),
                    cmd::log("info", &format!("MySQL discovery found {tables} tables")),
                ]
            }
            Err(message) => vec![
                cmd::status(&format!("Discovery · ERROR · {message}"), "error"),
                cmd::log("error", &format!("MySQL discovery failed: {message}")),
            ],
        };
        if let Some(url) = self.pending.take() {
            cmds.extend(self.hinted(url));
        }
        cmds
    }

    fn run(&mut self, run: String, check: Check) -> Vec<Cmd> {
        if let Err(error) = request(&check) {
            return vec![cmd::fail(&run, &error, &Value::Null)];
        }
        let sections = json!({"target": check.text_of("target"),
            "options": check.text_of("options"), "payload": check.text_of("payload")});
        self.resolving.insert(run.clone(), check);
        vec![run::resolve_value(format!("{run}{RESOLVE}"), &sections)]
    }

    fn resolved(&mut self, run: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        let Some(check) = self.resolving.remove(run) else {
            return vec![];
        };
        let request = run::resolved_value(result)
            .and_then(|sections| request(&run::with_sections(&check, &sections)));
        match request {
            Ok(request) => vec![
                self.calls
                    .call(run, DriverCall::new(mysql::DRIVER, &request)),
            ],
            Err(error) => vec![cmd::fail(run, &error, &Value::Null)],
        }
    }

    fn reply(&mut self, tag: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        if let Some(run) = tag.strip_suffix(RESOLVE) {
            return self.resolved(run, result);
        }
        if let Some(cmds) = self.calls.reply(tag, result.clone(), |call, response| {
            Ok(Answer {
                rendered: render(Some(&call.request()), response)?,
                status: rows(response),
            })
        }) {
            return cmds;
        }
        if let Some(id) = tag.strip_prefix(DISCOVER) {
            return id
                .parse()
                .map_or_else(|_| vec![], |id| self.discovered(id, result));
        }
        let Some(resolved) = self.resolver.reply(tag, result) else {
            return vec![];
        };
        match (resolved.result, resolved.purpose) {
            (_, Purpose::Remember) => vec![],
            (Ok(url), Purpose::Hints) => self.hinted(url),
            (Ok(url), Purpose::Manual) => self.discover(url),
            (Err(message), purpose) => {
                let message = format!(
                    "Variables of {} could not be resolved: {message}",
                    resolved.raw
                );
                let mut cmds = vec![cmd::log("warning", &message)];
                if purpose == Purpose::Manual {
                    cmds.insert(
                        0,
                        cmd::status(&format!("Discovery · ERROR · {message}"), "error"),
                    );
                }
                cmds
            }
        }
    }
}

/// The part of a check a line is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Area {
    /// Before the first marker, or a rendered response highlighted alone: table text.
    Prelude,
    Endpoint,
    Options,
    Query,
    Other,
    Response,
}

impl Area {
    const ALL: [Area; 6] = [
        Area::Prelude,
        Area::Endpoint,
        Area::Options,
        Area::Query,
        Area::Other,
        Area::Response,
    ];
}

fn whole(line: &str, kind: &str) -> Vec<Token> {
    if line.is_empty() {
        return vec![];
    }
    vec![Token {
        start: 0,
        len: line.len() as u32,
        kind: kind.into(),
    }]
}

/// Highlighting of a MySQL check: SQL, the endpoint, options and the result tables.
pub fn highlight(state: &[u8], lines: &[String], columns: &[LineColumns]) -> Vec<Line> {
    let (mut area, rest) = match state.split_first() {
        Some((area, rest)) => (
            Area::ALL
                .get(*area as usize)
                .copied()
                .unwrap_or(Area::Other),
            rest,
        ),
        None => (Area::Prelude, &[][..]),
    };
    let (mut response, rest) = match rest {
        [] => (
            table::State {
                guess_json: true,
                ..table::State::default()
            },
            rest,
        ),
        _ => table::State::read(rest),
    };
    let mut query = sql::State::decode(rest);
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let named = marker(line).map(|(name, _)| name);
            let is_marker = named.as_deref().is_some_and(|name| {
                area != Area::Response || matches!(name, "responce" | "response")
            });
            let tokens = if is_marker {
                area = match named.as_deref() {
                    Some("target") => Area::Endpoint,
                    Some("options") => Area::Options,
                    Some("payload") => Area::Query,
                    Some("responce" | "response") => Area::Response,
                    _ => Area::Other,
                };
                query = sql::State::default();
                response = response.restart();
                if let Some(reported) = columns.iter().find(|at| at.line == index as u32) {
                    response.report(&reported.columns);
                }
                whole(line, "comment")
            } else {
                match area {
                    Area::Endpoint => endpoint::tokens(line, false),
                    Area::Options => {
                        response.declare(line);
                        key_value::tokens(line)
                    }
                    Area::Query => sql::tokens(line, &mut query),
                    Area::Prelude | Area::Response => table::tokens(line, &mut response),
                    Area::Other => vec![],
                }
            };
            let mut state = vec![area as u8];
            response.encode(&mut state);
            query.encode(&mut state);
            Line { tokens, state }
        })
        .collect()
}

fn sql_kind(detail: &str) -> &'static str {
    match detail {
        "SQL" => "keyword",
        "Table" => "table",
        _ => "column",
    }
}

/// Completion in a MySQL check: markers, the `mysql://` scheme, `Columns-Type` and SQL
/// against the discovered schema.
pub fn complete(check: &Check, pos: usize, schema: &Value) -> Completions {
    let at = At::new(&check.text, pos);
    if at.in_placeholder() || at.in_response() {
        return at.none();
    }
    if let Some(markers) = at.markers(&SECTIONS) {
        return markers;
    }
    let line_start = at.line_start(at.line);
    match at.name() {
        Some("target") => {
            if let Some(next) = at.next_section(&SECTIONS) {
                return next;
            }
            let value = at.before.trim_start();
            let from = line_start + at.before.len() - value.len();
            finish(
                from,
                at.pos,
                value,
                vec![item("mysql://", "Connection string", "url")],
            )
        }
        Some("options") => {
            let used = key_value::used(at.other_lines());
            let items: Vec<Item> = [
                item(format!("{COLUMNS_TYPE}: "), COLUMNS_TYPE_DETAIL, "header"),
                item(format!("{COLUMNS_SIZE}: "), COLUMNS_SIZE_DETAIL, "header"),
                item("Format: json", "Header", "header"),
                item("Format: table", "Header", "header"),
            ]
            .into_iter()
            .filter(|item| !used.contains(&key_value::name(&item.insert)))
            .collect();
            let mut result = finish(line_start, at.pos, at.before.trim(), items);
            if at.before.trim().is_empty() && !used.is_empty() {
                let mut items = finish(line_start, at.pos, "", at.unused(&SECTIONS)).items;
                items.append(&mut result.items);
                result.items = items;
            }
            result
        }
        Some("payload") => {
            let (fragment, candidates) = sql::suggest(at.body(), at.whole(), schema);
            let items = candidates
                .into_iter()
                .map(|(text, detail)| {
                    let kind = sql_kind(&detail);
                    item(text, &detail, kind)
                })
                .collect();
            finish(at.pos - fragment.len(), at.pos, &fragment, items)
        }
        _ => at.none(),
    }
}

impl Module for Mysql {
    type Msg = ();

    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Document(check) => self.document(&check),
            Event::Hints { check, .. } => self.hints(&check),
            Event::Discover(check) => self.manual(&check),
            Event::Reply { tag, result } => self.reply(&tag, result),
            Event::Run { tag, check } => self.run(tag, check),
            Event::Cancel { tag } => {
                self.resolving.remove(&tag);
                self.calls.cancel(&tag);
                vec![]
            }
            _ => vec![],
        }
    }

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            document: true,
            hints: true,
            discover: true,
            run: true,
            ..Subscriptions::default()
        }
    }

    fn highlight(&self, state: &[u8], lines: &[String], columns: &[LineColumns]) -> Vec<Line> {
        highlight(state, lines, columns)
    }

    fn fold(&self, check: &Check) -> Vec<services_sdk::Fold> {
        services_sdk::lang::table::folds(&check.text, &check.columns)
    }

    fn complete(&self, check: &Check, pos: u32) -> Completions {
        complete(check, pos as usize, &self.schema(check))
    }

    fn schema(&self, check: &Check) -> Option<Value> {
        Some(Mysql::schema(self, check)).filter(|schema| !schema.is_null())
    }
}

services_sdk::export!(Mysql);
