#[allow(dead_code)]
mod event_detector;
mod server;

use anyhow::Result;
use omnish_common::config::load_daemon_config;
use omnish_daemon::session_mgr::SessionManager;
use omnish_llm::factory::create_default_backend;
use server::DaemonServer;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // Load configuration
    let config = load_daemon_config()?;

    // Environment variable takes precedence over config file
    let socket_path = std::env::var("OMNISH_SOCKET")
        .unwrap_or_else(|_| config.listen_addr.clone());

    let store_dir = std::path::PathBuf::from(&config.sessions_dir);

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

    let evict_hours = config.context.session_evict_hours;
    let session_mgr = Arc::new(SessionManager::new(store_dir, config.context));
    match session_mgr.load_existing().await {
        Ok(count) if count > 0 => tracing::info!("loaded {} existing session(s)", count),
        Ok(_) => {}
        Err(e) => tracing::warn!("failed to load existing sessions: {}", e),
    }

    // Spawn periodic eviction of inactive sessions
    {
        let mgr = Arc::clone(&session_mgr);
        let max_inactive = std::time::Duration::from_secs(evict_hours * 3600);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                mgr.evict_inactive(max_inactive).await;
            }
        });
    }

    let server = DaemonServer::new(session_mgr, llm_backend);

    tracing::info!("starting omnishd at {}", socket_path);
    server.run(&socket_path).await
}
