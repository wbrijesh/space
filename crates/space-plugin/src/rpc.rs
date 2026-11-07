//! A JSON-RPC 2.0 peer over newline-delimited streams (one message per line), as used between
//! the host and service plugins. Either side can send requests and notifications.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// A JSON-RPC error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL: i64 = -32603;
    /// The connection closed or the call timed out (not a JSON-RPC code).
    pub const UNAVAILABLE: i64 = -32000;

    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
    pub fn method_not_found(method: &str) -> Self {
        Self::new(Self::METHOD_NOT_FOUND, format!("method not found: {method}"))
    }
    pub fn invalid_params(message: impl std::fmt::Display) -> Self {
        Self::new(Self::INVALID_PARAMS, format!("invalid params: {message}"))
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(Self::INTERNAL, message)
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(Self::UNAVAILABLE, message)
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

/// Answers incoming requests: `(method, params) -> result`.
pub type RequestHandler = Arc<dyn Fn(String, Value) -> BoxFuture<'static, Result<Value, RpcError>> + Send + Sync>;
/// Receives incoming notifications.
pub type NotificationHandler = Arc<dyn Fn(String, Value) + Send + Sync>;

/// One end of a connection. Cheap to clone.
#[derive(Clone)]
pub struct Peer {
    inner: Arc<Inner>,
}

struct Inner {
    out: mpsc::UnboundedSender<String>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>,
    next_id: AtomicU64,
    closed: AtomicBool,
}

impl Peer {
    /// Start reading `reader` and writing `writer`. Must be called inside a tokio runtime. The
    /// returned task ends when `reader` reaches EOF; pending requests then fail.
    pub fn spawn<R, W>(
        reader: R,
        writer: W,
        on_request: RequestHandler,
        on_notification: NotificationHandler,
    ) -> (Peer, JoinHandle<()>)
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out, mut out_rx) = mpsc::unbounded_channel::<String>();
        let peer = Peer {
            inner: Arc::new(Inner {
                out,
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                closed: AtomicBool::new(false),
            }),
        };
        tokio::spawn(async move {
            let mut writer = writer;
            while let Some(mut line) = out_rx.recv().await {
                line.push('\n');
                if writer.write_all(line.as_bytes()).await.is_err() || writer.flush().await.is_err() {
                    break;
                }
            }
        });
        let reader_peer = peer.clone();
        let task = tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(&line) {
                    Ok(message) => reader_peer.dispatch(message, &on_request, &on_notification),
                    Err(e) => eprintln!("space-plugin: ignoring a line that is not JSON ({e})"),
                }
            }
            reader_peer.close();
        });
        (peer, task)
    }

    fn dispatch(&self, message: Value, on_request: &RequestHandler, on_notification: &NotificationHandler) {
        let method = message.get("method").and_then(Value::as_str).map(str::to_string);
        let id = message.get("id").cloned().filter(|id| !id.is_null());
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match (method, id) {
            (Some(method), Some(id)) => {
                let handler = on_request.clone();
                let out = self.inner.out.clone();
                tokio::spawn(async move {
                    let response = match handler(method, params).await {
                        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": e.code, "message": e.message}}),
                    };
                    let _ = out.send(response.to_string());
                });
            }
            (Some(method), None) => on_notification(method, params),
            (None, Some(id)) => {
                let Some(id) = id.as_u64() else { return };
                let Some(tx) = self.inner.pending.lock().unwrap().remove(&id) else { return };
                let result = match message.get("error") {
                    Some(err) => Err(RpcError::new(
                        err.get("code").and_then(Value::as_i64).unwrap_or(RpcError::INTERNAL),
                        err.get("message").and_then(Value::as_str).unwrap_or("error").to_string(),
                    )),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(result);
            }
            (None, None) => {}
        }
    }

    /// Fail every pending request; later requests fail at once.
    fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        for (_, tx) in self.inner.pending.lock().unwrap().drain() {
            let _ = tx.send(Err(RpcError::unavailable("connection closed")));
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// Send a request and wait for its result (`timeout`: give up after that long).
    pub async fn request(&self, method: &str, params: Value, timeout: Option<Duration>) -> Result<Value, RpcError> {
        if self.is_closed() {
            return Err(RpcError::unavailable("connection closed"));
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id, tx);
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if self.inner.out.send(message.to_string()).is_err() {
            self.inner.pending.lock().unwrap().remove(&id);
            return Err(RpcError::unavailable("connection closed"));
        }
        let result = match timeout {
            Some(t) => match tokio::time::timeout(t, rx).await {
                Ok(r) => r,
                Err(_) => {
                    self.inner.pending.lock().unwrap().remove(&id);
                    return Err(RpcError::unavailable(format!("{method} timed out after {}ms", t.as_millis())));
                }
            },
            None => rx.await,
        };
        result.unwrap_or_else(|_| Err(RpcError::unavailable("connection closed")))
    }

    /// Typed [`Peer::request`].
    pub async fn call<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        timeout: Option<Duration>,
    ) -> Result<R, RpcError> {
        let params = serde_json::to_value(params).map_err(RpcError::invalid_params)?;
        let value = self.request(method, params, timeout).await?;
        serde_json::from_value(value).map_err(|e| RpcError::internal(format!("bad {method} result: {e}")))
    }

    pub fn notify(&self, method: &str, params: impl Serialize) {
        let params = serde_json::to_value(params).unwrap_or(Value::Null);
        let _ = self.inner.out.send(json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string());
    }
}

/// Parse request params into `T`.
pub fn params<T: DeserializeOwned>(value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value).map_err(RpcError::invalid_params)
}

/// Serialize a handler result.
pub fn result<T: Serialize>(value: T) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|e| RpcError::internal(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(on_request: RequestHandler) -> (Peer, Peer, Arc<Mutex<Vec<String>>>) {
        let (a, b) = tokio::io::duplex(1 << 16);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let noop: RequestHandler = Arc::new(|m, _| Box::pin(async move { Err(RpcError::method_not_found(&m)) }));
        let (client, _) = Peer::spawn(ar, aw, noop, Arc::new(move |m, _| s.lock().unwrap().push(m)));
        let (server, _) = Peer::spawn(br, bw, on_request, Arc::new(|_, _| {}));
        (client, server, seen)
    }

    #[tokio::test]
    async fn requests_errors_notifications_and_timeouts() {
        let handler: RequestHandler = Arc::new(|method, params| {
            Box::pin(async move {
                match method.as_str() {
                    "echo" => Ok(params),
                    "slow" => {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        Ok(Value::Null)
                    }
                    _ => Err(RpcError::method_not_found(&method)),
                }
            })
        });
        let (client, server, seen) = pair(handler);
        assert_eq!(client.request("echo", json!({"a": 1}), None).await.unwrap(), json!({"a": 1}));
        let err = client.request("nope", Value::Null, None).await.unwrap_err();
        assert_eq!(err.code, RpcError::METHOD_NOT_FOUND);
        let err = client.request("slow", Value::Null, Some(Duration::from_millis(20))).await.unwrap_err();
        assert_eq!(err.code, RpcError::UNAVAILABLE);
        assert!(err.message.contains("timed out"));
        // Concurrent requests resolve independently.
        let (a, b) = tokio::join!(client.request("echo", json!(1), None), client.request("echo", json!(2), None));
        assert_eq!((a.unwrap(), b.unwrap()), (json!(1), json!(2)));
        server.notify("ping", json!({}));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(*seen.lock().unwrap(), ["ping"]);
    }

    #[tokio::test]
    async fn closed_connection_fails_pending_requests() {
        let (a, b) = tokio::io::duplex(1024);
        let (ar, aw) = tokio::io::split(a);
        let noop: RequestHandler = Arc::new(|m, _| Box::pin(async move { Err(RpcError::method_not_found(&m)) }));
        let (client, task) = Peer::spawn(ar, aw, noop, Arc::new(|_, _| {}));
        let pending = tokio::spawn({
            let client = client.clone();
            async move { client.request("hang", Value::Null, None).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(b); // the other side goes away without answering
        tokio::time::timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
        let err = tokio::time::timeout(Duration::from_secs(2), pending).await.unwrap().unwrap().unwrap_err();
        assert_eq!(err.code, RpcError::UNAVAILABLE);
        assert!(client.is_closed());
        assert!(client.request("again", Value::Null, None).await.is_err());
    }
}
