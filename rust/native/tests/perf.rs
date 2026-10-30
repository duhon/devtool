//! PERFORMANCE: budgets for paths users feel. Ignored by default; run with
//! `cargo test -p services-native --release --test perf -- --ignored --nocapture`.
//! Budgets are about 5x the time measured on a developer laptop, so a
//! failure means a regression rather than noise.

use serde_json::{Map, Value, json};
use services_native::adapters::framing::{FrameReader, response_frames};
use services_testkit::{Host, http};
use std::time::{Duration, Instant};

#[track_caller]
fn within(label: &str, budget: Duration, elapsed: Duration) {
    eprintln!("{label}: {elapsed:?} (budget {budget:?})");
    assert!(
        elapsed <= budget,
        "{label} took {elapsed:?}, budget {budget:?}"
    );
}

#[test]
#[ignore = "performance budget; run with --release --ignored"]
fn large_replies_cross_native_messaging() {
    let rows: Vec<Value> = (0..20_000)
        .map(|id| json!({"id":id,"payload":"fixture-🦀\\\"\n".repeat(64)}))
        .collect();
    let reply = json!({"id":1,"ok":true,"result":{"rows":rows}});
    let size = serde_json::to_vec(&reply).unwrap().len();
    assert!(size > 16 * 1024 * 1024, "{size}");
    let started = Instant::now();
    let mut reader = FrameReader::default();
    let mut assembler = services_contracts::native_messaging::ResponseAssembler::default();
    let mut assembled = None;
    for frame in response_frames(&reply, true).unwrap() {
        for message in reader.feed(&frame.unwrap()).unwrap() {
            if let Some(value) = assembler.push(&message).unwrap() {
                assembled = Some(value);
            }
        }
    }
    within(
        &format!("{} MB reply framed and reassembled", size / 1024 / 1024),
        Duration::from_millis(750),
        started.elapsed(),
    );
    assert_eq!(assembled, Some(reply));
}

#[tokio::test]
#[ignore = "performance budget; run with --release --ignored"]
async fn history_pages_stay_fast_with_many_entries() {
    const CHECK: &str = "019a1234-5678-7890-abcd-0123456789ab";
    const ENTRIES: usize = 1000;
    let host = Host::new();
    let (server, _) = http::services().await;
    let started = Instant::now();
    for index in 0..ENTRIES {
        host.send(json!({"service":"rest",
            "history":{"checkId":CHECK,"sessionId":CHECK,"title":format!("#check {index}"),"source":"#check"},
            "params":{"method":"POST","url":format!("{}/echo",server.url),"body":"x".repeat(2048)}}))
            .await
            .ok();
    }
    within(
        &format!("{ENTRIES} requests recorded"),
        Duration::from_secs(25),
        started.elapsed(),
    );
    let started = Instant::now();
    let mut before = Value::Null;
    let mut pages = 0;
    loop {
        let mut fields = json!({"checkId":CHECK,"limit":50});
        if !before.is_null() {
            fields["before"] = before;
        }
        let page = host.op("history", "list", fields).await;
        assert_eq!(page.ok()["total"], ENTRIES, "{page}");
        pages += 1;
        before = page.ok()["nextBefore"].clone();
        if before.is_null() {
            break;
        }
    }
    within(
        &format!("{pages} history pages listed"),
        Duration::from_millis(300),
        started.elapsed(),
    );
    assert_eq!(pages, ENTRIES / 50);
}

#[tokio::test]
#[ignore = "performance budget; run with --release --ignored"]
async fn variables_resolve_fast_with_many_profiles() {
    const PROFILES: usize = 200;
    const REQUESTS: usize = 100;
    let host = Host::new();
    let (server, _) = http::services().await;
    for profile in 0..PROFILES {
        let variables: Map<String, Value> = (0..20)
            .map(|index| {
                let value = if profile > 0 && index == 0 {
                    format!("{{{{p{}:V1}}}}-{profile}", profile - 1)
                } else {
                    format!("value-{profile}-{index}")
                };
                (format!("V{index}"), json!(value))
            })
            .collect();
        host.op(
            "dictionary",
            "save",
            json!({"name":format!("p{profile}"),"variables":variables}),
        )
        .await
        .ok();
    }
    let headers: Map<String, Value> = (0..50)
        .map(|index| {
            (
                format!("X-{index}"),
                json!(format!("{{{{p{}:V{}}}}}", index * 4, index % 20)),
            )
        })
        .collect();
    let started = Instant::now();
    for _ in 0..REQUESTS {
        let reply = host
            .call(
                "rest",
                json!({"method":"GET","url":format!("{}/echo",server.url),"headers":headers}),
            )
            .await;
        reply.ok();
    }
    within(
        &format!("{REQUESTS} requests with 50 variables across {PROFILES} profiles"),
        Duration::from_secs(2),
        started.elapsed(),
    );
}
