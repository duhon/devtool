use serde_json::{Value, json};
use services_ui::{console, format};

#[test]
fn original_javascript_parser_and_formatter_fixtures() {
    let fixtures: Vec<Value> =
        serde_json::from_str(include_str!("reference-fixtures.json")).unwrap();
    let mut checked = 0;
    for fixture in fixtures {
        let name = fixture["function"].as_str().unwrap();
        let args = fixture["args"].as_array().unwrap();
        let text = |index: usize| args.get(index).and_then(Value::as_str).unwrap_or("");
        let value: Result<Value, String> = match name {
            "parseSql" => services_module_mysql_ui::parse_sql(text(0), text(1)),
            "parseMysqlRequest" => console::parse("mysql", text(0)),
            "parseHttp" => console::parse("rest", text(0)),
            "parseGraphqlRequest" => console::parse("graphql", text(0)),
            "parseGrpcRequest" => console::parse("grpc", text(0)),
            "parseSplunkRequest" => console::parse("splunk", text(0)),
            "parseTerminalRequest" => console::parse("terminal", text(0)),
            "parseConsoleRequest" => console::parse(text(0), text(1)),
            "parseCurlCommand" => services_module_ui::command::parse_curl(text(0)),
            "parseGrpcurlCommand" => services_module_ui::command::parse_grpcurl(text(0)),
            "importCurlAsRestBlock" => console::import("rest", text(0)).map(Value::String),
            "importCurlAsGraphqlBlock" => console::import("graphql", text(0)).map(Value::String),
            "importGrpcurlAsGrpcBlock" => console::import("grpc", text(0)).map(Value::String),
            "importRequestBlock" => console::import(text(0), text(1)).map(Value::String),
            "exportRequestBlock" => console::export(text(0), text(1)).map(Value::String),
            "formatMysqlTable" => Ok(json!(format::table(&args[0], false))),
            "formatMysqlVertical" => Ok(json!(format::table(&args[0], true))),
            "formatConsoleResponse" => Ok(json!(format::console_response(
                text(0),
                &args[1],
                args.get(2).is_some_and(|v| v["vertical"] == true)
            ))),
            _ => continue,
        };
        checked += 1;
        if fixture.get("error").is_some() {
            assert!(
                value.is_err(),
                "{name}: expected validation error for {:?}",
                args
            );
        } else {
            assert_eq!(
                value.unwrap_or_else(|e| panic!("{name}: {e}")),
                fixture["result"],
                "{name}: {:?}",
                args
            );
        }
    }
    assert!(
        checked >= 60,
        "Expected broad coverage of the original parser/formatter tests"
    );
}
