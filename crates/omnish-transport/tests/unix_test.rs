use omnish_protocol::message::*;
use omnish_transport::unix::UnixTransport;
use omnish_transport::traits::{Transport, Listener};

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
            shell: "/bin/bash".to_string(),
            pid: 42,
            tty: "/dev/pts/0".to_string(),
            timestamp_ms: 0,
            cwd: "/tmp".to_string(),
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
