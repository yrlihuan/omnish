mod config_schema;
mod config_watcher;
mod file_watcher;
mod sandbox_rules;
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

/// Exit code indicating the daemon should be restarted (e.g. after upgrade).
/// Systemd's `Restart=on-failure` treats non-zero exits as failures and restarts.
const EXIT_RESTART: i32 = 42;

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("omnish-daemon {}", omnish_common::VERSION);
        return;
    }

    if std::env::args().any(|a| a == "--init") {
        let omnish_dir = omnish_dir();
        match init_omnish_dir(&omnish_dir) {
            Ok((token, token_status, cert_status)) => {
                println!("auth_token: {} ({})", omnish_common::auth::default_token_path().display(), token_status);
                let tls_dir = omnish_transport::tls::default_tls_dir();
                println!("tls cert:   {}/cert.pem ({})", tls_dir.display(), cert_status);
                println!("tls key:    {}/key.pem ({})", tls_dir.display(), cert_status);
                let _ = token;
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }

    let worker_threads = std::thread::available_parallelism()
        .map(|n| n.get().min(30))
        .unwrap_or(4);
    let exit_code = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|rt| rt.block_on(async_main()))
    {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Fatal: {}", e);
            1
        }
    };
    std::process::exit(exit_code);
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

async fn async_main() -> Result<i32> {
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
        match MultiBackend::new(&config.llm, config.proxy.as_deref(), config.no_proxy.as_deref()) {
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
    let periodic_summary_config = config.tasks.periodic_summary.clone();
    let disk_cleanup_config = config.tasks.disk_cleanup.clone();
    let auto_update_config = config.tasks.auto_update.clone();
    let session_mgr = Arc::new(SessionManager::new(omnish_dir.clone(), config.context.clone()));
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

    // Register periodic summary job
    {
        let notes_dir = omnish_dir.join("notes");
        let interval = periodic_summary_config.interval_hours;
        let (cron, job) = create_hourly_summary_job(
            Arc::clone(&session_mgr),
            Arc::clone(&conv_mgr),
            llm_backend.clone(),
            notes_dir,
            interval,
        );
        task_mgr.register("periodic_summary", &cron, job?).await?;
        tracing::info!("periodic summary enabled (interval={}h)", interval);
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
            Arc::clone(&conv_mgr),
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

    // Restart signal: notified when auto-update installs a new binary
    let restart_signal = Arc::new(tokio::sync::Notify::new());

    // Update cache: stores downloaded packages for distribution to clients
    let update_cache = Arc::new(omnish_daemon::update_cache::UpdateCache::new(&omnish_dir));

    // Periodic scan of updates directory (every 60s) to refresh cached versions
    {
        let uc = Arc::clone(&update_cache);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                uc.scan_updates();
            }
        });
    }

    // Register auto-update job if enabled
    if auto_update_config.enabled {
        let job = omnish_daemon::auto_update::create_auto_update_job(
            omnish_dir.clone(),
            &auto_update_config.schedule,
            auto_update_config.clients.clone(),
            auto_update_config.check_url.clone(),
            Arc::clone(&restart_signal),
            Arc::clone(&update_cache),
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

    // Auto-install bundled plugins when their tool config is present
    let plugins_dir = omnish_dir.join("plugins");
    omnish_daemon::plugin::auto_install_bundled_plugins(&plugins_dir, &config.tools);

    // Initialize plugin manager — loads tool definitions from JSON files
    let plugin_mgr = Arc::new(omnish_daemon::plugin::PluginManager::load(&plugins_dir));

    // Build unified tool registry from plugins + built-in tools
    let mut tool_registry = omnish_daemon::tool_registry::ToolRegistry::new();
    plugin_mgr.register_all(&mut tool_registry);
    omnish_daemon::tools::command_query::CommandQueryTool::register(&mut tool_registry);
    let tool_registry = Arc::new(tool_registry);

    // Shared file watcher for config and plugin hot-reload
    let file_watcher = Arc::new(file_watcher::FileWatcher::new());
    let fw = Arc::clone(&file_watcher);
    tokio::spawn(async move { fw.run().await });

    // Config watcher: monitors daemon.toml, notifies subscribers on section changes
    let config_path = std::env::var("OMNISH_DAEMON_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| omnish_dir.join("daemon.toml"));
    let config_watcher = config_watcher::ConfigWatcher::new(
        config_path.clone(),
        config.clone(),
        &file_watcher,
    );

    // Watch tool.override.json files for hot-reload via shared file watcher
    let plugin_mgr_watcher = Arc::clone(&plugin_mgr);
    let plugin_rx = file_watcher.watch(plugins_dir.clone());
    let tool_registry_watcher = Arc::clone(&tool_registry);
    tokio::spawn(async move { plugin_mgr_watcher.watch_with(plugin_rx, tool_registry_watcher).await });

    // Extract chat model name for ghost hint
    let chat_model_name = config.llm.use_cases.get("chat")
        .and_then(|backend_name| config.llm.backends.get(backend_name))
        .or_else(|| config.llm.backends.get(&config.llm.default))
        .map(|bc| bc.model.clone());

    let sandbox_rules = sandbox_rules::compile_config(&config.sandbox);
    let server_sandbox_rules: Arc<std::sync::RwLock<_>> = Arc::new(std::sync::RwLock::new(sandbox_rules));

    // Hot-reload sandbox rules on config change
    {
        let sandbox_rx = config_watcher.subscribe(config_watcher::ConfigSection::Sandbox);
        let sr = Arc::clone(&server_sandbox_rules);
        tokio::spawn(async move {
            let mut rx = sandbox_rx;
            while rx.changed().await.is_ok() {
                let config = rx.borrow_and_update().clone();
                let new_rules = crate::sandbox_rules::compile_config(&config.sandbox);
                let rule_count: usize = new_rules.values().map(|v| v.len()).sum();
                let tool_count = new_rules.len();
                *sr.write().unwrap() = new_rules;
                tracing::info!("sandbox rules reloaded: {} rules for {} tools", rule_count, tool_count);
            }
        });
    }

    let daemon_config_arc = std::sync::Arc::new(std::sync::RwLock::new(config.clone()));
    let server_opts = Arc::new(server::ServerOpts {
        proxy: config.proxy,
        no_proxy: config.no_proxy,
        sandbox_rules: Arc::clone(&server_sandbox_rules),
        config_path: config_path.clone(),
        daemon_config: daemon_config_arc,
    });
    // Create formatter manager and register external formatters from plugins
    let mut formatter_mgr = omnish_daemon::formatter_mgr::FormatterManager::new();
    for (name, binary) in plugin_mgr.formatter_binaries() {
        let Some(path) = binary.to_str() else {
            tracing::warn!("formatter '{}' has non-UTF-8 binary path, skipping", name);
            continue;
        };
        formatter_mgr.register_external(&name, path).await;
    }
    let formatter_mgr = Arc::new(formatter_mgr);
    let server = DaemonServer::new(session_mgr, llm_backend, task_mgr, conv_mgr, plugin_mgr, tool_registry, chat_model_name, config.tools, server_opts, formatter_mgr, Arc::clone(&update_cache));

    tracing::info!("starting omnishd at {}", socket_path);

    // Set up signal handlers
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigusr1 = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())?;

    // Race between server, signals, and restart request
    let exit_code = tokio::select! {
        result = server.run(&socket_path, auth_token, tls_acceptor) => {
            if let Err(e) = result {
                tracing::error!("server error: {}", e);
                1
            } else {
                0
            }
        }
        _ = restart_signal.notified() => {
            tracing::info!("restart requested after upgrade");
            EXIT_RESTART
        }
        _ = sigterm.recv() => {
            tracing::info!("received SIGTERM, shutting down");
            0
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("received SIGINT, shutting down");
            0
        }
        _ = sigusr1.recv() => {
            tracing::info!("received SIGUSR1, restarting");
            EXIT_RESTART
        }
    };

    tracing::info!("omnishd exiting with code {}", exit_code);
    Ok(exit_code)
}
