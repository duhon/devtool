use services_module_mongodb::Mongodb;
use services_sdk::Module;

/// The kinds of the tokens of the payload lines, as `text => kind`.
fn kinds(payload: &str) -> Vec<(String, String)> {
    let mut lines = vec!["#payload".to_owned()];
    lines.extend(payload.lines().map(String::from));
    let out = Mongodb::default().highlight(&[], &lines, &[]);
    let mut found = Vec::new();
    for (line, shown) in lines.iter().zip(&out).skip(1) {
        for token in &shown.tokens {
            let start = token.start as usize;
            found.push((
                line[start..start + token.len as usize].to_owned(),
                token.kind.clone(),
            ));
        }
    }
    found
}

fn kind(found: &[(String, String)], text: &str) -> String {
    found
        .iter()
        .find(|(name, _)| name == text)
        .unwrap_or_else(|| panic!("{text} in {found:?}"))
        .1
        .clone()
}

#[test]
fn a_pipeline_tells_keys_operators_fields_and_values_apart() {
    let found = kinds(
        r#"{
  "pipeline": [
    {"$match": {"status": {"$in": ["a", "b"]}, "qty": {"$gte": 25}}},
    {"$lookup": {"from": "inventory", "let": {"sid": "$storeId"},
      "pipeline": [{"$match": {"$expr": {"$eq": ["$storeId", "$$sid"]}}}], "as": "available"}},
    {"$addFields": {"count": {"$size": "$available"}, "total": {"$sum": "$available.price"}}},
    {"$sort": {"total": -1}},
    {"$project": {"avg": {"$round": [{"$divide": ["$total", "$count"]}, 2]}, "ok": true, "n": null}}
  ],
  // note
  "x": {"$oid": "65f0c0ffee0000000000000a"}
}"#,
    );
    for (text, expected) in [
        (r#""pipeline""#, "string attribute"),
        (r#""$match""#, "string keyword"),
        (r#""$in""#, "string keyword"),
        (r#""$round""#, "string keyword"),
        (r#""status""#, "string property"),
        (r#""total""#, "string property"),
        (r#""from""#, "string attribute"),
        (r#""let""#, "string attribute"),
        (r#""as""#, "string attribute"),
        (r#""inventory""#, "string"),
        (r#""a""#, "string"),
        (r#""$storeId""#, "string variable-3"),
        (r#""$$sid""#, "string variable-3"),
        (r#""$available.price""#, "string variable-3"),
        ("25", "number"),
        ("-1", "number"),
        ("2", "number"),
        ("true", "atom"),
        ("null", "atom"),
        ("// note", "comment"),
        (r#""$oid""#, "string type"),
        (r#""65f0c0ffee0000000000000a""#, "string"),
    ] {
        assert_eq!(kind(&found, text), expected, "{text}");
    }
}

#[test]
fn a_find_shows_numbers_and_strings() {
    let found =
        kinds(r#"{"filter": {"age": {"$gt": 21}, "name": "Ann", "ratio": 1e3}, "limit": 10}"#);
    for (text, expected) in [
        (r#""filter""#, "string attribute"),
        (r#""age""#, "string property"),
        (r#""$gt""#, "string keyword"),
        ("21", "number"),
        (r#""Ann""#, "string"),
        ("1e3", "number"),
        (r#""limit""#, "string attribute"),
        ("10", "number"),
    ] {
        assert_eq!(kind(&found, text), expected, "{text}");
    }
}
