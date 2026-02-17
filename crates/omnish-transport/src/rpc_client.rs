use anyhow::Result;
use omnish_protocol::message::{Frame, Message};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

struct WriteRequest {
    frame: Frame,
    reply_tx: oneshot::Sender<Message>,
}

pub struct RpcClient {
    tx: mpsc::Sender<WriteRequest>,
    next_id: AtomicU64,
    _write_task: JoinHandle<()>,
    _read_task: JoinHandle<()>,
}

impl RpcClient {
    pub async fn connect_unix(addr: &str) -> Result<Self> {
        let stream = UnixStream::connect(addr).await?;
        let (reader, writer) = stream.into_split();
        Self::from_split(reader, writer)
    }

    fn from_split(
        reader: tokio::net::unix::OwnedReadHalf,
        writer: tokio::net::unix::OwnedWriteHalf,
    ) -> Result<Self> {
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Message>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = mpsc::channel::<WriteRequest>(256);

        let write_pending = pending.clone();
        let _write_task = tokio::spawn(Self::write_loop(rx, writer, write_pending));

        let read_pending = pending.clone();
        let _read_task = tokio::spawn(Self::read_loop(reader, read_pending));

        Ok(Self {
            tx,
            next_id: AtomicU64::new(1),
            _write_task,
            _read_task,
        })
    }

    pub async fn call(&self, msg: Message) -> Result<Message> {
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = Frame {
            request_id,
            payload: msg,
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(WriteRequest { frame, reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("write task closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("read task closed before response"))
    }

    async fn write_loop(
        mut rx: mpsc::Receiver<WriteRequest>,
        mut writer: tokio::net::unix::OwnedWriteHalf,
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Message>>>>,
    ) {
        while let Some(req) = rx.recv().await {
            pending
                .lock()
                .await
                .insert(req.frame.request_id, req.reply_tx);
            let bytes = match req.frame.to_bytes() {
                Ok(b) => b,
                Err(_) => continue,
            };
            if writer.write_u32(bytes.len() as u32).await.is_err() {
                break;
            }
            if writer.write_all(&bytes).await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }
        }
    }

    async fn read_loop(
        mut reader: tokio::net::unix::OwnedReadHalf,
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Message>>>>,
    ) {
        loop {
            let len = match reader.read_u32().await {
                Ok(len) => len as usize,
                Err(_) => break,
            };
            let mut buf = vec![0u8; len];
            if reader.read_exact(&mut buf).await.is_err() {
                break;
            }
            let frame = match Frame::from_bytes(&buf) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let mut map = pending.lock().await;
            if let Some(tx) = map.remove(&frame.request_id) {
                let _ = tx.send(frame.payload);
            }
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
    use tokio::net::UnixListener;

    /// Helper: read one frame from a reader using the wire protocol [len:u32][frame_bytes]
    async fn read_frame(
        reader: &mut (impl AsyncReadExt + Unpin),
    ) -> Result<Frame> {
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
            // We expect exactly 2 frames (order may vary since they're sent concurrently,
            // but the write channel serializes them)
            let mut frames = Vec::new();
            for _ in 0..2 {
                let frame = read_frame(&mut stream).await.unwrap();
                frames.push(frame);
            }

            // Process frames: for Request, delay 50ms then send Response; for others, send Ack immediately
            // We process in order received but delay Request responses
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
}
