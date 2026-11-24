use serde_json::{Value, json};
use services_module_prometheus::{
    render::{answer, rows},
    request::{Plan, sections},
};
use services_sdk::{Check, Section, lang::table::Kind};

fn plan(options: &str) -> Plan {
    let check = Check {
        sections: [
            ("target", "https://example.invalid"),
            ("payload", "up"),
            ("options", options),
        ]
        .into_iter()
        .map(|(name, text)| Section {
            name: name.into(),
            text: text.into(),
            ..Section::default()
        })
        .collect(),
        ..Check::default()
    };
    Plan::parse(&sections(&check).unwrap(), 0.0).unwrap()
}
fn envelope(kind: &str, result: Value) -> Value {
    json!({"status":"success","data":{"resultType":kind,"result":result}})
}
fn response(status: u16, envelope: &Value) -> Value {
    json!({"status":status,"body":envelope.to_string()})
}

#[test]
fn all_sample_shapes_preserve_labels_and_special_string_values() {
    let vector = envelope(
        "vector",
        json!([
            {"metric":{"__name__":"a","label":"x|y\n☃"},"value":[1700000000.125,"NaN"]},
            {"metric":{},"value":[1,"+Inf"]}, {"metric":{},"value":[2,"-Inf"]}
        ]),
    );
    let (columns, samples) = rows(&vector).unwrap();
    assert_eq!(columns[0].kind, Kind::Datetime);
    assert_eq!(columns[1].kind, Kind::Json);
    assert_eq!(columns[2].kind, Kind::Number);
    assert_eq!(samples[0][0], json!(1700000000.125));
    assert_eq!(samples[0][1]["label"], "x|y\n☃");
    assert_eq!(
        samples
            .iter()
            .map(|r| r[2].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["NaN", "+Inf", "-Inf"]
    );
    let matrix = envelope(
        "matrix",
        json!([{"metric":{"job":"a"},"values":[[1,"1.000"],[2,"-0"]]}]),
    );
    assert_eq!(rows(&matrix).unwrap().1.len(), 2);
    for kind in ["scalar", "string"] {
        let e = envelope(kind, json!([1.25, "value\twith\ntext"]));
        assert_eq!(rows(&e).unwrap().1[0][2], "value\twith\ntext");
        assert_eq!(
            rows(&e).unwrap().0[2].kind,
            if kind == "string" {
                Kind::Text
            } else {
                Kind::Number
            }
        );
    }
}

#[test]
fn mixed_native_histograms_are_explicit_and_complete() {
    let histogram = json!({"count":"3","sum":"+Inf","buckets":[[0,"-Inf","1","2"],[1,"1","+Inf","1"]],"extra":{"future":"retained"}});
    let vector = envelope(
        "vector",
        json!([
            {"metric":{"job":"a"},"histogram":[1,histogram]},
            {"metric":{"job":"b"},"value":[2,"42"]}
        ]),
    );
    let (columns, samples) = rows(&vector).unwrap();
    assert_eq!(columns[3].name, "histogram");
    assert_eq!(samples[0][3], histogram);
    assert!(samples[0][2].is_null());
    assert!(samples[1][3].is_null());
    let shown = answer(&plan(""), &response(200, &vector)).unwrap();
    assert!(shown.rendered.text.contains("future"));
    let matrix = envelope(
        "matrix",
        json!([{"metric":{},"values":[[1,"2"]],"histograms":[[2,histogram],[3,histogram]]}]),
    );
    assert_eq!(rows(&matrix).unwrap().1.len(), 3);
}

#[test]
fn notices_empty_success_and_json_full_envelope() {
    let mut e = envelope("vector", json!([]));
    e["warnings"] = json!(["partial response"]);
    e["infos"] = json!(["query annotation"]);
    e["extra"] = json!({"retained":true});
    let shown = answer(&plan(""), &response(200, &e)).unwrap();
    assert_eq!(shown.rendered.outcome, "WARNING");
    assert!(shown.rendered.text.contains("Warning: partial response"));
    assert!(shown.rendered.text.contains("Info: query annotation"));
    assert!(shown.status.unwrap().starts_with("0 samples"));
    let full = answer(&plan("Format: json"), &response(200, &e)).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&full.rendered.text).unwrap(),
        e
    );
    let empty = answer(&plan(""), &response(200, &envelope("matrix", json!([])))).unwrap();
    assert_eq!(empty.rendered.outcome, "OK");
}

#[test]
fn status_errors_and_malformed_results_are_explicit() {
    let e = json!({"status":"error","errorType":"bad_data","error":"invalid query","warnings":["detail"]});
    for status in [200, 400, 500] {
        let error = answer(&plan(""), &response(status, &e)).err().unwrap();
        assert!(error.contains(&format!("HTTP {status}")));
        assert!(error.contains("bad_data"));
        assert!(error.contains("invalid query"));
        assert!(error.contains("detail"));
    }
    assert!(answer(&plan(""), &response(302, &envelope("vector", json!([])))).is_err());
    assert!(
        answer(&plan(""), &json!({"status":502,"body":"not JSON"}))
            .err()
            .unwrap()
            .contains("HTTP 502")
    );
    for e in [
        envelope("unsupported", json!([])),
        envelope("vector", json!([{"metric":{},"value":[1]}])),
        envelope("matrix", json!([{"metric":{}}])),
        envelope("scalar", json!([1, 42])),
        envelope("vector", json!([{"metric":{"x":1},"value":[1,"2"]}])),
    ] {
        assert!(answer(&plan(""), &response(200, &e)).is_err());
    }
}

#[test]
fn column_overrides_wrap_without_truncation_and_report_types() {
    let e = envelope("string", json!([1, "abcdefghij"]));
    let shown = answer(
        &plan("Columns-Size: value=3\nColumns-Type: value=text"),
        &response(200, &e),
    )
    .unwrap();
    assert_eq!(shown.rendered.columns[2].kind, "text");
    for fragment in ["abc", "def", "ghi", "j"] {
        assert!(shown.rendered.text.contains(fragment));
    }
    assert!(answer(&plan("Columns-Type: missing=json"), &response(200, &e)).is_err());
}
