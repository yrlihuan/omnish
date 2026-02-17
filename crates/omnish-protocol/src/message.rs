use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const MAGIC: [u8; 2] = [0x4F, 0x53]; // "OS" for OmniSh

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    SessionStart(SessionStart),
    SessionEnd(SessionEnd),
    IoData(IoData),
    Event(Event),
    Request(Request),
    Response(Response),
    CommandComplete(CommandComplete),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStart {
    pub session_id: String,
    #[serde(default)]
    pub parent_session_id: Option<String>,
    pub timestamp_ms: u64,
    #[serde(default)]
    pub attrs: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEnd {
    pub session_id: String,
    pub timestamp_ms: u64,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IoData {
    pub session_id: String,
    pub direction: IoDirection,
    pub timestamp_ms: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IoDirection {
    Input,
    Output,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub session_id: String,
    pub timestamp_ms: u64,
    pub event_type: EventType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventType {
    NonZeroExit(i32),
    PatternMatch(String),
    CommandBoundary { command: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub request_id: String,
    pub session_id: String,
    pub query: String,
    pub scope: RequestScope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RequestScope {
    CurrentSession,
    AllSessions,
    Sessions(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub request_id: String,
    pub content: String,
    pub is_streaming: bool,
    pub is_final: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandComplete {
    pub session_id: String,
    pub record: omnish_store::command::CommandRecord,
}

impl Message {
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let payload = bincode::serialize(self)?;
        let len = payload.len() as u32;
        let mut buf = Vec::with_capacity(2 + 4 + payload.len());
        buf.extend_from_slice(&MAGIC);
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&payload);
        Ok(buf)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 6 {
            bail!("message too short");
        }
        if bytes[0..2] != MAGIC {
            bail!("invalid magic bytes");
        }
        let len = u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]) as usize;
        if bytes.len() < 6 + len {
            bail!("message truncated");
        }
        let msg: Message = bincode::deserialize(&bytes[6..6 + len])?;
        Ok(msg)
    }
}
