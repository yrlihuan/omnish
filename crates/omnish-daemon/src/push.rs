//! Daemon -> client broadcast helpers over the shared PushRegistry.

use omnish_protocol::message::Message;
use omnish_transport::rpc_server::PushRegistry;

/// Broadcast a generic ServerPush to every connected client. Clients filter
/// by `kind`/payload. Mirrors `deploy::broadcast_notice`. `payload` is encoded
/// to a JSON string because the wire type is `String` (bincode cannot carry
/// serde_json::Value).
pub async fn broadcast_server_push(registry: &PushRegistry, kind: &str, payload: serde_json::Value) {
    let payload = payload.to_string();
    let senders: Vec<_> = { registry.lock().await.values().cloned().collect() };
    for tx in senders {
        let _ = tx
            .send(Message::ServerPush { kind: kind.to_string(), payload: payload.clone() })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex};

    #[tokio::test]
    async fn broadcast_reaches_registered_sender() {
        let registry: PushRegistry = Arc::new(Mutex::new(HashMap::new()));
        let (tx, mut rx) = mpsc::channel(4);
        registry.lock().await.insert(1u64, tx);

        broadcast_server_push(&registry, "thread_title_update", serde_json::json!({ "thread_id": "t1", "title": "deploy" })).await;

        let msg = rx.recv().await.expect("message");
        match msg {
            Message::ServerPush { kind, payload } => {
                assert_eq!(kind, "thread_title_update");
                let v: serde_json::Value = serde_json::from_str(&payload).expect("parse payload");
                assert_eq!(v["thread_id"], "t1");
                assert_eq!(v["title"], "deploy");
            }
            _ => panic!("wrong variant"),
        }
    }
}
