use anyhow::Result;
use async_trait::async_trait;
use omnish_protocol::message::Message;

#[async_trait]
pub trait Transport: Send + Sync {
    async fn connect(&self, addr: &str) -> Result<Box<dyn Connection>>;
    async fn listen(&self, addr: &str) -> Result<Box<dyn Listener>>;
}

#[async_trait]
pub trait Connection: Send + Sync {
    async fn send(&self, msg: &Message) -> Result<()>;
    async fn recv(&self) -> Result<Message>;
}

#[async_trait]
pub trait Listener: Send + Sync {
    async fn accept(&mut self) -> Result<Box<dyn Connection>>;
}
