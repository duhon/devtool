use serde_json::{Value, json};
use services_module_prometheus::{
    chart::State,
    render::{answer, answer_with_chart},
    request::{Plan, sections},
};
use services_sdk::{Check, Section};

fn plan(options: &str) -> Plan {
    let check = Check {
        sections: [
            ("target", "https://example.invalid"),
            (
                "metadata",
                "Query-Mode: range\nQuery-Start: 0\nQuery-End: 10\nQuery-Step: 1s",
            ),
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

fn response(samples: Value) -> Value {
    json!({"status":200, "body":json!({
        "status":"success", "data":{"resultType":"matrix","result":samples}
    }).to_string()})
}

fn series(values: Value) -> Value {
    json!([{"metric":{"job":"api"},"values":values}])
}

fn dots(text: &str) -> usize {
    text.chars()
        .filter(|c| ('\u{2801}'..='\u{28ff}').contains(c))
        .count()
}

#[test]
fn chart_is_copyable_braille_text_with_fixed_geometry_and_complete_labels() {
    let result = answer(
        &plan("Format: chart\nChart-Width: 64\nChart-Height: 12"),
        &response(series(json!([
            [0, "1"],
            [1, "2"],
            [2, "2"],
            [3, "6"],
            [4, "4"],
            [5, "7"],
            [6, "8"],
            [7, "7"],
            [8, "4"],
            [9, "3"],
            [10, "2"]
        ]))),
    )
    .unwrap();
    let text = &result.rendered.text;
    assert!(dots(text) > 10);
    assert!(text.contains("1970-01-01"));
    assert!(text.contains("\"job\":\"api\""));
    assert!(!text.contains('\x1b') && !text.contains('\t'));
    assert_eq!(result.rendered.language, "text");
    assert!(result.rendered.columns.is_empty());
    let graph: Vec<_> = text
        .lines()
        .skip_while(|line| !line.starts_with("Series "))
        .skip(1)
        .take(12)
        .collect();
    assert_eq!(graph.len(), 12);
    assert!(graph.iter().all(|line| line.chars().count() == 64));
    println!("{text}");
}

#[test]
fn non_finite_samples_and_missing_steps_break_lines_instead_of_fabricating_data() {
    let plan = plan("Format: chart");
    for (values, gaps) in [
        (json!([[0, "1"], [5, "NaN"], [10, "2"]]), 1),
        (json!([[0, "1"], [1, "+Inf"], [2, "-Inf"], [10, "2"]]), 2),
        (json!([[0, "1"], [10, "2"]]), 1),
    ] {
        let text = answer(&plan, &response(series(values)))
            .unwrap()
            .rendered
            .text;
        assert!(dots(&text) <= 2, "{text}");
        assert!(text.contains(&format!("{gaps} gaps")), "{text}");
    }
}

#[test]
fn series_order_and_repeat_scales_are_stable_while_query_changes_reset_them() {
    let mut state = State::default();
    let mut plan = plan("Format: chart");
    let first = response(json!([
        {"metric":{"job":"z"},"values":[[0,"0"],[1,"100"]]},
        {"metric":{"job":"a"},"values":[[0,"0"],[1,"100"]]},
    ]));
    let initial = answer_with_chart(&plan, &first, &mut state)
        .unwrap()
        .rendered
        .text;
    assert!(initial.find("\"a\"").unwrap() < initial.find("\"z\"").unwrap());
    let second = response(json!([
        {"metric":{"job":"a"},"values":[[0,"0"],[1,"10"]]},
        {"metric":{"job":"z"},"values":[[0,"0"],[1,"10"]]},
    ]));
    let repeated = answer_with_chart(&plan, &second, &mut state)
        .unwrap()
        .rendered
        .text;
    assert!(initial.contains("105") && repeated.contains("105"));
    plan.chart_identity.push('x');
    let changed = answer_with_chart(&plan, &second, &mut state)
        .unwrap()
        .rendered
        .text;
    assert!(changed.contains("10.5") && !changed.contains("105"));
}

#[test]
fn options_validate_and_explicit_bounds_report_clipping() {
    for options in [
        "Format: chart\nChart-Width: 5",
        "Format: chart\nChart-Height: 100",
        "Format: chart\nChart-Min: 0",
        "Format: chart\nChart-Min: NaN\nChart-Max: 1",
        "Format: chart\nChart-Min: 2\nChart-Max: 1",
        "Format: table\nChart-Width: 80",
        "Format: chart\nColumns-Size: value=10",
    ] {
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
        assert!(
            Plan::parse(&sections(&check).unwrap(), 0.0).is_err(),
            "{options}"
        );
    }
    let text = answer(
        &plan("Format: chart\nChart-Min: 0\nChart-Max: 10"),
        &response(series(json!([[0, "-1"], [1, "5"], [2, "100"]]))),
    )
    .unwrap()
    .rendered
    .text;
    assert!(text.contains("2 clipped"));
}

#[test]
fn charts_handle_constants_extremes_empty_data_and_reject_non_numeric_results() {
    let plan = plan("Format: chart");
    for value in ["0", "5", "-4", "1e-320", "1e308"] {
        let text = answer(&plan, &response(series(json!([[0, value], [1, value]]))))
            .unwrap()
            .rendered
            .text;
        assert!(dots(&text) > 0, "{value}: {text}");
    }
    let text = answer(
        &plan,
        &response(series(json!([[0, "-1e308"], [1, "1e308"]]))),
    )
    .unwrap()
    .rendered
    .text;
    assert!(dots(&text) > 0);
    assert!(
        answer(&plan, &response(json!([])))
            .unwrap()
            .rendered
            .text
            .contains("No data")
    );
    let histograms =
        response(json!([{"metric":{},"histograms":[[0,{"count":"1","sum":"2","buckets":[]}]]}]));
    assert!(
        answer(&plan, &histograms)
            .err()
            .unwrap()
            .contains("histogram")
    );
    let string = json!({"status":200,"body":json!({"status":"success","data":{"resultType":"string","result":[0,"42"]}}).to_string()});
    assert!(answer(&plan, &string).is_err());
}

#[test]
fn instant_samples_use_real_values_without_fabricating_a_time_history() {
    let check = Check {
        sections: [
            ("target", "https://example.invalid"),
            ("metadata", "Query-Time: 0.0005"),
            ("payload", "up"),
            ("options", "Format: chart"),
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
    let plan = Plan::parse(&sections(&check).unwrap(), 0.0).unwrap();
    let response = json!({"status":200,"body":json!({
        "status":"success","data":{"resultType":"vector","result":[{"metric":{},"value":[0,"5"]}]}
    }).to_string()});
    let text = answer(&plan, &response).unwrap().rendered.text;
    assert_eq!(dots(&text), 1, "{text}");
    assert!(text.contains("1 samples"));
}

#[test]
fn dense_series_keep_narrow_peaks_when_reduced_to_braille_resolution() {
    let values: Vec<_> = (0..10_001)
        .map(|index| {
            json!([
                index as f64 / 1000.0,
                if index == 5037 { "100" } else { "0" }
            ])
        })
        .collect();
    let text = answer(&plan("Format: chart"), &response(series(json!(values))))
        .unwrap()
        .rendered
        .text;
    assert!(text.contains("105") && text.contains("10001 samples"));
    assert!(dots(&text) > 20);
}
