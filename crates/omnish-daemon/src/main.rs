mod config_schema;
mod config_watcher;
mod sandbox_rules;
mod server;

use omnish_daemon::file_watcher;

use anyhow::Result;
use omnish_common::config::{load_daemon_config, omnish_dir};
use omnish_daemon::conversation_mgr::ConversationManager;
use omnish_daemon::session_mgr::SessionManager;
use omnish_llm::backend::{LlmBackend, UnavailableBackend};
use omnish_llm::factory::{MultiBackend, SharedLlmBackend};
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

    tracing::info!("omnish-daemon {}", omnish_common::VERSION);

    // Load configuration
    let mut config = load_daemon_config()?;
    omnish_daemon::task_mgr::inject_task_defaults(&mut config.tasks);

    // Environment variable takes precedence over config file
    let socket_path = std::env::var("OMNISH_SOCKET").unwrap_or_else(|_| config.listen_addr.clone());

    // SessionManager manages both sessions and completions internally:
    //   - sessions stored in $omnish_dir/sessions
    //   - completion logs stored in $omnish_dir/logs/completions
    let omnish_dir = omnish_dir();

    // Create LLM backend (falls back to UnavailableBackend if config fails)
    let llm_backend: SharedLlmBackend = {
        let backend = match MultiBackend::new(&config.llm, config.proxy.http_proxy.as_deref(), config.proxy.no_proxy.as_deref()) {
            Ok(backend) => {
                tracing::info!("LLM backend initialized: {}", backend.name());
                Arc::new(backend)
            }
            Err(e) => {
                tracing::warn!("LLM backend not available: {}", e);
                Arc::new(MultiBackend::from_single(Arc::new(UnavailableBackend)))
            }
        };
        Arc::new(std::sync::RwLock::new(backend))
    };

    let session_mgr = Arc::new(SessionManager::new(omnish_dir.clone(), config.context.clone()));
    match session_mgr.load_existing().await {
        Ok(count) if count > 0 => tracing::info!("loaded {} existing session(s)", count),
        Ok(_) => {}
        Err(e) => tracing::warn!("failed to load existing sessions: {}", e),
    }

    let conv_mgr = Arc::new(ConversationManager::new(omnish_dir.join("threads")));

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
    omnish_daemon::plugin::auto_install_bundled_plugins(&plugins_dir, &config.plugins);

    // Initialize plugin manager - loads tool definitions from JSON files
    let plugin_mgr = Arc::new(omnish_daemon::plugin::PluginManager::load(&plugins_dir, &config.plugins));

    // Build unified tool registry from plugins + built-in tools
    let tool_registry = Arc::new(omnish_daemon::tool_registry::ToolRegistry::new());
    plugin_mgr.register_all(&tool_registry);
    omnish_daemon::tools::command_query::CommandQueryTool::register(&tool_registry);

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

    // Watch plugin subdirectories for hot-reload via shared file watcher.
    // The initial rescan subscribes every existing plugin subdir to the
    // watcher so edits inside them (tool.json, tool.override.json) fire,
    // which inotify-on-plugins_dir alone would miss (non-recursive).
    let plugin_mgr_watcher = Arc::clone(&plugin_mgr);
    let plugin_rx = file_watcher.watch(plugins_dir.clone());
    let tool_registry_watcher = Arc::clone(&tool_registry);
    let file_watcher_for_plugin = Arc::clone(&file_watcher);
    plugin_mgr.rescan(&tool_registry, Some(&file_watcher));
    tokio::spawn(async move {
        plugin_mgr_watcher
            .watch_with(plugin_rx, tool_registry_watcher, file_watcher_for_plugin)
            .await
    });

    // Issue #588: package plugins/ into an in-memory tarball so clients can
    // mirror their local `~/.omnish/plugins/` via the update streaming
    // path. Initial rebuild here makes the bundle available to the first
    // client that polls before the scheduled PluginBundleTask fires;
    // subsequent refreshes run on the task manager's schedule (default
    // every 5 minutes).
    let plugin_bundler = Arc::new(omnish_daemon::plugin_bundle::PluginBundler::new(plugins_dir.clone()));
    plugin_bundler.rebuild().await;

    // Shared daemon-level context for scheduled tasks. Built here (not
    // earlier) so it can carry plugin_bundler - the PluginBundleTask
    // reaches the bundler through TaskContext.daemon.
    let daemon_ctx = Arc::new(omnish_daemon::task_mgr::DaemonContext {
        omnish_dir: omnish_dir.clone(),
        restart_signal: Arc::clone(&restart_signal),
        update_cache: Arc::clone(&update_cache),
        plugin_bundler: Arc::clone(&plugin_bundler),
    });

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

    // Hot-reload LLM backends on config change
    {
        let llm_rx = config_watcher.subscribe(config_watcher::ConfigSection::Llm);
        let llm_holder = llm_backend.clone();
        tokio::spawn(async move {
            let mut rx = llm_rx;
            while rx.changed().await.is_ok() {
                let config = rx.borrow_and_update().clone();
                match MultiBackend::new(&config.llm, config.proxy.http_proxy.as_deref(), config.proxy.no_proxy.as_deref()) {
                    Ok(new_backend) => {
                        let name = new_backend.name().to_string();
                        *llm_holder.write().unwrap() = Arc::new(new_backend);
                        tracing::info!("LLM backend reloaded: {}", name);
                    }
                    Err(e) => {
                        tracing::warn!("LLM backend reload failed (keeping current): {}", e);
                    }
                }
            }
        });
    }

    let daemon_config_arc = std::sync::Arc::new(std::sync::RwLock::new(config.clone()));

    // Keep daemon_config_arc in sync with daemon.toml file changes.
    // ConfigQuery reads from this arc; without this, manual edits to daemon.toml
    // (e.g. use_proxy = true) wouldn't be reflected in /config until daemon restart.
    {
        let llm_rx = config_watcher.subscribe(config_watcher::ConfigSection::Llm);
        let sandbox_rx = config_watcher.subscribe(config_watcher::ConfigSection::Sandbox);
        let plugins_rx = config_watcher.subscribe(config_watcher::ConfigSection::Plugins);
        let tasks_rx = config_watcher.subscribe(config_watcher::ConfigSection::Tasks);
        let client_rx = config_watcher.subscribe(config_watcher::ConfigSection::Client);
        let dca = Arc::clone(&daemon_config_arc);
        tokio::spawn(async move {
            let mut llm = llm_rx;
            let mut sandbox = sandbox_rx;
            let mut plugins = plugins_rx;
            let mut tasks = tasks_rx;
            let mut client = client_rx;
            loop {
                tokio::select! {
                    Ok(()) = llm.changed() => {
                        let config = llm.borrow_and_update().clone();
                        *dca.write().unwrap() = (*config).clone();
                    }
                    Ok(()) = sandbox.changed() => {
                        let config = sandbox.borrow_and_update().clone();
                        *dca.write().unwrap() = (*config).clone();
                    }
                    Ok(()) = plugins.changed() => {
                        let config = plugins.borrow_and_update().clone();
                        *dca.write().unwrap() = (*config).clone();
                    }
                    Ok(()) = tasks.changed() => {
                        let config = tasks.borrow_and_update().clone();
                        *dca.write().unwrap() = (*config).clone();
                    }
                    Ok(()) = client.changed() => {
                        let config = client.borrow_and_update().clone();
                        *dca.write().unwrap() = (*config).clone();
                    }
                    else => break,
                }
            }
        });
    }

    // Hot-reload plugins on config change (enable/disable). Both this path
    // and the filesystem event path go through `rescan` so there is one
    // reconciliation routine: whichever event wakes up first, it reads the
    // already-mirrored `[plugins]` section and produces the same final state.
    {
        let plugins_rx = config_watcher.subscribe(config_watcher::ConfigSection::Plugins);
        let pm = Arc::clone(&plugin_mgr);
        let tr = Arc::clone(&tool_registry);
        tokio::spawn(async move {
            let mut rx = plugins_rx;
            while rx.changed().await.is_ok() {
                let config = rx.borrow_and_update().clone();
                pm.update_plugins_config(&config.plugins);
                pm.rescan(&tr, None);
            }
        });
    }

    let server_opts = Arc::new(server::ServerOpts {
        sandbox_rules: Arc::clone(&server_sandbox_rules),
        config_path: config_path.clone(),
        daemon_config: Arc::clone(&daemon_config_arc),
    });

    // Set up scheduled tasks using unified TaskContext + create_all_tasks
    let task_ctx = omnish_daemon::task_mgr::TaskContext {
        session_mgr: Arc::clone(&session_mgr),
        conv_mgr: Arc::clone(&conv_mgr),
        llm_backend: llm_backend.clone(),
        daemon: Arc::clone(&daemon_ctx),
        daemon_config: Arc::clone(&daemon_config_arc),
    };

    let mut task_mgr = omnish_daemon::task_mgr::TaskManager::new().await?;
    let all_tasks = omnish_daemon::task_mgr::create_all_tasks(&config.tasks);
    for task in &all_tasks {
        if task.enabled() {
            let job = task.create_job(&task_ctx)?;
            task_mgr.register(task.name(), task.schedule(), job).await?;
        }
    }
    task_mgr.start().await?;
    let task_mgr = Arc::new(tokio::sync::Mutex::new(task_mgr));

    // Hot-reload tasks on config change
    {
        let tasks_rx = config_watcher.subscribe(config_watcher::ConfigSection::Tasks);
        let tm = Arc::clone(&task_mgr);
        tokio::spawn(async move {
            let mut rx = tasks_rx;
            while rx.changed().await.is_ok() {
                let config = rx.borrow_and_update().clone();
                let all_tasks = omnish_daemon::task_mgr::create_all_tasks(&config.tasks);
                let mut mgr = tm.lock().await;
                if let Err(e) = mgr.reload(&all_tasks, &task_ctx).await {
                    tracing::warn!("task reload failed: {}", e);
                }
            }
        });
    }

    // Create formatter manager and register external formatters from plugins
    let mut formatter_mgr = omnish_daemon::formatter_mgr::FormatterManager::new();
    for (name, binary) in plugin_mgr.formatter_binaries() {
        let Some(path) = binary.to_str() else {
            tracing::warn!("formatter '{}' has non-UTF-8 binary path, skipping", name);
            continue;
        };
        let _ = formatter_mgr.register_external(&name, path).await;
    }
    let formatter_mgr = Arc::new(formatter_mgr);
    let server = DaemonServer::new(session_mgr, llm_backend, task_mgr, conv_mgr, plugin_mgr.clone(), tool_registry.clone(), server_opts, formatter_mgr, Arc::clone(&update_cache), Arc::clone(&plugin_bundler));

    // Push client-relevant config changes to all connected clients via push_registry.
    // Only watches [client] section; sandbox settings are now client-local.
    {
        let client_rx = config_watcher.subscribe(config_watcher::ConfigSection::Client);
        let push_reg = server.push_registry.clone();
        tokio::spawn(async move {
            let mut client_rx = client_rx;
            let mut prev = client_rx.borrow_and_update().clone();
            while client_rx.changed().await.is_ok() {
                let config = client_rx.borrow_and_update().clone();
                let changes = crate::server::diff_client_config(&prev, &config);
                if !changes.is_empty() {
                    let msg = omnish_protocol::message::Message::ConfigClient { changes };
                    let registry = push_reg.lock().await;
                    for (_, push_tx) in registry.iter() {
                        let _ = push_tx.send(msg.clone()).await;
                    }
                    tracing::info!("pushed client config to {} connections", registry.len());
                }
                prev = config;
            }
        });
    }

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
