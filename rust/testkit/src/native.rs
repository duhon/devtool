//! Native-messaging pipe helpers: a host running `adapters::native::run` over
//! in-memory streams, as Chrome would drive it.

use serde_json::Value;
use services_native::{
    Runtime,
    adapters::{framing::encode, native},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    task::JoinHandle,
};

pub struct Pipe {
    pub input: DuplexStream,
    pub output: DuplexStream,
    pub host: JoinHandle<services_native::Result<()>>,
}

impl Pipe {
    pub fn start(runtime: Runtime) -> Self {
        let (input, input_host) = tokio::io::duplex(65536);
        let (output_host, output) = tokio::io::duplex(65536);
        let host = tokio::spawn(native::run(runtime, input_host, output_host));
        Self {
            input,
            output,
            host,
        }
    }

    pub async fn send(&mut self, message: &Value) {
        self.input
            .write_all(&encode(message).unwrap())
            .await
            .unwrap();
    }

    pub async fn read(&mut self) -> Value {
        read_frame(&mut self.output).await
    }
}

pub async fn read_frame(reader: &mut (impl tokio::io::AsyncRead + Unpin)) -> Value {
    let n = reader.read_u32_le().await.unwrap();
    assert!(n as usize <= services_contracts::native_messaging::MAX_HOST_MESSAGE_BYTES);
    let mut bytes = vec![0; n as usize];
    reader.read_exact(&mut bytes).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
