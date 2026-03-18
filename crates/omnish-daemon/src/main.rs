#[allow(dead_code)]
mod event_detector;
mod server;

use anyhow::Result;
use omnish_common::config::{load_daemon_config, omnish_dir};
use omnish_daemon::conversation_mgr::ConversationManager;
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

    if std::env::args().any(|a| a == "--init") {
        let omnish_dir = omnish_dir();
        let (token, token_status, cert_status) = init_omnish_dir(&omnish_dir)?;
        println!("auth_token: {} ({})", omnish_common::auth::default_token_path().display(), token_status);
        let tls_dir = omnish_transport::tls::default_tls_dir();
        println!("tls cert:   {}/cert.pem ({})", tls_dir.display(), cert_status);
        println!("tls key:    {}/key.pem ({})", tls_dir.display(), cert_status);
        let _ = token;
        return Ok(());
    }

    let worker_threads = std::thread::available_parallelism()
        .map(|n| n.get().min(30))
        .unwrap_or(4);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()?
        .block_on(async_main())
}

/// Initialize ~/.omnish/ directory: create credentials.
/// Returns (auth_token, token_status, cert_status) where status is "existed, skip" or "created".
fn init_omnish_dir(omnish_dir: &std::path::Path) -> Result<(String, &'static str, &'static str)> {
    std::fs::create_dir_all(omnish_dir)?;

    // Auth token
    let token_path = omnish_common::auth::default_token_path();
    let token_existed = token_path.exists();
    let token = omnish_common::auth::load_or_create_token(&token_path)?;
    let token_status = if token_existed { "existed, skip" } else { "created" };

    // TLS cert
    let tls_dir = omnish_transport::tls::default_tls_dir();
    let cert_existed = tls_dir.join("cert.pem").exists();
    let _ = omnish_transport::tls::load_or_create_cert(&tls_dir)?;
    let cert_status = if cert_existed { "existed, skip" } else { "created" };

    Ok((token, token_status, cert_status))
}

async fn async_main() -> Result<()> {
    // Initialize tracing: stderr (RUST_LOG, default info) + file (always debug)
    let log_dir = omnish_dir().join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let file_appender = tracing_appender::rolling::daily(&log_dir, "daemon.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let stderr_filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive("debug".parse().unwrap())
        .add_directive("rustls=off".parse().unwrap());
    let file_filter = tracing_subscriber::EnvFilter::new("debug")
        .add_directive("rustls=off".parse().unwrap());

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(stderr_filter),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(non_blocking)
                .with_filter(file_filter),
        )
        .init();

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
    let disk_cleanup_config = config.tasks.disk_cleanup.clone();
    let auto_update_config = config.tasks.auto_update.clone();
    let session_mgr = Arc::new(SessionManager::new(omnish_dir.clone(), config.context));
    match session_mgr.load_existing().await {
        Ok(count) if count > 0 => tracing::info!("loaded {} existing session(s)", count),
        Ok(_) => {}
        Err(e) => tracing::warn!("failed to load existing sessions: {}", e),
    }

    let conv_mgr = Arc::new(ConversationManager::new(omnish_dir.join("threads")));

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
        let notes_dir = omnish_dir.join("notes");
        let job = create_hourly_summary_job(
            Arc::clone(&session_mgr),
            Arc::clone(&conv_mgr),
            llm_backend.clone(),
            notes_dir,
        )?;
        task_mgr.register("hourly_summary", "0 0 * * * *", job).await?;
        tracing::info!("hourly summary enabled");
    }

    // Register disk cleanup job
    {
        let max_age = std::time::Duration::from_secs(48 * 3600);
        let job = omnish_daemon::cleanup::create_disk_cleanup_job(
            Arc::clone(&session_mgr),
            max_age,
            &disk_cleanup_config.schedule,
        )?;
        task_mgr.register("disk_cleanup", &disk_cleanup_config.schedule, job).await?;
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

    // Register auto-update job if enabled
    if auto_update_config.enabled {
        let job = omnish_daemon::auto_update::create_auto_update_job(
            omnish_dir.clone(),
            &auto_update_config.schedule,
            auto_update_config.clients.clone(),
            auto_update_config.check_url.clone(),
        )?;
        task_mgr
            .register("auto_update", &auto_update_config.schedule, job)
            .await?;
        tracing::info!(
            "auto update enabled (schedule={})",
            auto_update_config.schedule
        );
    }

    // Register thread summary job (runs every 10 minutes)
    {
        let job = omnish_daemon::thread_summary::create_thread_summary_job(
            Arc::clone(&conv_mgr),
            llm_backend.clone(),
        )?;
        task_mgr.register("thread_summary", "0 */10 * * * *", job).await?;
    }

    task_mgr.start().await?;
    let task_mgr = Arc::new(tokio::sync::Mutex::new(task_mgr));

    // Initialize credentials and embedded assets
    let (auth_token, token_status, _cert_status) = init_omnish_dir(&omnish_dir)?;
    tracing::info!("auth token {} ({})", omnish_common::auth::default_token_path().display(), token_status);

    // Create TLS acceptor (only for TCP mode)
    let tls_acceptor = if socket_path.contains(':') {
        let tls_dir = omnish_transport::tls::default_tls_dir();
        let acceptor = omnish_transport::tls::make_acceptor(&tls_dir)?;
        tracing::info!("TLS enabled for TCP (cert dir: {})", tls_dir.display());
        Some(acceptor)
    } else {
        None
    };

    // Initialize plugin manager — loads tool definitions from JSON files
    let plugins_dir = omnish_dir.join("plugins");
    let plugin_mgr = Arc::new(omnish_daemon::plugin::PluginManager::load(&plugins_dir));

    // Watch tool.override.json files for hot-reload
    let plugin_mgr_watcher = Arc::clone(&plugin_mgr);
    tokio::spawn(async move { plugin_mgr_watcher.watch_overrides().await });

    let server = DaemonServer::new(session_mgr, llm_backend, task_mgr, conv_mgr, plugin_mgr);

    tracing::info!("starting omnishd at {}", socket_path);
    server.run(&socket_path, auth_token, tls_acceptor).await
}
