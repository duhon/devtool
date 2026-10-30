use axum::{Router, body::to_bytes, extract::Request, response::IntoResponse};
use serde_json::json;
use services_testkit::{
    Host,
    http::{self, Server},
};
use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[test]
fn options_keep_derived_fields_and_resolution_modes() {
    let host = Host::new();
    let config = &host.runtime().registry.manifest("splunk").unwrap().params;
    let options = config.iter().find(|p| p.name == "options").unwrap();
    assert!(options.multi);
    assert_eq!(options.keys["from"], "earliestTime");
    for name in ["count", "preview", "offset"] {
        assert_eq!(options.keys[name], name);
        let param = config.iter().find(|p| p.name == name).unwrap();
        assert_eq!(param.resolve.as_deref(), Some("none"));
        assert!(param.flag.is_none());
    }
}

#[tokio::test]
async fn export_sends_app_time_range_and_count_and_returns_debug() {
    let host = Host::new();
    let (server, seen) = http::services().await;
    let reply=host.call("splunk",json!({"baseUrl":server.url,"search":"index=fixture","app":"search","count":"0","earliestTime":"-1h","latestTime":"now","headers":{"Authorization":"Splunk fixture-token","X-Token":"splunk-fixture"}})).await;
    let result = reply.ok();
    assert_eq!(result["columns"], json!(["host", "count"]), "{reply}");
    assert_eq!(result["rows"][0]["host"], "fixture");
    assert_eq!(result["resultCount"], 1);
    assert_eq!(result["debug"]["export"]["streamComplete"], true);
    assert_eq!(result["debug"]["export"]["stoppedAfterCount"], false);
    assert!(result["debug"].get("job").is_none());
    assert_eq!(
        result["debug"]["response"]["messages"][0]["text"],
        "fixture message"
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["token"], "splunk-fixture");
    assert_eq!(
        result["debug"]["request"]["headers"]["Authorization"],
        "Splunk fixture-token"
    );
    assert_eq!(
        seen[0]["path"],
        "/servicesNS/-/search/search/v2/jobs/export"
    );
    let form: std::collections::HashMap<_, _> =
        url::form_urlencoded::parse(seen[0]["body"].as_str().unwrap().as_bytes())
            .into_owned()
            .collect();
    assert_eq!(form["search"], "search index=fixture");
    assert_eq!(form["earliest_time"], "-1h");
    assert_eq!(form["latest_time"], "now");
    assert_eq!(form["output_mode"], "json");
    assert_eq!(form["preview"], "false");
    assert!(!form.contains_key("count"));
    assert!(!form.contains_key("exec_mode"));
}

#[tokio::test]
async fn preview_offset_returns_one_provisional_snapshot_before_eof() {
    use futures_util::StreamExt;
    for (preview, offset, count, expected) in [
        (true, 2, 2, vec!["b2", "b3"]),
        (true, 2, 0, vec!["b2", "b3"]),
        (true, 0, 30, vec!["a0"]),
        (false, 2, 2, vec!["f2", "f3"]),
    ] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let forms = seen.clone();
        let server=Server::new(Router::new().fallback(move |request: Request| {
            let forms=forms.clone();
            async move {
                let request=to_bytes(request.into_body(),1024*1024).await.unwrap();
                let form=url::form_urlencoded::parse(&request).into_owned().collect::<std::collections::HashMap<_,_>>();
                forms.lock().unwrap().push(form);
                let mut body=String::new();
                for (prefix,preview,rows) in [("a",true,1),("b",true,4),("f",false,4)] {
                    for n in 0..rows {
                        body.push_str(&format!("{}\n",json!({
                            "preview":preview,"offset":n,"lastrow":n+1==rows,"result":{"n":format!("{prefix}{n}")}
                        })));
                    }
                }
                let stream=futures_util::stream::once(async {Ok::<_,std::io::Error>(body)})
                    .chain(futures_util::stream::pending());
                axum::body::Body::from_stream(stream).into_response()
            }
        })).await;
        let host = Host::new();
        let reply=tokio::time::timeout(Duration::from_secs(3),host.call("splunk",json!({
            "baseUrl":server.url,"search":"index=fixture","preview":preview,"offset":offset,"count":count
        }))).await.expect("preview/offset must not wait for EOF");
        let result = reply.ok();
        assert_eq!(
            result["rows"],
            json!(expected.iter().map(|n| json!({"n":n})).collect::<Vec<_>>()),
            "{reply}"
        );
        assert_eq!(result["resultCount"], expected.len());
        assert_eq!(result["preview"], preview);
        assert_eq!(result["offset"], offset);
        assert_eq!(result["debug"]["export"]["rowsSkipped"], offset);
        assert_eq!(result["debug"]["export"]["streamComplete"], false);
        if preview {
            assert!(result["warning"].as_str().unwrap().contains("provisional"));
            assert_eq!(result["outcome"], "WARNING");
        } else {
            assert!(result.get("warning").is_none());
            assert_eq!(result["outcome"], "OK");
        }
        let forms = seen.lock().unwrap();
        assert_eq!(forms.len(), 1);
        assert_eq!(forms[0]["preview"], if preview { "true" } else { "false" });
        assert!(!forms[0].contains_key("offset") && !forms[0].contains_key("count"));
    }
}

#[tokio::test]
async fn offset_beyond_final_results_returns_an_empty_final_portion() {
    let host = Host::new();
    let (server, _) = http::services().await;
    let reply = host
        .call(
            "splunk",
            json!({
                "baseUrl":server.url,"search":"index=fixture","offset":"100","count":"30"
            }),
        )
        .await;
    let result = reply.ok();
    assert_eq!(result["rows"], json!([]), "{reply}");
    assert_eq!(result["resultCount"], 0);
    assert_eq!(result["preview"], false);
    assert_eq!(result["debug"]["export"]["streamComplete"], true);
    assert_eq!(result["debug"]["export"]["rowsSkipped"], 1);
}

#[tokio::test]
async fn network_error_causes_are_kept_in_debug() {
    let host = Host::new();
    let reply = host
        .call(
            "splunk",
            json!({
                "baseUrl":"http://127.0.0.1:0","search":"index=fixture"
            }),
        )
        .await;
    assert!(reply.error().contains("Request failed"), "{reply}");
    assert!(
        reply["debug"]["connection"]["error"]["cause"].is_object(),
        "{reply}"
    );
}

#[tokio::test]
async fn http_and_stream_errors_are_reported_without_retry_or_partial_rows() {
    for (status, body, expected) in [
        (
            401,
            r#"{"messages":[{"type":"ERROR","text":"access denied"}]}"#,
            "access denied",
        ),
        (500, "upstream unavailable", "HTTP 500"),
        (
            200,
            "{\"preview\":false,\"result\":{\"x\":1}}\n{\"preview\":false,\"result\":",
            "truncated",
        ),
        (
            200,
            "{\"preview\":false,\"result\":{\"x\":1}}\n{\"messages\":[{\"type\":\"ERROR\",\"text\":\"bad search\"}]}\n",
            "bad search",
        ),
    ] {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let server = Server::new(Router::new().fallback(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            async move { (axum::http::StatusCode::from_u16(status).unwrap(), body) }
        }))
        .await;
        let host = Host::new();
        let reply = host
            .call(
                "splunk",
                json!({
                    "baseUrl":server.url,"search":"index=fixture","count":"0"
                }),
            )
            .await;
        assert!(reply.error().contains(expected), "{reply}");
        let debug = &reply["debug"];
        assert_eq!(debug["response"]["status"], status, "{reply}");
        assert_eq!(
            debug["connection"]["events"].as_array().unwrap().len(),
            2,
            "{reply}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        for (invalid, label) in [
            (json!({"count":"-1"}), "Count"),
            (json!({"offset":"-1"}), "Offset"),
            (json!({"preview":"yes"}), "Preview"),
        ] {
            let mut params = json!({"baseUrl":server.url,"search":"index=fixture"});
            params
                .as_object_mut()
                .unwrap()
                .extend(invalid.as_object().unwrap().clone());
            let reply = host.call("splunk", params).await;
            assert!(reply.error().contains(label), "{reply}");
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "invalid Count must not consume an API call"
        );
    }
}

#[tokio::test]
async fn legacy_export_fallback_is_explicit_paced_and_only_for_404() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let server = Server::new(Router::new().fallback(move |request: Request| {
        seen.lock()
            .unwrap()
            .push((request.uri().path().to_string(), std::time::Instant::now()));
        async {
            if request.uri().path().contains("/v2/") {
                return axum::http::StatusCode::NOT_FOUND.into_response();
            }
            http::answer(request).await
        }
    }))
    .await;
    let host = Host::new();
    let reply = host
        .call(
            "splunk",
            json!({
                "baseUrl":format!("{}/proxy",server.url),"app":"some app","search":"index=fixture"
            }),
        )
        .await;
    let result = reply.ok();
    assert_eq!(result["rows"][0]["host"], "fixture", "{reply}");
    assert_eq!(result["debug"]["export"]["legacyFallbacks"], 1);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0].0,
        "/proxy/servicesNS/-/some%20app/search/v2/jobs/export"
    );
    assert_eq!(
        calls[1].0,
        "/proxy/servicesNS/-/some%20app/search/jobs/export"
    );
    assert!(calls[1].1.duration_since(calls[0].1) >= Duration::from_secs(13));
}

#[tokio::test]
async fn first_portion_returns_before_eof_and_closes_the_connection() {
    struct Closed(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Closed {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    for (count, expected) in [(Some(json!("2")), 2), (Some(json!(2.0)), 2), (None, 100)] {
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (disconnected, seen) = (closed.clone(), calls.clone());
        let server = Server::new(Router::new().fallback(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            let guard = Closed(disconnected.clone());
            async move {
                let mut body = "{\"preview\":true,\"result\":{\"host\":\"not final\"},\"lastrow\":true}\n".to_string();
                for n in 0..101 {
                    body.push_str(&format!("{}\n",json!({"preview":false,"offset":n,"result":{"host":format!("сервер🦀-{n}")}})));
                }
                let stream = futures_util::stream::unfold((Some(body), guard), |(body, guard)| async {
                    match body {
                        Some(body) => Some((Ok::<_,std::io::Error>(body), (None, guard))),
                        None => std::future::pending().await,
                    }
                });
                axum::body::Body::from_stream(stream).into_response()
            }
        })).await;
        let host = Host::new();
        let mut params = json!({"baseUrl":server.url,"search":"index=fixture | sort - _time"});
        if let Some(count) = count {
            params["count"] = count;
        }
        let reply = tokio::time::timeout(Duration::from_secs(3), host.call("splunk", params))
            .await
            .expect("must return without EOF or full search completion");
        let result = reply.ok();
        assert_eq!(
            result["rows"].as_array().unwrap().len(),
            expected,
            "{reply}"
        );
        assert_eq!(result["rows"][0]["host"], "сервер🦀-0");
        assert_eq!(result["resultCount"], expected);
        assert_eq!(result["debug"]["export"]["stoppedAfterCount"], true);
        assert_eq!(result["debug"]["export"]["streamComplete"], false);
        assert_eq!(result["debug"]["export"]["ignoredPreviewRecords"], 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        tokio::time::timeout(Duration::from_secs(3), async {
            while !closed.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dropping the unfinished response must close its connection");
    }
}

#[tokio::test]
async fn zero_count_reads_to_eof_and_stop_drops_a_pending_stream() {
    let host = Host::new();
    let server = Server::new(Router::new().fallback(|| async {
        let stream = futures_util::stream::unfold(0, |index| async move {
            match index {
                0 => Some((
                    Ok::<_, std::io::Error>("{\"preview\":false,\"result\":{\"n\":1}}\n"),
                    1,
                )),
                1 => {
                    tokio::time::sleep(Duration::from_millis(75)).await;
                    Some((
                        Ok("{\"preview\":false,\"result\":{\"n\":2},\"lastrow\":true}\n"),
                        2,
                    ))
                }
                _ => None,
            }
        });
        axum::body::Body::from_stream(stream).into_response()
    }))
    .await;
    let started = std::time::Instant::now();
    let reply = host
        .call(
            "splunk",
            json!({
                "baseUrl":server.url,"search":"index=fixture","count":"0"
            }),
        )
        .await;
    let result = reply.ok();
    assert!(started.elapsed() >= Duration::from_millis(75));
    assert_eq!(result["rows"], json!([{"n":1},{"n":2}]), "{reply}");
    assert_eq!(result["debug"]["export"]["streamComplete"], true);

    let headers_sent = Arc::new(tokio::sync::Notify::new());
    let notify = headers_sent.clone();
    let server = Server::new(Router::new().fallback(move || {
        notify.notify_one();
        async {
            axum::body::Body::from_stream(futures_util::stream::pending::<
                std::result::Result<String, std::io::Error>,
            >())
            .into_response()
        }
    }))
    .await;
    let cancel = CancellationToken::new();
    let stopped = cancel.clone();
    let task = tokio::spawn(async move {
        host.call_cancellable(
            "splunk",
            json!({
                "baseUrl":server.url,"search":"index=fixture","count":"0"
            }),
            cancel,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), headers_sent.notified())
        .await
        .unwrap();
    stopped.cancel();
    let reply = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply.ok()["cancelled"], true, "{reply}");
}

#[tokio::test]
async fn server_quota_is_shared_across_processes_paths_and_apps_only() {
    type Calls = Arc<Mutex<Vec<(usize, std::time::Instant)>>>;
    async fn clock_server(id: usize, calls: Calls) -> Server {
        Server::new(Router::new().fallback(move |request: Request| {
            let calls = calls.clone();
            async move {
                calls.lock().unwrap().push((id, std::time::Instant::now()));
                http::answer(request).await
            }
        }))
        .await
    }
    let host = Host::new();
    let other_host = host.reopen();
    let calls = Calls::default();
    let shared = clock_server(0, calls.clone()).await;
    let independent = clock_server(1, calls.clone()).await;
    let params =
        json!({"baseUrl":format!("{}/proxy", shared.url),"search":"index=main","app":"search"});
    let other_params = json!({"baseUrl":independent.url,"search":"index=main"});
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_services-devtools"));
    command
        .env("SERVICES_DATA_DIR", host.path())
        .args(["cli", "splunk", &shared.url, "index=cli", "-o", "app=other"])
        .kill_on_drop(true);
    let (reply, child, other_reply) = tokio::join!(
        host.call("splunk", params),
        command.output(),
        other_host.call("splunk", other_params)
    );
    assert_eq!(reply.ok()["rows"][0]["host"], "fixture", "{reply}");
    let child = child.unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(String::from_utf8_lossy(&child.stdout).contains("fixture"));
    assert_eq!(
        other_reply.ok()["rows"][0]["host"],
        "fixture",
        "{other_reply}"
    );
    let calls = calls.lock().unwrap();
    let shared: Vec<_> = calls
        .iter()
        .filter(|(id, _)| *id == 0)
        .map(|(_, at)| *at)
        .collect();
    let independent: Vec<_> = calls
        .iter()
        .filter(|(id, _)| *id == 1)
        .map(|(_, at)| *at)
        .collect();
    assert_eq!(shared.len(), 2);
    assert_eq!(independent.len(), 1);
    for timestamps in [&shared, &independent] {
        for pair in timestamps.windows(2) {
            assert!(
                pair[1].duration_since(pair[0]) >= Duration::from_secs(13),
                "{timestamps:?}"
            );
        }
        for start in timestamps {
            assert!(
                timestamps
                    .iter()
                    .filter(
                        |at| **at >= *start && at.duration_since(*start) < Duration::from_secs(60)
                    )
                    .count()
                    <= 5
            );
        }
    }
    assert!(
        shared[0]
            .max(independent[0])
            .duration_since(shared[0].min(independent[0]))
            < Duration::from_secs(5),
        "Different servers must not wait for each other's quota"
    );
}

#[tokio::test]
async fn redirects_cannot_bypass_the_server_quota() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let server = Server::new(Router::new().fallback(move || {
        seen.fetch_add(1, Ordering::SeqCst);
        async {
            (
                axum::http::StatusCode::TEMPORARY_REDIRECT,
                [("location", "/redirect-target")],
            )
        }
    }))
    .await;
    let host = Host::new();
    let reply = host
        .call(
            "splunk",
            json!({"baseUrl":server.url,"search":"index=main"}),
        )
        .await;
    assert!(
        reply.error().contains("automatic redirects are disabled"),
        "{reply}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rate_limit_retries_once_and_keeps_trace_and_header_date() {
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = count.clone();
    let router = Router::new().fallback(move |request: Request| {
        let state = state.clone();
        async move {
            if state.fetch_add(1, Ordering::SeqCst) == 0 {
                return (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "Thu, 01 Jan 1970 00:00:00 GMT")],
                    axum::Json(json!({"messages":[{"text":"retry"}]})),
                )
                    .into_response();
            }
            http::answer(request).await
        }
    });
    let server = Server::new(router).await;
    let host = Host::new();
    let reply = host
        .call(
            "splunk",
            json!({"baseUrl":server.url,"search":"search\nindex=main"}),
        )
        .await;
    let result = reply.ok();
    assert_eq!(
        result["debug"]["export"]["rateLimitedRetries"], 1,
        "{reply}"
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(
        result["debug"]["connection"]["events"]
            .as_array()
            .unwrap()
            .len()
            >= 4
    );
}
