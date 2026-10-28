//! A TLS handshake that never progresses (a peer whose network dropped right
//! after the TCP connect) must not block anything for long:
//! - on the server it must not keep other clients from connecting;
//! - on the client it must fail within CONNECT_TIMEOUT so the reconnect loop
//!   tries again, instead of waiting until TCP gives up (12+ minutes seen on
//!   macOS).

use omnish_protocol::message::Message;
use omnish_transport::rpc_client::RpcClient;
use omnish_transport::rpc_server::RpcServer;
use omnish_transport::{tls, CONNECT_TIMEOUT};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

#[tokio::test]
async fn stalled_handshake_does_not_block_other_clients() {
    let dir = tempfile::tempdir().unwrap();
    let tls_dir = dir.path().join("tls");
    let acceptor = tls::make_acceptor(&tls_dir).unwrap();
    let connector = tls::make_connector(&tls_dir.join("cert.pem")).unwrap();

    let mut server = RpcServer::bind_tcp("127.0.0.1:0").await.unwrap();
    let addr = server.local_tcp_addr().unwrap();
    tokio::spawn(async move {
        let _ = server
            .serve(
                |_msg: Message, _tx: mpsc::Sender<Message>| Box::pin(async {}),
                None,
                Some(acceptor),
                None,
                None,
                None,
            )
            .await;
    });

    // Connects over TCP and then sends nothing: its handshake never ends.
    let _stalled = TcpStream::connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Another client must still get through right away.
    let tcp = TcpStream::connect(addr).await.unwrap();
    let domain = rustls::pki_types::ServerName::try_from("localhost").unwrap().to_owned();
    let done = tokio::time::timeout(Duration::from_secs(3), connector.connect(domain, tcp)).await;
    assert!(
        matches!(done, Ok(Ok(_))),
        "second client's handshake was blocked by the stalled one"
    );
}

#[tokio::test]
async fn client_connect_gives_up_on_a_silent_handshake() {
    let dir = tempfile::tempdir().unwrap();
    let tls_dir = dir.path().join("tls");
    tls::load_or_create_cert(&tls_dir).unwrap();
    let connector = tls::make_connector(&tls_dir.join("cert.pem")).unwrap();

    // Accepts TCP connections and never answers the TLS ClientHello.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((s, _)) = listener.accept().await {
            held.push(s);
        }
    });

    let errors: Arc<Mutex<Vec<String>>> = Arc::default();
    let errors_cb = errors.clone();
    let started = Instant::now();
    let client = tokio::time::timeout(
        CONNECT_TIMEOUT + Duration::from_secs(5),
        RpcClient::connect_with_reconnect_full(
            &format!("tcp://{}", addr),
            Some(connector),
            |_c: &RpcClient| Box::pin(async { Ok(()) }),
            None::<fn()>,
            None::<fn()>,
            Some(move |msg: &str| errors_cb.lock().unwrap().push(msg.to_string())),
        ),
    )
    .await
    .expect("initial connect hung on the silent handshake")
    .unwrap();
    let waited = started.elapsed();
    assert!(waited >= CONNECT_TIMEOUT - Duration::from_secs(1), "gave up too early: {:?}", waited);
    assert!(!client.is_connected().await);

    // The reconnect loop keeps trying and reports each failed attempt.
    let deadline = Instant::now() + CONNECT_TIMEOUT + Duration::from_secs(5);
    while errors.lock().unwrap().is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let errors = errors.lock().unwrap();
    assert!(
        errors.first().is_some_and(|e| e.contains("reconnect attempt 1 failed (connect)")
            && e.contains("timed out")),
        "reconnect failure not reported: {:?}",
        errors
    );
}
