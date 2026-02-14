use omnish_protocol::message::*;
use omnish_transport::unix::UnixTransport;
use omnish_transport::traits::Transport;
use std::collections::HashMap;

#[tokio::test]
async fn test_unix_send_recv() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("test.sock");
    let addr = sock_path.to_str().unwrap().to_string();

    let transport = UnixTransport;

    let mut listener = transport.listen(&addr).await.unwrap();

    let addr2 = addr.clone();
    let handle = tokio::spawn(async move {
        let conn = UnixTransport.connect(&addr2).await.unwrap();
        let msg = Message::SessionStart(SessionStart {
            session_id: "test".to_string(),
            parent_session_id: None,
            timestamp_ms: 0,
            attrs: HashMap::from([
                ("shell".to_string(), "/bin/bash".to_string()),
                ("pid".to_string(), "42".to_string()),
                ("tty".to_string(), "/dev/pts/0".to_string()),
                ("cwd".to_string(), "/tmp".to_string()),
            ]),
        });
        conn.send(&msg).await.unwrap();
    });

    let conn = listener.accept().await.unwrap();
    let msg = conn.recv().await.unwrap();
    match msg {
        Message::SessionStart(s) => assert_eq!(s.session_id, "test"),
        _ => panic!("wrong message type"),
    }

    handle.await.unwrap();
}
