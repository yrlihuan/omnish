#[allow(dead_code)]
mod event_detector;
mod server;

use anyhow::Result;
use omnish_common::config::load_config;
use omnish_daemon::session_mgr::SessionManager;
use omnish_llm::factory::create_default_backend;
use omnish_transport::unix::UnixTransport;
use server::DaemonServer;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // Load configuration
    let config = load_config()?;

    // Environment variable takes precedence over config file
    let socket_path = std::env::var("OMNISH_SOCKET")
        .unwrap_or_else(|_| config.daemon.socket_path.clone());

    let store_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("omnish/sessions");

    // Create LLM backend if configured
    let llm_backend = match create_default_backend(&config.llm) {
        Ok(backend) => {
            tracing::info!("LLM backend initialized: {}", backend.name());
            Some(backend)
        }
        Err(e) => {
            tracing::warn!("LLM backend not available: {}", e);
            None
        }
    };

    let session_mgr = Arc::new(SessionManager::new(store_dir));
    let server = DaemonServer::new(session_mgr, llm_backend);
    let transport = UnixTransport;

    tracing::info!("starting omnishd at {}", socket_path);
    server.run(&transport, &socket_path).await
}
