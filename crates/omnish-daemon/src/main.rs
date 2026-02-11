#[allow(dead_code)]
mod event_detector;
mod server;

use anyhow::Result;
use omnish_daemon::session_mgr::SessionManager;
use omnish_transport::unix::UnixTransport;
use server::DaemonServer;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let socket_path = std::env::var("OMNISH_SOCKET").unwrap_or_else(|_| {
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            format!("{}/omnish.sock", dir)
        } else {
            "/tmp/omnish.sock".to_string()
        }
    });

    let store_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("omnish/sessions");

    let session_mgr = Arc::new(SessionManager::new(store_dir));
    let server = DaemonServer::new(session_mgr);
    let transport = UnixTransport;

    tracing::info!("starting omnishd at {}", socket_path);
    server.run(&transport, &socket_path).await
}
