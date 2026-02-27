#[allow(dead_code)]
mod event_detector;
mod server;

use anyhow::Result;
use omnish_common::config::{load_daemon_config, omnish_dir};
use omnish_daemon::daily_notes::create_daily_notes_job;
use tokio_cron_scheduler::JobScheduler;
use omnish_daemon::session_mgr::SessionManager;
use omnish_llm::factory::MultiBackend;
use server::DaemonServer;
use std::sync::Arc;

fn main() -> Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("omnish-daemon {}", omnish_common::VERSION);
        return Ok(());
    }

    let worker_threads = std::thread::available_parallelism()
        .map(|n| n.get().min(16))
        .unwrap_or(4);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()?
        .block_on(async_main())
}

async fn async_main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // Load configuration
    let config = load_daemon_config()?;

    // Environment variable takes precedence over config file
    let socket_path = std::env::var("OMNISH_SOCKET").unwrap_or_else(|_| config.listen_addr.clone());

    let store_dir = std::path::PathBuf::from(&config.sessions_dir);

    // Create LLM backend if configured
    let llm_backend: Option<Arc<dyn omnish_llm::backend::LlmBackend>> =
        match MultiBackend::new(&config.llm) {
            Ok(backend) => {
                let backend: Arc<dyn omnish_llm::backend::LlmBackend> = Arc::new(backend);
                tracing::info!("LLM backend initialized: {}", backend.name());
                Some(backend)
            }
            Err(e) => {
                tracing::warn!("LLM backend not available: {}", e);
                None
            }
        };

    let evict_hours = config.context.session_evict_hours;
    let daily_notes_config = config.daily_notes.clone();
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

    // Spawn daily notes task if enabled
    if daily_notes_config.enabled {
        let notes_dir = omnish_dir().join("notes");
        let job = create_daily_notes_job(
            Arc::clone(&session_mgr),
            llm_backend.clone(),
            notes_dir,
            daily_notes_config.schedule_hour,
        )?;
        let sched = JobScheduler::new().await?;
        sched.add(job).await?;
        sched.start().await?;
        tracing::info!(
            "daily notes enabled (schedule_hour={})",
            daily_notes_config.schedule_hour
        );
    }

    let server = DaemonServer::new(session_mgr, llm_backend);

    tracing::info!("starting omnishd at {}", socket_path);
    server.run(&socket_path).await
}
