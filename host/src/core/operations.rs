use super::error::{Error, Result};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct Scope {
    pub cancel: CancellationToken,
    requests: Mutex<HashMap<u32, CancellationToken>>,
}
impl Scope {
    pub async fn request(&self, id: u32) -> CancellationToken {
        self.requests
            .lock()
            .await
            .entry(id)
            .or_insert_with(|| self.cancel.child_token())
            .clone()
    }
    pub async fn finish(&self, id: u32) {
        self.requests.lock().await.remove(&id);
    }
}

#[derive(Clone, Default)]
pub struct Operations(Arc<Mutex<HashMap<String, Arc<Scope>>>>);
impl Operations {
    pub async fn open(&self, id: &str) -> Result<Arc<Scope>> {
        let mut scopes = self.0.lock().await;
        if id.len() != 36 || !id.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-') {
            return Err(Error::new("Invalid Console lifecycle identity"));
        }
        if scopes.contains_key(id) || scopes.len() >= 64 {
            return Err(Error::new(
                "Console lifecycle already exists or too many windows are open",
            ));
        }
        let scope = Arc::new(Scope::default());
        scopes.insert(id.into(), scope.clone());
        Ok(scope)
    }
    pub async fn get(&self, id: &str) -> Result<Arc<Scope>> {
        self.0
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| Error::new("Console lifecycle is closed; request was not dispatched"))
    }
    pub async fn close(&self, id: &str) {
        if let Some(scope) = self.0.lock().await.remove(id) {
            scope.cancel.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn scopes_isolate_windows_and_preserve_early_cancellation() {
        let operations = Operations::default();
        let first = "00000000-0000-0000-0000-000000000001";
        let second = "00000000-0000-0000-0000-000000000002";
        let a = operations.open(first).await.unwrap();
        let b = operations.open(second).await.unwrap();
        a.request(1).await.cancel();
        assert!(a.request(1).await.is_cancelled());
        let sibling = a.request(2).await;
        let other_window = b.request(1).await;
        assert!(!sibling.is_cancelled());
        operations.close(first).await;
        assert!(sibling.is_cancelled());
        assert!(!other_window.is_cancelled());
        assert!(operations.get(first).await.is_err());
        assert!(a.request(3).await.is_cancelled());
        assert!(operations.open(second).await.is_err());
    }
}
