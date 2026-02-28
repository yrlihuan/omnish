#[allow(dead_code)]
mod event_detector;
mod server;

use anyhow::Result;
use omnish_common::config::{load_daemon_config, omnish_dir};
use omnish_daemon::daily_notes::create_daily_notes_job;
use omnish_daemon::hourly_summary::create_hourly_summary_job;
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

    // SessionManager manages both sessions and completions internally:
    //   - sessions stored in $omnish_dir/sessions
    //   - completion logs stored in $omnish_dir/logs/completions
    let omnish_dir = omnish_dir();

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

    let evict_hours = config.tasks.eviction.session_evict_hours;
    let daily_notes_config = config.tasks.daily_notes.clone();
    let session_mgr = Arc::new(SessionManager::new(omnish_dir.clone(), config.context));
    match session_mgr.load_existing().await {
        Ok(count) if count > 0 => tracing::info!("loaded {} existing session(s)", count),
        Ok(_) => {}
        Err(e) => tracing::warn!("failed to load existing sessions: {}", e),
    }

    // Set up scheduled task manager
    let mut task_mgr = omnish_daemon::task_mgr::TaskManager::new().await?;

    // Register session eviction job (hourly)
    {
        let max_inactive = std::time::Duration::from_secs(evict_hours * 3600);
        let job = omnish_daemon::eviction::create_eviction_job(
            Arc::clone(&session_mgr),
            max_inactive,
        )?;
        task_mgr.register("eviction", "0 0 * * * *", job).await?;
    }

    // Register hourly summary job (runs every hour at minute 0)
    {
        let summaries_dir = omnish_dir.join("logs").join("hourly_summaries");
        let job = create_hourly_summary_job(
            Arc::clone(&session_mgr),
            llm_backend.clone(),
            summaries_dir,
        )?;
        task_mgr.register("hourly_summary", "0 0 * * * *", job).await?;
        tracing::info!("hourly summary enabled");
    }

    // Register daily notes job if enabled
    if daily_notes_config.enabled {
        let notes_dir = omnish_dir.join("notes");
        let cron = format!("0 0 {} * * *", daily_notes_config.schedule_hour);
        let job = create_daily_notes_job(
            Arc::clone(&session_mgr),
            llm_backend.clone(),
            notes_dir,
            daily_notes_config.schedule_hour,
        )?;
        task_mgr.register("daily_notes", &cron, job).await?;
        tracing::info!(
            "daily notes enabled (schedule_hour={})",
            daily_notes_config.schedule_hour
        );
    }

    task_mgr.start().await?;
    let task_mgr = Arc::new(tokio::sync::Mutex::new(task_mgr));

    let server = DaemonServer::new(session_mgr, llm_backend, task_mgr);

    tracing::info!("starting omnishd at {}", socket_path);
    server.run(&socket_path).await
}
