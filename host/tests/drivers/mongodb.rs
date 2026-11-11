use serde_json::{Value, json};
use services_sdk::mongodb::{self, Event};
use services_testkit::{DataDir, native::Pipe};
use std::time::{Duration, Instant};

fn call(id: u32, op: &str, request: &mongodb::Request) -> Value {
    super::call("mongodb", id, op, json!(request))
}

/// The server of the tests that need one, from `MONGODB_URL` (for example
/// `mongodb://127.0.0.1:27017`); they pass without running when it is not set.
fn server() -> Option<String> {
    let url = std::env::var("MONGODB_URL")
        .ok()
        .filter(|url| !url.is_empty());
    if url.is_none() {
        eprintln!("MONGODB_URL is not set; skipping");
    }
    url
}

/// A database of its own per test, dropped when the test ends.
struct Database {
    url: String,
    name: String,
}

impl Database {
    async fn new(url: &str, data: &DataDir, test: &str) -> Self {
        let name = format!("devtools_{test}_{}", std::process::id());
        let database = Self {
            url: url.into(),
            name,
        };
        let reply = database
            .run(
                data,
                json!({"insert": "pets", "documents": (0..25).map(|n| json!({
                    "_id": {"$oid": format!("65a1b2c3d4e5f6a7b8c9d0{n:02x}")},
                    "n": n,
                    "big": {"$numberLong": "9007199254740993"},
                    "at": {"$date": "2026-01-15T12:34:56.789Z"},
                    "price": {"$numberDecimal": "12.30"},
                    "raw": {"$binary": {"base64": "AQI=", "subType": "00"}},
                })).collect::<Vec<_>>()}),
            )
            .await;
        assert_eq!(reply["ok"], true, "{reply}");
        database
    }

    fn request(&self, command: Value) -> mongodb::Request {
        mongodb::Request::new(&self.url, &self.name, command)
    }

    async fn run(&self, data: &DataDir, command: Value) -> Value {
        data.runtime()
            .dispatch(&call(0, "call", &self.request(command)))
            .await
    }

    async fn drop(self, data: &DataDir) {
        self.run(data, json!({"dropDatabase": 1})).await;
    }
}

#[tokio::test]
async fn mongodb_call_iterates_cursors_and_keeps_bson_types() {
    let Some(url) = server() else { return };
    let data = DataDir::new();
    let database = Database::new(&url, &data, "call").await;
    let runtime = data.runtime();

    let mut find = database.request(json!({"find": "pets", "sort": {"n": 1}, "batchSize": 4}));
    find.batch_size = Some(5);
    let reply = runtime.dispatch(&call(1, "call", &find)).await;
    let response = mongodb::Response::parse(&reply["result"]).unwrap();
    assert_eq!((response.count, response.documents.len()), (25, 25));
    assert!(!response.truncated && response.reply.is_none());
    assert_eq!(
        response.documents[1],
        json!({
            "_id": {"$oid": "65a1b2c3d4e5f6a7b8c9d001"},
            "n": {"$numberInt": "1"},
            "big": {"$numberLong": "9007199254740993"},
            "at": {"$date": {"$numberLong": "1768480496789"}},
            "price": {"$numberDecimal": "12.30"},
            "raw": {"$binary": {"base64": "AQI=", "subType": "00"}},
        })
    );
    assert!(response.timing.total_us >= response.timing.first_byte_us);

    for (limit, truncated) in [(10, true), (25, false), (24, true)] {
        let mut limited = find.clone();
        limited.max_documents = Some(limit);
        let reply = runtime.dispatch(&call(2, "call", &limited)).await;
        let response = mongodb::Response::parse(&reply["result"]).unwrap();
        assert_eq!(response.documents.len() as u64, limit);
        assert_eq!(response.truncated, truncated, "maxDocuments {limit}");
    }

    let reply = database
        .run(
            &data,
            json!({"aggregate": "pets", "pipeline": [{"$count": "all"}], "cursor": {}}),
        )
        .await;
    let response = mongodb::Response::parse(&reply["result"]).unwrap();
    assert_eq!(response.documents, [json!({"all": {"$numberInt": "25"}})]);

    let reply = database.run(&data, json!({"ping": 1})).await;
    let response = mongodb::Response::parse(&reply["result"]).unwrap();
    assert_eq!(
        response.reply.unwrap()["ok"],
        json!({"$numberDouble": "1.0"})
    );

    let reply = database
        .run(&data, json!({"find": "pets", "filter": {"$bogus": 1}}))
        .await;
    assert_eq!(reply["ok"], true, "a server error is an answer: {reply}");
    let error = mongodb::Response::parse(&reply["result"])
        .unwrap()
        .error
        .unwrap();
    assert_eq!((error.code, error.code_name.as_str()), (2, "BadValue"));
    assert!(error.message.contains("$bogus"), "{}", error.message);

    database.drop(&data).await;
}

#[tokio::test]
async fn mongodb_stream_sends_server_batches_then_exit() {
    let Some(url) = server() else { return };
    let data = DataDir::new();
    let database = Database::new(&url, &data, "stream").await;
    let mut pipe = Pipe::start(data.runtime());
    let mut request = database.request(json!({"find": "pets", "batchSize": 10}));
    request.batch_size = Some(10);
    pipe.send(&call(1, "stream", &request)).await;
    let (events, exit) = super::events(&mut pipe).await;
    let sizes: Vec<usize> = events
        .iter()
        .map(|event| match Event::parse(event).unwrap() {
            Event::Documents(documents) => documents.len(),
            Event::Reply(_) => 0,
        })
        .collect();
    assert_eq!(sizes, [10, 10, 5]);
    let response = mongodb::Response::parse(&exit["info"]).unwrap();
    assert_eq!(response.count, 25);
    assert!(response.documents.is_empty());

    pipe.send(&call(2, "stream", &database.request(json!({"ping": 1}))))
        .await;
    let (events, _) = super::events(&mut pipe).await;
    assert!(matches!(Event::parse(&events[0]).unwrap(), Event::Reply(_)));

    database.drop(&data).await;
}

#[tokio::test]
async fn mongodb_cancel_and_timeout_drop_the_command() {
    let Some(url) = server() else { return };
    let data = DataDir::new();
    let database = Database::new(&url, &data, "cancel").await;
    let slow = json!({"find": "pets", "filter": {"$where": "sleep(200) || true"}});

    let mut pipe = Pipe::start(data.runtime());
    pipe.send(&call(1, "stream", &database.request(slow.clone())))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    pipe.send(&json!({"id": 1, "type": "stop"})).await;
    let (_, reply) = super::events(&mut pipe).await;
    assert_eq!(reply["failure"]["stage"], "cancelled", "{reply}");
    let causes = reply["failure"]["causes"].to_string();
    assert!(causes.contains("not rolled back"), "{causes}");

    let mut timed = database.request(slow.clone());
    timed.timeout_ms = Some(300);
    let started = Instant::now();
    let reply = data.runtime().dispatch(&call(2, "call", &timed)).await;
    assert_eq!(reply["failure"]["stage"], "timeout", "{reply}");
    assert!(started.elapsed() < Duration::from_secs(3));

    let mut own = slow.clone();
    own["maxTimeMS"] = json!(100);
    let reply = database.run(&data, own).await;
    assert_eq!(reply["ok"], true, "{reply}");
    let error = mongodb::Response::parse(&reply["result"])
        .unwrap()
        .error
        .unwrap();
    assert_eq!(error.code_name, "MaxTimeMSExpired", "{reply}");

    database.drop(&data).await;
}

#[tokio::test]
async fn mongodb_timeout_is_sent_as_max_time_ms() {
    let Some(url) = server() else { return };
    let data = DataDir::new();
    let database = Database::new(&url, &data, "maxtime").await;
    database.run(&data, json!({"profile": 2})).await;
    let mut find = database.request(json!({"find": "pets", "filter": {"n": 7}}));
    find.timeout_ms = Some(5_000);
    let reply = data.runtime().dispatch(&call(1, "call", &find)).await;
    assert_eq!(reply["ok"], true, "{reply}");
    let reply = database
        .run(
            &data,
            json!({"find": "system.profile", "filter": {"command.filter.n": 7}}),
        )
        .await;
    let profiled = mongodb::Response::parse(&reply["result"])
        .unwrap()
        .documents;
    let sent: i64 = profiled[0]["command"]["maxTimeMS"]["$numberLong"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((4_000..=5_000).contains(&sent), "{sent}");
    database.run(&data, json!({"profile": 0})).await;
    database.drop(&data).await;
}

#[tokio::test]
async fn mongodb_failures_name_their_stage_without_credentials() {
    let data = DataDir::new();
    let runtime = data.runtime();
    let stage = |reply: &Value| reply["failure"]["stage"].as_str().unwrap().to_owned();
    let request = |uri: &str, database: &str, command: Value| {
        let mut request = mongodb::Request::new(uri, database, command);
        request.connect_ms = Some(300);
        request
    };
    let closed = "mongodb://admin:s3cr%2Ft@127.0.0.1:1/?authSource=admin";
    for (n, invalid) in [
        request("mysql://127.0.0.1/db", "db", json!({"ping": 1})),
        request(closed, "db", json!([{"ping": 1}])),
        request(closed, "db", json!({})),
        request(
            closed,
            "db",
            json!({"find": "c", "filter": {"_id": {"$oid": "zz"}}}),
        ),
        request(closed, "a.b", json!({"ping": 1})),
        mongodb::Request {
            database: None,
            ..request(closed, "", json!({"ping": 1}))
        },
    ]
    .iter()
    .enumerate()
    {
        let reply = runtime.dispatch(&call(n as u32, "call", invalid)).await;
        assert_eq!(stage(&reply), "send", "{reply}");
    }

    let started = Instant::now();
    let reply = runtime
        .dispatch(&call(9, "call", &request(closed, "db", json!({"ping": 1}))))
        .await;
    assert_eq!(stage(&reply), "connect", "{reply}");
    assert!(started.elapsed() < Duration::from_secs(5));
    let text = reply.to_string();
    assert!(!text.contains("s3cr"), "{text}");
}
