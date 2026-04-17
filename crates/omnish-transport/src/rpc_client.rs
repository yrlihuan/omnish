use crate::{parse_addr, TransportAddr};
use anyhow::Result;
use omnish_protocol::message::{Frame, Message};

/// Return this error from the `on_reconnect` callback to signal a permanent
/// failure (e.g. auth rejected, protocol mismatch).  After several consecutive
/// permanent failures the reconnect loop gives up.
#[derive(Debug)]
pub struct PermanentFailure(pub String);

impl std::fmt::Display for PermanentFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for PermanentFailure {}
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;

enum ReplyTx {
    Once(oneshot::Sender<Message>),
    Stream(mpsc::Sender<Message>),
    None,
}

struct WriteRequest {
    frame: Frame,
    reply_tx: ReplyTx,
}

struct Inner {
    tx: mpsc::Sender<WriteRequest>,
    connected: Arc<AtomicBool>,
    _write_task: JoinHandle<()>,
    _read_task: JoinHandle<()>,
    _push_tx: mpsc::Sender<Message>,
}

/// Epoch millis after which reconnection is allowed again.
/// 0 means no suppression. Shared between RpcClient and the reconnect loop.
type SuppressUntil = Arc<AtomicU64>;

type ConnectorFn = Arc<
    dyn Fn()
            -> Pin<
                Box<
                    dyn Future<
                            Output = Result<(
                                Box<dyn AsyncRead + Unpin + Send>,
                                Box<dyn AsyncWrite + Unpin + Send>,
                            )>,
                        > + Send,
                >,
            > + Send
        + Sync,
>;

type ReconnectFn = Arc<
    dyn Fn(&RpcClient) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
        + Send
        + Sync,
>;

type NotifyFn = Arc<dyn Fn() + Send + Sync>;

fn make_connector(addr: &str, tls_connector: Option<TlsConnector>) -> ConnectorFn {
    let addr = addr.to_string();
    Arc::new(move || {
        let addr = addr.clone();
        let tls = tls_connector.clone();
        Box::pin(async move {
            match parse_addr(&addr) {
                TransportAddr::Unix(path) => {
                    let stream = UnixStream::connect(&path).await?;
                    let (r, w) = stream.into_split();
                    Ok((
                        Box::new(r) as Box<dyn AsyncRead + Unpin + Send>,
                        Box::new(w) as Box<dyn AsyncWrite + Unpin + Send>,
                    ))
                }
                TransportAddr::Tcp(hp) => {
                    let stream = TcpStream::connect(&hp).await?;
                    stream.set_nodelay(true)?;
                    if let Some(ref tls) = tls {
                        let domain = rustls::pki_types::ServerName::try_from("localhost")
                            .unwrap()
                            .to_owned();
                        let tls_stream = tls.connect(domain, stream).await?;
                        let (r, w) = tokio::io::split(tls_stream);
                        Ok((
                            Box::new(r) as Box<dyn AsyncRead + Unpin + Send>,
                            Box::new(w) as Box<dyn AsyncWrite + Unpin + Send>,
                        ))
                    } else {
                        let (r, w) = stream.into_split();
                        Ok((
                            Box::new(r) as Box<dyn AsyncRead + Unpin + Send>,
                            Box::new(w) as Box<dyn AsyncWrite + Unpin + Send>,
                        ))
                    }
                }
            }
        })
    })
}

#[derive(Clone)]
pub struct RpcClient {
    inner: Arc<Mutex<Inner>>,
    next_id: Arc<AtomicU64>,
    push_rx: Arc<Mutex<mpsc::Receiver<Message>>>,
    suppress_until: SuppressUntil,
}

impl RpcClient {
    pub async fn connect_unix(addr: &str) -> Result<Self> {
        let stream = UnixStream::connect(addr).await?;
        let (reader, writer) = stream.into_split();
        let (push_tx, push_rx) = mpsc::channel::<Message>(64);
        let inner = Self::create_inner(reader, writer, None, push_tx);
        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
            next_id: Arc::new(AtomicU64::new(1)),
            push_rx: Arc::new(Mutex::new(push_rx)),
            suppress_until: Arc::new(AtomicU64::new(0)),
        })
    }

    pub async fn connect_tcp(addr: &str) -> Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        stream.set_nodelay(true)?;
        let (reader, writer) = stream.into_split();
        let (push_tx, push_rx) = mpsc::channel::<Message>(64);
        let inner = Self::create_inner(reader, writer, None, push_tx);
        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
            next_id: Arc::new(AtomicU64::new(1)),
            push_rx: Arc::new(Mutex::new(push_rx)),
            suppress_until: Arc::new(AtomicU64::new(0)),
        })
    }

    pub async fn connect(addr: &str) -> Result<Self> {
        match parse_addr(addr) {
            TransportAddr::Unix(p) => Self::connect_unix(&p).await,
            TransportAddr::Tcp(hp) => Self::connect_tcp(&hp).await,
        }
    }

    fn create_inner<R, W>(
        reader: R,
        writer: W,
        disconnect_tx: Option<oneshot::Sender<()>>,
        push_tx: mpsc::Sender<Message>,
    ) -> Inner
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let pending: Arc<Mutex<HashMap<u64, ReplyTx>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = mpsc::channel::<WriteRequest>(256);

        let connected = Arc::new(AtomicBool::new(true));

        let write_connected = connected.clone();
        let write_pending = pending.clone();
        let _write_task =
            tokio::spawn(Self::write_loop(rx, writer, write_pending, write_connected));

        let read_connected = connected.clone();
        let read_pending = pending.clone();
        let _read_task = tokio::spawn(Self::read_loop(
            reader,
            read_pending,
            read_connected,
            disconnect_tx,
            push_tx.clone(),
        ));

        Inner {
            tx,
            connected,
            _write_task,
            _read_task,
            _push_tx: push_tx,
        }
    }

    pub async fn connect_unix_with_reconnect(
        addr: &str,
        on_reconnect: impl Fn(&RpcClient) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
            + Send
            + Sync
            + 'static,
    ) -> Result<Self> {
        Self::connect_with_reconnect(addr, None, on_reconnect).await
    }

    pub async fn connect_with_reconnect(
        addr: &str,
        tls_connector: Option<TlsConnector>,
        on_reconnect: impl Fn(&RpcClient) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
            + Send
            + Sync
            + 'static,
    ) -> Result<Self> {
        Self::connect_with_reconnect_notify(addr, tls_connector, on_reconnect, None::<fn()>).await
    }

    pub async fn connect_with_reconnect_notify(
        addr: &str,
        tls_connector: Option<TlsConnector>,
        on_reconnect: impl Fn(&RpcClient) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
            + Send
            + Sync
            + 'static,
        on_reconnect_notify: Option<impl Fn() + Send + Sync + 'static>,
    ) -> Result<Self> {
        Self::connect_with_reconnect_full(addr, tls_connector, on_reconnect, on_reconnect_notify, None::<fn()>).await
    }

    pub async fn connect_with_reconnect_full(
        addr: &str,
        tls_connector: Option<TlsConnector>,
        on_reconnect: impl Fn(&RpcClient) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
            + Send
            + Sync
            + 'static,
        on_reconnect_notify: Option<impl Fn() + Send + Sync + 'static>,
        on_disconnect: Option<impl Fn() + Send + Sync + 'static>,
    ) -> Result<Self> {
        let notify_fn: Option<NotifyFn> = on_reconnect_notify.map(|f| Arc::new(f) as NotifyFn);
        let disconnect_fn: Option<NotifyFn> = on_disconnect.map(|f| Arc::new(f) as NotifyFn);
        let connector = make_connector(addr, tls_connector);

        // Try initial connection
        let initial_connection_result = connector().await;

        match initial_connection_result {
            Ok((reader, writer)) => {
                // Initial connection succeeded - normal flow
                let (disc_tx, disc_rx) = oneshot::channel::<()>();
                let (push_tx, push_rx) = mpsc::channel::<Message>(64);
                let inner = Self::create_inner(reader, writer, Some(disc_tx), push_tx);
                let next_id = Arc::new(AtomicU64::new(1));
                let push_rx = Arc::new(Mutex::new(push_rx));
                let suppress_until = Arc::new(AtomicU64::new(0));
                let client = Self {
                    inner: Arc::new(Mutex::new(inner)),
                    next_id: next_id.clone(),
                    push_rx: push_rx.clone(),
                    suppress_until: suppress_until.clone(),
                };

                // Call on_reconnect for initial registration
                on_reconnect(&client).await?;

                // Spawn reconnect loop
                let inner_ref = client.inner.clone();
                let next_id_ref = next_id.clone();
                let on_reconnect = Arc::new(on_reconnect);

                tokio::spawn(Self::reconnect_loop(
                    inner_ref,
                    next_id_ref,
                    connector,
                    on_reconnect,
                    notify_fn.clone(),
                    disconnect_fn.clone(),
                    disc_rx,
                    push_rx,
                    suppress_until,
                ));

                Ok(client)
            }
            Err(_) => {
                // Initial connection failed - create a disconnected client
                // that will attempt to reconnect immediately
                tracing::debug!("Initial connection to {} failed, creating disconnected client", addr);

                // Create a disconnected inner state
                let (tx, _rx) = mpsc::channel::<WriteRequest>(256);
                let connected = Arc::new(AtomicBool::new(false));
                let (push_tx, push_rx) = mpsc::channel::<Message>(64);

                let inner = Inner {
                    tx,
                    connected,
                    _write_task: tokio::spawn(async {}), // dummy task
                    _read_task: tokio::spawn(async {}),  // dummy task
                    _push_tx: push_tx,
                };

                let next_id = Arc::new(AtomicU64::new(1));
                let push_rx = Arc::new(Mutex::new(push_rx));
                let suppress_until = Arc::new(AtomicU64::new(0));
                let client = Self {
                    inner: Arc::new(Mutex::new(inner)),
                    next_id: next_id.clone(),
                    push_rx: push_rx.clone(),
                    suppress_until: suppress_until.clone(),
                };

                // Create a oneshot channel that's already closed to trigger immediate reconnection
                let (disc_tx, disc_rx) = oneshot::channel::<()>();
                let _ = disc_tx.send(()); // Immediately trigger disconnect

                // Spawn reconnect loop
                let inner_ref = client.inner.clone();
                let next_id_ref = next_id.clone();
                let on_reconnect = Arc::new(on_reconnect);

                tokio::spawn(Self::reconnect_loop(
                    inner_ref,
                    next_id_ref,
                    connector,
                    on_reconnect,
                    notify_fn,
                    disconnect_fn,
                    disc_rx,
                    push_rx,
                    suppress_until,
                ));

                Ok(client)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn reconnect_loop(
        inner_ref: Arc<Mutex<Inner>>,
        next_id: Arc<AtomicU64>,
        connector: ConnectorFn,
        on_reconnect: ReconnectFn,
        on_notify: Option<NotifyFn>,
        on_disconnect: Option<NotifyFn>,
        mut disc_rx: oneshot::Receiver<()>,
        push_rx: Arc<Mutex<mpsc::Receiver<Message>>>,
        suppress_until: SuppressUntil,
    ) {
        loop {
            // Wait for disconnect notification
            let _ = (&mut disc_rx).await;

            // Notify caller of disconnect
            if let Some(ref disc_fn) = on_disconnect {
                disc_fn();
            }

            // Mark as disconnected
            {
                let guard = inner_ref.lock().await;
                guard.connected.store(false, Ordering::SeqCst);
            }

            // Wait for suppress period to elapse (set by TestDisconnect)
            let until = suppress_until.load(Ordering::Relaxed);
            if until > 0 {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                if now < until {
                    let wait = std::time::Duration::from_millis(until - now);
                    tracing::info!("reconnect suppressed for {}ms", wait.as_millis());
                    tokio::time::sleep(wait).await;
                }
                suppress_until.store(0, Ordering::Relaxed);
            }

            // Exponential backoff reconnection
            let mut backoff_ms: u64 = 1000;
            let max_backoff_ms: u64 = 30_000;
            let mut consecutive_auth_failures: u32 = 0;
            const MAX_AUTH_FAILURES: u32 = 5;

            loop {
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;

                // Try to connect
                let (reader, writer) = match connector().await {
                    Ok(rw) => rw,
                    Err(_) => {
                        // Connection failure resets auth failure counter (daemon may be down)
                        consecutive_auth_failures = 0;
                        backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
                        continue;
                    }
                };

                let (new_disc_tx, new_disc_rx) = oneshot::channel::<()>();
                let (new_push_tx, new_push_rx) = mpsc::channel::<Message>(64);
                let new_inner = Self::create_inner(reader, writer, Some(new_disc_tx), new_push_tx);

                // Create a temporary client wrapping the new inner for the callback
                let dummy_push_rx = Arc::new(Mutex::new(mpsc::channel::<Message>(1).1));
                let temp_client = RpcClient {
                    inner: Arc::new(Mutex::new(new_inner)),
                    next_id: next_id.clone(),
                    push_rx: dummy_push_rx,
                    suppress_until: Arc::new(AtomicU64::new(0)),
                };

                // Call on_reconnect with the temp client
                if let Err(e) = on_reconnect(&temp_client).await {
                    if e.downcast_ref::<PermanentFailure>().is_some() {
                        consecutive_auth_failures += 1;
                        if consecutive_auth_failures >= MAX_AUTH_FAILURES {
                            tracing::warn!(
                                "giving up reconnection after {} consecutive auth failures",
                                consecutive_auth_failures
                            );
                            return;
                        }
                    }
                    backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
                    continue;
                }
                consecutive_auth_failures = 0;

                // Success - swap the inner
                let temp_inner_arc = temp_client.inner.clone();
                drop(temp_client);
                let temp_inner_mutex = match Arc::try_unwrap(temp_inner_arc) {
                    Ok(m) => m,
                    Err(_) => {
                        backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
                        continue;
                    }
                };
                let new_inner = temp_inner_mutex.into_inner();

                // Swap into the real client's inner
                {
                    let mut guard = inner_ref.lock().await;
                    *guard = new_inner;
                }

                // Swap push_rx to match the new inner's push_tx
                {
                    let mut rx_guard = push_rx.lock().await;
                    *rx_guard = new_push_rx;
                }

                // Notify caller of successful reconnection
                if let Some(ref notify) = on_notify {
                    notify();
                }

                // Update disc_rx for next iteration
                disc_rx = new_disc_rx;
                break;
            }
        }
    }

    pub async fn is_connected(&self) -> bool {
        self.inner.lock().await.connected.load(Ordering::SeqCst)
    }

    /// Non-blocking receive of a push message (server-initiated, request_id=0).
    pub async fn try_recv_push(&self) -> Option<Message> {
        self.push_rx.lock().await.try_recv().ok()
    }

    /// Suppress reconnection attempts for the given duration.
    /// Used by TestDisconnect to delay reconnection.
    pub fn suppress_reconnect(&self, duration: std::time::Duration) {
        let until = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
            + duration.as_millis() as u64;
        self.suppress_until.store(until, Ordering::Relaxed);
    }

    pub async fn call(&self, msg: Message) -> Result<Message> {
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = Frame {
            request_id,
            payload: msg,
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        let tx = {
            let inner = self.inner.lock().await;
            if !inner.connected.load(Ordering::SeqCst) {
                return Err(anyhow::anyhow!("not connected"));
            }
            inner.tx.clone()
        };
        tx.send(WriteRequest { frame, reply_tx: ReplyTx::Once(reply_tx) })
            .await
            .map_err(|_| anyhow::anyhow!("write task closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("read task closed before response"))
    }

    /// Fire-and-forget: send a message without waiting for a response.
    /// Returns Ok(()) once the message is queued for writing.
    pub async fn send(&self, msg: Message) -> Result<()> {
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = Frame {
            request_id,
            payload: msg,
        };

        let tx = {
            let inner = self.inner.lock().await;
            if !inner.connected.load(Ordering::SeqCst) {
                return Err(anyhow::anyhow!("not connected"));
            }
            inner.tx.clone()
        };
        tx.send(WriteRequest { frame, reply_tx: ReplyTx::None })
            .await
            .map_err(|_| anyhow::anyhow!("write task closed"))?;
        Ok(())
    }

    /// Send a message and receive multiple responses (for streaming).
    pub async fn call_stream(&self, msg: Message) -> Result<mpsc::Receiver<Message>> {
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = Frame { request_id, payload: msg };
        let (reply_tx, reply_rx) = mpsc::channel(128);

        let tx = {
            let inner = self.inner.lock().await;
            if !inner.connected.load(Ordering::SeqCst) {
                return Err(anyhow::anyhow!("not connected"));
            }
            inner.tx.clone()
        };
        tx.send(WriteRequest { frame, reply_tx: ReplyTx::Stream(reply_tx) }).await
            .map_err(|_| anyhow::anyhow!("write task closed"))?;

        Ok(reply_rx)
    }

    async fn write_loop<W: AsyncWrite + Unpin>(
        mut rx: mpsc::Receiver<WriteRequest>,
        mut writer: W,
        pending: Arc<Mutex<HashMap<u64, ReplyTx>>>,
        connected: Arc<AtomicBool>,
    ) {
        while let Some(req) = rx.recv().await {
            let bytes = match req.frame.to_bytes() {
                Ok(b) => b,
                Err(_) => {
                    drop(req.reply_tx);
                    continue;
                }
            };
            if !matches!(req.reply_tx, ReplyTx::None) {
                pending
                    .lock()
                    .await
                    .insert(req.frame.request_id, req.reply_tx);
            }
            if writer.write_u32(bytes.len() as u32).await.is_err() {
                connected.store(false, Ordering::SeqCst);
                break;
            }
            if writer.write_all(&bytes).await.is_err() {
                connected.store(false, Ordering::SeqCst);
                break;
            }
            if writer.flush().await.is_err() {
                connected.store(false, Ordering::SeqCst);
                break;
            }
        }
        // Drop all pending reply senders so callers unblock on write failure too.
        pending.lock().await.clear();
    }

    async fn read_loop<R: AsyncRead + Unpin>(
        mut reader: R,
        pending: Arc<Mutex<HashMap<u64, ReplyTx>>>,
        connected: Arc<AtomicBool>,
        disconnect_tx: Option<oneshot::Sender<()>>,
        push_tx: mpsc::Sender<Message>,
    ) {
        while let Ok(l) = reader.read_u32().await {
            let len = l as usize;
            let mut buf = vec![0u8; len];
            if reader.read_exact(&mut buf).await.is_err() {
                break;
            }
            let frame = match Frame::from_bytes(&buf) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!("rpc frame parse error ({} bytes): {}", len, e);
                    continue;
                }
            };
            // Server-initiated push messages use request_id 0
            if frame.request_id == 0 {
                let _ = push_tx.send(frame.payload).await;
                continue;
            }
            // Dispatch under lock, but for Stream sends we clone the sender
            // and release the lock before the async send to avoid blocking
            // the read loop while the consumer is slow.
            let stream_send = {
                let mut map = pending.lock().await;
                if let Some(tx) = map.get(&frame.request_id) {
                    match tx {
                        ReplyTx::Once(_) => {
                            if let Some(ReplyTx::Once(tx)) = map.remove(&frame.request_id) {
                                let _ = tx.send(frame.payload);
                            }
                            None
                        }
                        ReplyTx::Stream(tx) => {
                            if matches!(frame.payload, Message::Ack) {
                                // End-of-stream sentinel - remove entry, dropping sender
                                map.remove(&frame.request_id);
                                None
                            } else {
                                // Clone sender and release lock before async send
                                Some((tx.clone(), frame))
                            }
                        }
                        ReplyTx::None => {
                            // Fire-and-forget - discard response, clean up
                            map.remove(&frame.request_id);
                            None
                        }
                    }
                } else {
                    None
                }
            }; // lock released here
            if let Some((tx, frame)) = stream_send {
                match tx.try_send(frame.payload) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(payload)) => {
                        tracing::debug!("stream channel full (request_id={}), waiting", frame.request_id);
                        if tx.send(payload).await.is_err() {
                            pending.lock().await.remove(&frame.request_id);
                        }
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        pending.lock().await.remove(&frame.request_id);
                    }
                }
            }
        }
        connected.store(false, Ordering::SeqCst);
        // Drop all pending reply senders so callers' reply_rx.await unblocks
        // with RecvError instead of hanging forever.
        pending.lock().await.clear();
        if let Some(tx) = disconnect_tx {
            let _ = tx.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnish_protocol::message::{
        IoData, IoDirection, Request, RequestScope, Response, SessionStart,
    };
    use std::collections::HashMap;
    use tokio::net::{TcpListener, UnixListener};

    /// Helper: read one frame from a reader using the wire protocol [len:u32][frame_bytes]
    async fn read_frame(reader: &mut (impl AsyncReadExt + Unpin)) -> Result<Frame> {
        let len = reader.read_u32().await? as usize;
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).await?;
        Frame::from_bytes(&buf)
    }

    /// Helper: write one frame to a writer using the wire protocol [len:u32][frame_bytes]
    async fn write_frame(
        writer: &mut (impl AsyncWriteExt + Unpin),
        frame: &Frame,
    ) -> Result<()> {
        let bytes = frame.to_bytes()?;
        writer.write_u32(bytes.len() as u32).await?;
        writer.write_all(&bytes).await?;
        writer.flush().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_rpc_client_call_returns_ack() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");
        let sock_path_str = sock_path.to_str().unwrap().to_string();

        let listener = UnixListener::bind(&sock_path).unwrap();

        // Spawn a minimal echo server that replies with Ack
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let frame = read_frame(&mut stream).await.unwrap();
            assert!(matches!(frame.payload, Message::SessionStart(_)));
            let reply = Frame {
                request_id: frame.request_id,
                payload: Message::Ack,
            };
            write_frame(&mut stream, &reply).await.unwrap();
        });

        let client = RpcClient::connect_unix(&sock_path_str).await.unwrap();
        let msg = Message::SessionStart(SessionStart {
            session_id: "s1".to_string(),
            parent_session_id: None,
            timestamp_ms: 1000,
            attrs: HashMap::new(),
        });
        let resp = client.call(msg).await.unwrap();
        assert!(matches!(resp, Message::Ack));

        server.await.unwrap();
    }

    #[tokio::test]
    async fn test_rpc_client_concurrent_calls() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test_concurrent.sock");
        let sock_path_str = sock_path.to_str().unwrap().to_string();

        let listener = UnixListener::bind(&sock_path).unwrap();

        // Server: delays 50ms for Request messages, instant Ack for others
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut frames = Vec::new();
            for _ in 0..2 {
                let frame = read_frame(&mut stream).await.unwrap();
                frames.push(frame);
            }

            for frame in frames {
                match &frame.payload {
                    Message::Request(req) => {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        let reply = Frame {
                            request_id: frame.request_id,
                            payload: Message::Response(Response {
                                request_id: req.request_id.clone(),
                                content: "answer".to_string(),
                                is_streaming: false,
                                is_final: true,
                            }),
                        };
                        write_frame(&mut stream, &reply).await.unwrap();
                    }
                    _ => {
                        let reply = Frame {
                            request_id: frame.request_id,
                            payload: Message::Ack,
                        };
                        write_frame(&mut stream, &reply).await.unwrap();
                    }
                }
            }
        });

        let client = RpcClient::connect_unix(&sock_path_str).await.unwrap();

        let io_msg = Message::IoData(IoData {
            session_id: "s1".to_string(),
            direction: IoDirection::Output,
            timestamp_ms: 2000,
            data: b"hello".to_vec(),
        });
        let req_msg = Message::Request(Request {
            request_id: "r1".to_string(),
            session_id: "s1".to_string(),
            query: "what happened?".to_string(),
            scope: RequestScope::CurrentSession,
        });

        let (io_resp, req_resp) = tokio::join!(client.call(io_msg), client.call(req_msg));

        let io_resp = io_resp.unwrap();
        let req_resp = req_resp.unwrap();

        assert!(matches!(io_resp, Message::Ack));
        assert!(matches!(req_resp, Message::Response(_)));
        if let Message::Response(r) = req_resp {
            assert_eq!(r.content, "answer");
        }

        server.await.unwrap();
    }

    #[tokio::test]
    async fn test_rpc_client_reconnects_after_server_drop() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("reconnect.sock");
        let sock_path_str = sock_path.to_str().unwrap().to_string();

        // Start first server
        let listener1 = UnixListener::bind(&sock_path).unwrap();
        let server1 = tokio::spawn(async move {
            let (mut stream, _) = listener1.accept().await.unwrap();
            let frame = read_frame(&mut stream).await.unwrap();
            assert!(matches!(frame.payload, Message::SessionStart(_)));
            write_frame(
                &mut stream,
                &Frame {
                    request_id: frame.request_id,
                    payload: Message::Ack,
                },
            )
            .await
            .unwrap();
            let frame = read_frame(&mut stream).await.unwrap();
            assert!(matches!(frame.payload, Message::IoData(_)));
            write_frame(
                &mut stream,
                &Frame {
                    request_id: frame.request_id,
                    payload: Message::Ack,
                },
            )
            .await
            .unwrap();
            // Drop stream to simulate disconnect
        });

        let reconnect_count = Arc::new(AtomicU64::new(0));
        let reconnect_count_clone = reconnect_count.clone();

        let client = RpcClient::connect_unix_with_reconnect(&sock_path_str, move |rpc| {
            let count = reconnect_count_clone.clone();
            let rpc = rpc.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::Relaxed);
                rpc.call(Message::SessionStart(SessionStart {
                    session_id: "s1".to_string(),
                    parent_session_id: None,
                    timestamp_ms: 1000,
                    attrs: HashMap::new(),
                }))
                .await?;
                Ok(())
            })
        })
        .await
        .unwrap();

        // First call should succeed
        let resp = client
            .call(Message::IoData(IoData {
                session_id: "s1".to_string(),
                direction: IoDirection::Input,
                timestamp_ms: 2000,
                data: b"ls".to_vec(),
            }))
            .await
            .unwrap();
        assert!(matches!(resp, Message::Ack));

        // Wait for server1 to drop connection
        server1.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Calls during disconnection should fail
        let result = client
            .call(Message::IoData(IoData {
                session_id: "s1".to_string(),
                direction: IoDirection::Input,
                timestamp_ms: 3000,
                data: b"pwd".to_vec(),
            }))
            .await;
        assert!(result.is_err());

        // Start second server on same socket
        let _ = std::fs::remove_file(&sock_path);
        let listener2 = UnixListener::bind(&sock_path).unwrap();
        let server2 = tokio::spawn(async move {
            let (mut stream, _) = listener2.accept().await.unwrap();
            let frame = read_frame(&mut stream).await.unwrap();
            assert!(matches!(frame.payload, Message::SessionStart(_)));
            write_frame(
                &mut stream,
                &Frame {
                    request_id: frame.request_id,
                    payload: Message::Ack,
                },
            )
            .await
            .unwrap();
            let frame = read_frame(&mut stream).await.unwrap();
            assert!(matches!(frame.payload, Message::IoData(_)));
            write_frame(
                &mut stream,
                &Frame {
                    request_id: frame.request_id,
                    payload: Message::Ack,
                },
            )
            .await
            .unwrap();
        });

        // Wait for reconnection (backoff starts at 1s)
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

        // After reconnection, calls should succeed
        let resp = client
            .call(Message::IoData(IoData {
                session_id: "s1".to_string(),
                direction: IoDirection::Input,
                timestamp_ms: 4000,
                data: b"whoami".to_vec(),
            }))
            .await
            .unwrap();
        assert!(matches!(resp, Message::Ack));

        // on_reconnect called twice (initial + reconnect)
        assert_eq!(reconnect_count.load(Ordering::Relaxed), 2);

        server2.await.unwrap();
    }

    #[tokio::test]
    async fn test_rpc_client_is_connected() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("is_connected.sock");
        let sock_path_str = sock_path.to_str().unwrap().to_string();

        let listener = UnixListener::bind(&sock_path).unwrap();

        // Server: accept, handle one SessionStart, then drop
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let frame = read_frame(&mut stream).await.unwrap();
            write_frame(
                &mut stream,
                &Frame {
                    request_id: frame.request_id,
                    payload: Message::Ack,
                },
            )
            .await
            .unwrap();
            // Keep connection alive briefly
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            // Drop stream to disconnect
        });

        let client = RpcClient::connect_unix_with_reconnect(&sock_path_str, move |rpc| {
            let rpc = rpc.clone();
            Box::pin(async move {
                rpc.call(Message::SessionStart(SessionStart {
                    session_id: "s1".to_string(),
                    parent_session_id: None,
                    timestamp_ms: 1000,
                    attrs: HashMap::new(),
                }))
                .await?;
                Ok(())
            })
        })
        .await
        .unwrap();

        // Should be connected after initial connect
        assert!(client.is_connected().await);

        // Wait for server to drop
        server.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Should be disconnected after server drops
        assert!(!client.is_connected().await);
    }

    #[tokio::test]
    async fn test_rpc_client_tcp_call_returns_ack() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let frame = read_frame(&mut stream).await.unwrap();
            assert!(matches!(frame.payload, Message::SessionStart(_)));
            let reply = Frame {
                request_id: frame.request_id,
                payload: Message::Ack,
            };
            write_frame(&mut stream, &reply).await.unwrap();
        });

        let client = RpcClient::connect_tcp(&addr).await.unwrap();
        let msg = Message::SessionStart(SessionStart {
            session_id: "s1".to_string(),
            parent_session_id: None,
            timestamp_ms: 1000,
            attrs: HashMap::new(),
        });
        let resp = client.call(msg).await.unwrap();
        assert!(matches!(resp, Message::Ack));

        server.await.unwrap();
    }

    #[tokio::test]
    async fn test_rpc_client_tcp_concurrent_calls() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut frames = Vec::new();
            for _ in 0..2 {
                let frame = read_frame(&mut stream).await.unwrap();
                frames.push(frame);
            }
            for frame in frames {
                match &frame.payload {
                    Message::Request(req) => {
                        let reply = Frame {
                            request_id: frame.request_id,
                            payload: Message::Response(Response {
                                request_id: req.request_id.clone(),
                                content: "tcp answer".to_string(),
                                is_streaming: false,
                                is_final: true,
                            }),
                        };
                        write_frame(&mut stream, &reply).await.unwrap();
                    }
                    _ => {
                        let reply = Frame {
                            request_id: frame.request_id,
                            payload: Message::Ack,
                        };
                        write_frame(&mut stream, &reply).await.unwrap();
                    }
                }
            }
        });

        let client = RpcClient::connect_tcp(&addr).await.unwrap();

        let io_msg = Message::IoData(IoData {
            session_id: "s1".to_string(),
            direction: IoDirection::Output,
            timestamp_ms: 2000,
            data: b"hello".to_vec(),
        });
        let req_msg = Message::Request(Request {
            request_id: "r1".to_string(),
            session_id: "s1".to_string(),
            query: "tcp test".to_string(),
            scope: RequestScope::CurrentSession,
        });

        let (io_resp, req_resp) = tokio::join!(client.call(io_msg), client.call(req_msg));
        assert!(matches!(io_resp.unwrap(), Message::Ack));
        if let Message::Response(r) = req_resp.unwrap() {
            assert_eq!(r.content, "tcp answer");
        } else {
            panic!("expected Response");
        }

        server.await.unwrap();
    }

    #[tokio::test]
    async fn test_rpc_client_connect_auto_dispatch() {
        // TCP address (contains ':')
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let frame = read_frame(&mut stream).await.unwrap();
            write_frame(
                &mut stream,
                &Frame {
                    request_id: frame.request_id,
                    payload: Message::Ack,
                },
            )
            .await
            .unwrap();
        });

        // Use the auto-dispatch connect() method
        let client = RpcClient::connect(&addr).await.unwrap();
        let msg = Message::SessionStart(SessionStart {
            session_id: "s1".to_string(),
            parent_session_id: None,
            timestamp_ms: 1000,
            attrs: HashMap::new(),
        });
        let resp = client.call(msg).await.unwrap();
        assert!(matches!(resp, Message::Ack));

        server.await.unwrap();
    }
}
