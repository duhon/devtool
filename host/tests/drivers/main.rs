//! CONTRACT: the protocol drivers wasm modules reach the outside through (besides http,
//! covered with the Host transports).

mod grpc;
mod mysql;
mod process;
mod tcp;

use serde_json::{Value, json};

fn call(driver: &str, id: u32, op: &str, request: Value) -> Value {
    json!({"id": id, "service": "drivers", "driver": driver, "op": op, "request": request})
}

/// Reads data events until the exit message, which it returns with them.
async fn events(pipe: &mut services_testkit::native::Pipe) -> (Vec<Value>, Value) {
    let mut events = vec![];
    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(10), pipe.read())
            .await
            .expect("the stream ends");
        if message["type"] == "data" {
            events.push(message["event"].clone());
        } else {
            return (events, message);
        }
    }
}
