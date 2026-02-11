use crate::traits::{Connection, Listener, Transport};
use anyhow::Result;
use async_trait::async_trait;
use omnish_protocol::message::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener as TokioUnixListener, UnixStream};
use tokio::sync::Mutex;

pub struct UnixTransport;

#[async_trait]
impl Transport for UnixTransport {
    async fn connect(&self, addr: &str) -> Result<Box<dyn Connection>> {
        let stream = UnixStream::connect(addr).await?;
        Ok(Box::new(UnixConnection {
            stream: Mutex::new(stream),
        }))
    }

    async fn listen(&self, addr: &str) -> Result<Box<dyn Listener>> {
        let _ = std::fs::remove_file(addr);
        let listener = TokioUnixListener::bind(addr)?;
        Ok(Box::new(UnixListener { listener }))
    }
}

struct UnixConnection {
    stream: Mutex<UnixStream>,
}

#[async_trait]
impl Connection for UnixConnection {
    async fn send(&self, msg: &Message) -> Result<()> {
        let bytes = msg.to_bytes()?;
        let mut stream = self.stream.lock().await;
        stream.write_u32(bytes.len() as u32).await?;
        stream.write_all(&bytes).await?;
        stream.flush().await?;
        Ok(())
    }

    async fn recv(&self) -> Result<Message> {
        let mut stream = self.stream.lock().await;
        let len = stream.read_u32().await? as usize;
        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).await?;
        Message::from_bytes(&buf)
    }
}

struct UnixListener {
    listener: TokioUnixListener,
}

#[async_trait]
impl Listener for UnixListener {
    async fn accept(&mut self) -> Result<Box<dyn Connection>> {
        let (stream, _) = self.listener.accept().await?;
        Ok(Box::new(UnixConnection {
            stream: Mutex::new(stream),
        }))
    }
}
