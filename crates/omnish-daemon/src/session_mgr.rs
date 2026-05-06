use anyhow::{anyhow, Result};
use omnish_common::config::ContextConfig;
use omnish_context::recent::{CompletionFormatter, CompletionSections, GroupedFormatter, RecentCommands};
use omnish_context::StreamReader;
use omnish_store::command::CommandRecord;
use omnish_store::completion::CompletionRecord;
use omnish_store::sample::{CompletionSample, PendingSample};
use omnish_store::session::SessionMeta;
use omnish_store::session_update::SessionUpdateRecord;
use omnish_store::stream::{read_range, StreamEntry, StreamWriter};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock};

/// Minimum edit distance similarity to consider a completion a "near miss".
const SAMPLE_SIMILARITY_THRESHOLD: f64 = 0.3;
/// Global rate limit: at most one sample per this many seconds.
const SAMPLE_RATE_LIMIT_SECS: u64 = 300; // 5 minutes
/// Max elapsed time (seconds) between completion request and next command for sampling.
const SAMPLE_MAX_ELAPSED_SECS: u64 = 15;

struct FileStreamReader {
    stream_path: PathBuf,
}

impl StreamReader for FileStreamReader {
    fn read_command_output(&self, offset: u64, length: u64) -> Result<Vec<StreamEntry>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        read_range(&self.stream_path, offset, length)
    }
}

struct MultiSessionReader {
    readers: HashMap<(u64, u64), PathBuf>,
}

impl StreamReader for MultiSessionReader {
    fn read_command_output(&self, offset: u64, length: u64) -> Result<Vec<StreamEntry>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        let path = self
            .readers
            .get(&(offset, length))
            .ok_or_else(|| anyhow!("no stream file for offset={}, length={}", offset, length))?;
        read_range(path, offset, length)
    }
}

struct StreamWriterState {
    writer: StreamWriter,
    last_command_stream_pos: u64,
    last_active: Instant,
}

struct Session {
    dir: PathBuf, // immutable after creation
    meta: RwLock<SessionMeta>,
    commands: RwLock<Vec<CommandRecord>>,
    stream_writer: Mutex<StreamWriterState>,
    last_update: Mutex<Option<u64>>, // timestamp_ms of last SessionUpdate
    pending_sample: Mutex<Option<PendingSample>>,
}

pub struct SessionManager {
    base_dir: PathBuf,
    /// Persistent index of every client that has ever connected. Survives
    /// per-session 48h cleanup so the deploy menu retains stale hosts.
    clients_history: RwLock<crate::clients_history::ClientsHistory>,
    clients_history_path: PathBuf,
    sessions: RwLock<HashMap<String, Arc<Session>>>,
    context_config: ContextConfig,
    completion_writer: mpsc::Sender<CompletionRecord>,
    session_writer: mpsc::Sender<SessionUpdateRecord>,
    /// Frozen history cutoff: commands with `started_at <= this` are history.
    /// Between elastic resets this value is stable, so the History prefix never changes.
    /// `None` means the cutoff has not been established yet (first call).
    history_frozen_until: RwLock<Option<u64>>,
    /// Frozen warmup cutoff within the `<recent>` block: commands with
    /// `started_at <= this` are in the cacheable stable_prefix, while newer
    /// commands go into the remainder. Advanced only at warmup time, so the
    /// stable_prefix stays byte-identical between warmups, maximizing KV cache
    /// hits across consecutive completion requests.
    /// `None` means no warmup has occurred yet (first call).
    recent_frozen_until: RwLock<Option<u64>>,
    /// Cached completion context from last build, used to detect prefix changes
    /// for KV cache warmup.
    last_completion_context: RwLock<String>,
    sample_writer: mpsc::Sender<CompletionSample>,
    last_sample_time: Mutex<Option<Instant>>,
}

/// Infer `last_active` from persisted data so that idle time survives daemon restarts.
/// Falls back to `Instant::now()` when no timestamp is available.
fn infer_last_active(commands: &[CommandRecord], meta: &SessionMeta) -> Instant {
    // Best source: last command's ended_at or started_at (epoch ms).
    let last_cmd_ms = commands
        .last()
        .and_then(|cmd| cmd.ended_at.or(Some(cmd.started_at)));

    // Fallback: session's ended_at or started_at (RFC 3339 string → epoch ms).
    let parse_rfc3339_ms = |s: &str| -> Option<u64> {
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.timestamp_millis() as u64)
    };

    let ts_ms = last_cmd_ms
        .or_else(|| meta.ended_at.as_deref().and_then(parse_rfc3339_ms))
        .or_else(|| parse_rfc3339_ms(&meta.started_at));

    match ts_ms {
        Some(ms) => {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let age = Duration::from_millis(now_ms.saturating_sub(ms));
            Instant::now() - age
        }
        None => Instant::now(),
    }
}

fn format_idle(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

/// Extract `(deploy_addr, hostname)` from session attrs for clients-history
/// bookkeeping. Returns None when hostname is missing or empty (no useful
/// menu entry can be derived without it). `client_addr` falls back to
/// `hostname` so legacy clients without the ClientAddrProbe still register.
fn history_pair_from_attrs(
    attrs: &std::collections::HashMap<String, String>,
) -> Option<(String, String)> {
    let hostname = attrs.get("hostname").cloned().filter(|s| !s.is_empty())?;
    let deploy_addr = attrs
        .get("client_addr")
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| hostname.clone());
    Some((deploy_addr, hostname))
}

fn pending_to_sample(
    pending: PendingSample,
    next_command: Option<&str>,
    similarity: Option<f64>,
) -> CompletionSample {
    let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    CompletionSample {
        recorded_at: now,
        session_id: pending.session_id,
        context: pending.context,
        prompt: pending.prompt,
        suggestions: pending.suggestions,
        input: pending.input,
        accepted: pending.accepted,
        next_command: next_command.map(|s| s.to_string()),
        similarity,
        cwd: pending.cwd,
        latency_ms: pending.latency_ms,
    }
}

impl SessionManager {
    /// Create a new SessionManager.
    /// - `omnish_dir`: base directory (e.g., ~/.omnish)
    ///   - sessions are stored in `$omnish_dir/sessions`
    ///   - completion logs are stored in `$omnish_dir/logs/completions`
    pub fn new(omnish_dir: PathBuf, context_config: ContextConfig) -> Self {
        let sessions_dir = omnish_dir.join("sessions");
        let completions_dir = omnish_dir.join("logs").join("completions");
        let session_updates_dir = omnish_dir.join("logs").join("sessions");
        let clients_history_path = omnish_dir.join("clients.json");
        std::fs::create_dir_all(&sessions_dir).ok();
        let completion_writer = omnish_store::completion::spawn_writer_thread(completions_dir);
        let session_writer = omnish_store::session_update::spawn_writer_thread(session_updates_dir);
        let samples_dir = omnish_dir.join("logs").join("samples");
        let sample_writer = omnish_store::sample::spawn_sample_writer(samples_dir);
        let clients_history = crate::clients_history::ClientsHistory::load(&clients_history_path);
        Self {
            base_dir: sessions_dir,
            clients_history: RwLock::new(clients_history),
            clients_history_path,
            sessions: RwLock::new(HashMap::new()),
            context_config,
            completion_writer,
            session_writer,
            history_frozen_until: RwLock::new(None),
            recent_frozen_until: RwLock::new(None),
            last_completion_context: RwLock::new(String::new()),
            sample_writer,
            last_sample_time: Mutex::new(None),
        }
    }

    /// Persist a `(deploy_addr, hostname)` pair to the history index.
    /// No-op when the pair is None.
    async fn touch_clients_history(&self, pair: Option<(String, String)>) {
        let Some((deploy_addr, hostname)) = pair else { return };
        let mut hist = self.clients_history.write().await;
        hist.touch(&deploy_addr, &hostname);
        if let Err(e) = hist.save(&self.clients_history_path) {
            tracing::warn!("clients_history: save failed: {}", e);
        }
    }

    /// Remove all history entries with the given deploy address.
    /// Returns the number of entries removed; 0 means the addr was unknown.
    pub async fn forget_client_addr(&self, deploy_addr: &str) -> usize {
        let mut hist = self.clients_history.write().await;
        let removed = hist.forget_by_addr(deploy_addr);
        if removed > 0 {
            if let Err(e) = hist.save(&self.clients_history_path) {
                tracing::warn!("clients_history: save after forget failed: {}", e);
            }
        }
        removed
    }

    /// Prune entries older than `max_age` from the persisted history.
    /// Called at startup; logs the prune count if > 0.
    pub async fn prune_clients_history(&self, max_age: chrono::Duration) -> usize {
        let mut hist = self.clients_history.write().await;
        let pruned = hist.prune(max_age);
        if pruned > 0 {
            if let Err(e) = hist.save(&self.clients_history_path) {
                tracing::warn!("clients_history: save after prune failed: {}", e);
            }
            tracing::info!(
                "clients_history: pruned {} entries older than {} days",
                pruned,
                max_age.num_days()
            );
        }
        pruned
    }

    pub async fn load_existing(&self) -> Result<usize> {
        let mut count = 0;
        let entries = match std::fs::read_dir(&self.base_dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!("failed to read session store directory: {}", e);
                return Ok(0);
            }
        };

        let mut sessions = self.sessions.write().await;
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("failed to read directory entry: {}", e);
                    continue;
                }
            };
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }

            let mut load = || -> Result<()> {
                let meta = SessionMeta::load(&dir)?;
                let commands = CommandRecord::load_all(&dir)?;
                let stream_path = dir.join("stream.bin");
                let stream_writer = if stream_path.exists() {
                    StreamWriter::open_append(&stream_path)?
                } else {
                    StreamWriter::create(&stream_path)?
                };

                let last_command_stream_pos = commands
                    .last()
                    .map(|cmd| cmd.stream_offset + cmd.stream_length)
                    .unwrap_or(0);

                let last_active = infer_last_active(&commands, &meta);

                let session_id = meta.session_id.clone();
                sessions.insert(
                    session_id,
                    Arc::new(Session {
                        dir: dir.clone(),
                        meta: RwLock::new(meta),
                        commands: RwLock::new(commands),
                        stream_writer: Mutex::new(StreamWriterState {
                            writer: stream_writer,
                            last_command_stream_pos,
                            last_active,
                        }),
                        last_update: Mutex::new(None),
                        pending_sample: Mutex::new(None),
                    }),
                );
                count += 1;
                Ok(())
            };

            if let Err(e) = load() {
                tracing::warn!("removing corrupt session dir {:?}: {}", dir, e);
                if let Err(rm_err) = std::fs::remove_dir_all(&dir) {
                    tracing::error!("failed to remove corrupt session dir {:?}: {}", dir, rm_err);
                }
            }
        }

        // Drop the write lock before calling cleanup_expired_dirs
        // to avoid deadlock (cleanup_expired_dirs acquires a read lock)
        drop(sessions);

        // Clean up expired directories on startup (48 hours)
        let max_age = std::time::Duration::from_secs(48 * 3600);
        let cleaned = self.cleanup_expired_dirs(max_age).await;
        if cleaned > 0 {
            tracing::info!("cleaned up {} expired session directories on startup", cleaned);
        }

        // Prune clients_history entries older than 90 days. Independent of
        // the per-session 48h cleanup so the deploy menu retains hosts that
        // were last touched up to 3 months ago.
        self.prune_clients_history(chrono::Duration::days(90)).await;

        Ok(count)
    }

    pub async fn register(
        &self,
        session_id: &str,
        parent_session_id: Option<String>,
        attrs: std::collections::HashMap<String, String>,
    ) -> Result<()> {
        let history_pair = history_pair_from_attrs(&attrs);

        // Fast path: check if session exists with read lock
        {
            let sessions = self.sessions.read().await;
            if let Some(session) = sessions.get(session_id) {
                let mut meta = session.meta.write().await;
                meta.attrs = attrs;
                meta.save(&session.dir)?;
                tracing::info!("session {} re-registered (reconnect)", session_id);
                drop(meta);
                drop(sessions);
                self.touch_clients_history(history_pair).await;
                return Ok(());
            }
        }

        // Slow path: create new session under write lock
        let mut sessions = self.sessions.write().await;

        // Double-check after acquiring write lock
        if let Some(session) = sessions.get(session_id) {
            let mut meta = session.meta.write().await;
            meta.attrs = attrs;
            meta.save(&session.dir)?;
            tracing::info!("session {} re-registered (reconnect)", session_id);
            drop(meta);
            drop(sessions);
            self.touch_clients_history(history_pair).await;
            return Ok(());
        }

        let now = chrono::Utc::now().to_rfc3339();
        let session_dir = self
            .base_dir
            .join(format!("{}_{}", now.replace(':', "-"), session_id));
        std::fs::create_dir_all(&session_dir)?;

        let meta = SessionMeta {
            session_id: session_id.to_string(),
            parent_session_id,
            started_at: now,
            ended_at: None,
            attrs,
        };
        meta.save(&session_dir)?;

        let stream_writer = StreamWriter::create(&session_dir.join("stream.bin"))?;

        sessions.insert(
            session_id.to_string(),
            Arc::new(Session {
                dir: session_dir,
                meta: RwLock::new(meta),
                commands: RwLock::new(Vec::new()),
                stream_writer: Mutex::new(StreamWriterState {
                    writer: stream_writer,
                    last_command_stream_pos: 0,
                    last_active: Instant::now(),
                }),
                last_update: Mutex::new(None),
                pending_sample: Mutex::new(None),
            }),
        );
        drop(sessions);

        self.touch_clients_history(history_pair).await;
        Ok(())
    }

    pub async fn update_attrs(
        &self,
        session_id: &str,
        timestamp_ms: u64,
        attrs: HashMap<String, String>,
    ) -> Result<()> {
        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("session {} not found", session_id))?;
        let mut meta = session.meta.write().await;

        // Update session attributes
        for (k, v) in &attrs {
            meta.attrs.insert(k.clone(), v.clone());
        }
        meta.save(&session.dir)?;

        // Update last_update timestamp
        let mut last_update = session.last_update.lock().await;
        *last_update = Some(timestamp_ms);

        // Send to session writer for logging (non-blocking)
        let record = omnish_store::session_update::SessionUpdateRecord::new(
            session_id.to_string(),
            timestamp_ms,
            attrs,
        );
        let _ = self.session_writer.send(record);

        Ok(())
    }

    pub async fn write_io(
        &self,
        session_id: &str,
        timestamp_ms: u64,
        direction: u8,
        data: &[u8],
    ) -> Result<()> {
        let session = {
            let sessions = self.sessions.read().await;
            sessions.get(session_id).cloned()
        };
        if let Some(session) = session {
            let mut sw = session.stream_writer.lock().await;
            sw.writer.write_entry(timestamp_ms, direction, data)?;
            sw.last_active = Instant::now();
        }
        Ok(())
    }

    pub async fn receive_command(&self, session_id: &str, mut record: CommandRecord) -> Result<()> {
        let session = {
            let sessions = self.sessions.read().await;
            sessions.get(session_id).cloned()
        };
        if let Some(session) = session {
            // Extract command line before record is moved
            let next_cmd_line = record.command_line.clone();

            // Lock stream_writer to get position info
            let current_pos;
            {
                let mut sw = session.stream_writer.lock().await;
                current_pos = sw.writer.position();
                record.stream_offset = sw.last_command_stream_pos;
                record.stream_length = current_pos - sw.last_command_stream_pos;
                sw.last_command_stream_pos = current_pos;
                sw.last_active = Instant::now();
            }

            // Lock commands to push and save
            let mut commands = session.commands.write().await;
            commands.push(record);
            CommandRecord::save_all(&commands, &session.dir)?;

            // Check pending sample for completion sampling
            let pending = {
                let mut p = session.pending_sample.lock().await;
                p.take()
            };
            if let Some(pending) = pending {
                let next_cmd = next_cmd_line.as_deref().unwrap_or("");
                let elapsed = pending.created_at.elapsed().as_secs();
                if !pending.accepted && !next_cmd.is_empty() && elapsed <= SAMPLE_MAX_ELAPSED_SECS {
                    // Find best similarity across all suggestions
                    let best_sim = pending
                        .suggestions
                        .iter()
                        .map(|s| omnish_store::sample::similarity(s, next_cmd))
                        .fold(0.0_f64, f64::max);

                    if best_sim > SAMPLE_SIMILARITY_THRESHOLD {
                        // Check global rate limit
                        let should_sample = {
                            let mut last = self.last_sample_time.lock().await;
                            let now = Instant::now();
                            let ok = !last.is_some_and(|t| {
                                now.duration_since(t).as_secs() < SAMPLE_RATE_LIMIT_SECS
                            });
                            if ok {
                                *last = Some(now);
                            }
                            ok
                        };
                        if should_sample {
                            let sample = pending_to_sample(pending, Some(next_cmd), Some(best_sim));
                            tracing::info!(
                                "Sampling completion near-miss: sim={:.2}, suggestion={:?}, actual={:?}",
                                best_sim,
                                sample.suggestions.first(),
                                next_cmd
                            );
                            let _ = self.sample_writer.send(sample);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Receive and persist a completion summary for analytics.
    /// This sends the record to the async writer thread.
    pub async fn receive_completion(&self, summary: omnish_protocol::message::CompletionSummary) -> Result<()> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let record = CompletionRecord {
            session_id: summary.session_id,
            sequence_id: summary.sequence_id,
            prompt: summary.prompt,
            completion: summary.completion,
            accepted: summary.accepted,
            latency_ms: summary.latency_ms,
            dwell_time_ms: summary.dwell_time_ms,
            cwd: summary.cwd,
            recorded_at: now_ms,
            extra: summary.extra,
        };
        // Send to writer thread (non-blocking)
        let _ = self.completion_writer.send(record);
        Ok(())
    }

    pub async fn end_session(&self, session_id: &str) -> Result<()> {
        let session = {
            let sessions = self.sessions.read().await;
            sessions.get(session_id).cloned()
        };
        if let Some(session) = session {
            let mut meta = session.meta.write().await;
            meta.ended_at = Some(chrono::Utc::now().to_rfc3339());
            meta.save(&session.dir)?;

            let commands = session.commands.read().await;
            CommandRecord::save_all(&commands, &session.dir)?;

            // Flush any pending sample without next_command
            let pending = {
                let mut p = session.pending_sample.lock().await;
                p.take()
            };
            if let Some(pending) = pending {
                let sample = pending_to_sample(pending, None, None);
                let _ = self.sample_writer.send(sample);
            }
        }
        Ok(())
    }

    /// Store a pending completion sample for a session.
    /// Called from handle_completion_request after getting LLM suggestions.
    pub async fn store_pending_sample(&self, sample: PendingSample) {
        let session = {
            let sessions = self.sessions.read().await;
            sessions.get(&sample.session_id).cloned()
        };
        if let Some(session) = session {
            let mut pending = session.pending_sample.lock().await;
            *pending = Some(sample);
        }
    }

    /// Update the pending sample's accepted flag when CompletionSummary arrives.
    pub async fn update_pending_sample_accepted(&self, session_id: &str, accepted: bool) {
        let session = {
            let sessions = self.sessions.read().await;
            sessions.get(session_id).cloned()
        };
        if let Some(session) = session {
            let mut pending = session.pending_sample.lock().await;
            if let Some(ref mut sample) = *pending {
                sample.accepted = accepted;
            }
        }
    }

    pub async fn get_commands(&self, session_id: &str) -> Result<Vec<CommandRecord>> {
        let session = {
            let sessions = self.sessions.read().await;
            sessions.get(session_id).cloned()
        };
        if let Some(session) = session {
            let commands = session.commands.read().await;
            Ok(commands.clone())
        } else {
            Ok(Vec::new())
        }
    }

    /// Get a single session attribute value by key.
    pub async fn get_session_attr(&self, session_id: &str, key: &str) -> Option<String> {
        let session = {
            let sessions = self.sessions.read().await;
            sessions.get(session_id).cloned()
        };
        if let Some(session) = session {
            let meta = session.meta.read().await;
            meta.attrs.get(key).cloned()
        } else {
            None
        }
    }

    pub async fn get_session_attrs(&self, session_id: &str) -> std::collections::HashMap<String, String> {
        let session = {
            let sessions = self.sessions.read().await;
            sessions.get(session_id).cloned()
        };
        if let Some(session) = session {
            let meta = session.meta.read().await;
            meta.attrs.clone()
        } else {
            std::collections::HashMap::new()
        }
    }

    /// Get session debug information including metadata, commands count, last active time, and last update timestamp
    pub async fn get_session_debug_info(&self, session_id: &str) -> Result<(SessionMeta, usize, Duration, Option<u64>)> {
        let session = {
            let sessions = self.sessions.read().await;
            sessions
                .get(session_id)
                .cloned()
                .ok_or_else(|| anyhow!("session {} not found", session_id))?
        };

        let meta = session.meta.read().await.clone();
        let commands = session.commands.read().await;
        let cmd_count = commands.len();
        drop(commands); // Release the lock early

        let sw = session.stream_writer.lock().await;
        let last_active_duration = sw.last_active.elapsed();
        drop(sw);

        let last_update = session.last_update.lock().await;

        Ok((meta, cmd_count, last_active_duration, *last_update))
    }

    pub async fn list_active(&self) -> Vec<String> {
        let sessions = self.sessions.read().await;
        let mut result = Vec::new();
        for (_, session) in sessions.iter() {
            let meta = session.meta.read().await;
            if meta.ended_at.is_none() {
                result.push(meta.session_id.clone());
            }
        }
        result
    }

    /// Returns `(deploy_addr, hostname, is_active)` triples for clients ever
    /// seen by this daemon. The persisted `clients_history` index is the
    /// source of truth (so hosts that disconnected days ago still appear in
    /// the deploy menu); `is_active` is overlaid from in-memory sessions.
    ///
    /// `deploy_addr` is `attrs["client_addr"]` (the ssh target last used to
    /// deploy this host, e.g. `alice@box1`); otherwise it falls back to
    /// `hostname`. Entries are keyed by `(deploy_addr, hostname)` so the
    /// same host reached via different users renders as separate items.
    /// Sorted by `deploy_addr` then `hostname` for stable menu order.
    pub async fn list_clients(&self) -> Vec<(String, String, bool)> {
        let mut by_client: HashMap<(String, String), bool> = {
            let hist = self.clients_history.read().await;
            hist.list().into_iter().map(|p| (p, false)).collect()
        };

        let session_arcs: Vec<_> = {
            let sessions = self.sessions.read().await;
            sessions.values().cloned().collect()
        };
        for session in &session_arcs {
            let meta = session.meta.read().await;
            let Some((deploy_addr, hostname)) = history_pair_from_attrs(&meta.attrs) else {
                continue;
            };
            let active = meta.ended_at.is_none();
            // Insert with at-least-active=true; a persisted-only entry
            // stays false, but a persisted entry with an active session
            // upgrades to true.
            by_client.entry((deploy_addr, hostname))
                .and_modify(|a| if active { *a = true; })
                .or_insert(active);
        }

        let mut result: Vec<(String, String, bool)> = by_client.into_iter()
            .map(|((addr, host), active)| (addr, host, active))
            .collect();
        result.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        result
    }

    /// Format a human-readable list of active in-memory sessions.
    ///
    /// Only shows active sessions (ended=false). Output is grouped by host,
    /// with hosts ordered by their most-recently-active session (newest first),
    /// and sessions within each host also ordered newest-first.
    /// The current session is marked with a `*` prefix.
    /// Each host section includes a summary line for dead sessions on that host.
    pub async fn format_sessions_list(&self, current_session_id: &str) -> String {
        // Snapshot data under brief locks
        struct SessionSnapshot {
            session_id: String,
            hostname: Option<String>,
            ended: bool,
            last_active: Instant,
            cmd_count: usize,
            context_cmd_count: usize, // number of detailed commands from this session that would be included in context
        }

        // Snapshot session Arcs under brief read lock, then collect data without holding it
        let session_arcs: Vec<_> = {
            let sessions = self.sessions.read().await;
            sessions.values().cloned().collect()
        };

        let mut snapshots = Vec::new();
        let mut all_commands = Vec::new();
        for session in &session_arcs {
            let commands = session.commands.read().await;
            if commands.is_empty() {
                continue;
            }
            let meta = session.meta.read().await;
            let sw = session.stream_writer.lock().await;
            let cmd_count = commands.iter().filter(|c| c.command_line.is_some()).count();
            snapshots.push(SessionSnapshot {
                session_id: meta.session_id.clone(),
                hostname: meta.attrs.get("hostname").cloned(),
                ended: meta.ended_at.is_some(),
                last_active: sw.last_active,
                cmd_count,
                context_cmd_count: 0,
            });
            all_commands.extend(commands.iter().filter(|c| c.command_line.is_some()).cloned());
        }

        if snapshots.is_empty() {
            return "(no sessions)".to_string();
        }

        // Calculate how many detailed commands from each session would be included in context
        if !all_commands.is_empty() {
            // Sort commands by started_at (chronological order)
            all_commands.sort_by_key(|c| c.started_at);

            let total = self.context_config.completion.detailed_commands + self.context_config.completion.history_commands;
            let strategy = RecentCommands::new(total)
                .with_current_session(current_session_id, self.context_config.completion.min_current_session_commands);

            // Use the same select+split logic as build_context_with_session
            let (_history_cmds, detailed_cmds) = omnish_context::select_and_split(
                &strategy,
                &all_commands,
                self.context_config.completion.detailed_commands,
                Some(current_session_id),
                self.context_config.completion.min_current_session_commands,
            ).await;

            // Count detailed commands per session
            let mut detailed_counts: HashMap<String, usize> = HashMap::new();
            for cmd in detailed_cmds {
                *detailed_counts.entry(cmd.session_id.clone()).or_insert(0) += 1;
            }

            // Update context_cmd_count in snapshots (now represents detailed commands only)
            for snapshot in &mut snapshots {
                if let Some(count) = detailed_counts.get(&snapshot.session_id) {
                    snapshot.context_cmd_count = *count;
                }
                // else remains 0
            }
        }

        let mut entries = snapshots;
        entries.sort_by_key(|e| std::cmp::Reverse(e.last_active));

        // Group by host and separate active/dead sessions
        let mut host_sessions: HashMap<String, Vec<&SessionSnapshot>> = HashMap::new();
        let mut dead_stats_by_host: HashMap<String, (usize, usize)> = HashMap::new(); // (session_count, total_commands)

        for s in &entries {
            let host = s.hostname.as_deref().unwrap_or("?").to_string();

            if s.ended {
                // Count dead sessions for statistics
                let stats = dead_stats_by_host.entry(host.clone()).or_insert((0, 0));
                stats.0 += 1; // session count
                stats.1 += s.cmd_count; // total commands
            } else {
                // Add active sessions to display
                host_sessions.entry(host).or_default().push(s);
            }
        }

        // Determine host order based on most recent active session
        let mut host_info: Vec<(String, Vec<&SessionSnapshot>, (usize, usize))> = Vec::new();
        for (host, active_sessions) in host_sessions {
            let dead_stats = dead_stats_by_host.get(&host).copied().unwrap_or((0, 0));
            host_info.push((host, active_sessions, dead_stats));
        }

        // Sort by most recent active session within each host
        host_info.sort_by(|a, b| {
            let latest_a = a.1.first().map(|s| s.last_active).unwrap_or(Instant::now() - Duration::from_secs(3600));
            let latest_b = b.1.first().map(|s| s.last_active).unwrap_or(Instant::now() - Duration::from_secs(3600));
            latest_b.cmp(&latest_a) // newest first
        });

        let mut lines = Vec::new();
        for (host, active_sessions, dead_stats) in host_info {
            lines.push(format!("[{}]", host));

            // Display active sessions (sorted newest first within host)
            for s in active_sessions {
                let idle = format_idle(s.last_active.elapsed().as_secs());
                let is_current = s.session_id == current_session_id;
                let marker = if is_current { "*" } else { " " };
                let (color_start, color_end) = if is_current {
                    ("\x1b[1;37m", "\x1b[0m")
                } else {
                    ("\x1b[2m", "\x1b[0m")
                };
                lines.push(format!(
                    "  {}{} {} [active] cmds={}/{} idle={}{}",
                    color_start, marker, s.session_id, s.context_cmd_count, s.cmd_count, idle, color_end,
                ));
            }

            // Add dead sessions summary for this host
            let (dead_sessions, dead_commands) = dead_stats;
            if dead_sessions > 0 {
                lines.push(format!(
                    "  {} dead session(s), {} command(s)",
                    dead_sessions, dead_commands
                ));
            }
        }
        lines.join("\n")
    }

    /// Remove sessions that have been inactive longer than `max_inactive`.
    /// Data is already persisted on disk; evicted sessions will be reloaded
    /// on demand if they reconnect via `register()`.
    pub async fn evict_inactive(&self, max_inactive: std::time::Duration) -> usize {
        // Phase 1: scan under read lock to find candidates (doesn't block other readers)
        let to_remove = {
            let sessions = self.sessions.read().await;
            let mut candidates = Vec::new();
            for (sid, session) in sessions.iter() {
                let sw = session.stream_writer.lock().await;
                if sw.last_active.elapsed() >= max_inactive {
                    candidates.push(sid.clone());
                }
            }
            candidates
        };

        if to_remove.is_empty() {
            return 0;
        }

        // Phase 2: remove under write lock (brief, no nested awaits)
        let mut sessions = self.sessions.write().await;
        let mut evicted = 0;
        for sid in &to_remove {
            if sessions.remove(sid).is_some() {
                evicted += 1;
            }
        }

        if evicted > 0 {
            tracing::info!("evicted {} inactive session(s) from memory", evicted);
        }
        evicted
    }

    /// Clean up session directories that have been inactive longer than `max_age`.
    /// Returns the number of directories deleted.
    pub async fn cleanup_expired_dirs(&self, max_age: std::time::Duration) -> usize {
        let mut cleaned = 0;

        // Snapshot loaded session IDs under brief read lock, then release
        let loaded_ids: std::collections::HashSet<String> = {
            let sessions = self.sessions.read().await;
            sessions.keys().cloned().collect()
        };

        // Get list of directories in base_dir
        let entries = match std::fs::read_dir(&self.base_dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!("failed to read session store directory: {}", e);
                return 0;
            }
        };

        // Helper function to parse RFC3339 timestamp to milliseconds
        let parse_rfc3339_ms = |s: &str| -> Option<u64> {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|dt| dt.timestamp_millis() as u64)
        };

        // Get current time in milliseconds once
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("failed to read directory entry: {}", e);
                    continue;
                }
            };

            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }

            // Extract session ID from directory name (format: timestamp_sessionid)
            // Note: sessionid can contain underscores, so we need to split on first underscore
            let dir_name = dir.file_name().and_then(|n| n.to_str());
            let session_id = dir_name.and_then(|name| {
                name.find('_').map(|pos| &name[pos + 1..]) // Everything after first underscore
            });

            // Skip if this session is currently loaded in memory
            if let Some(sid) = session_id {
                if loaded_ids.contains(sid) {
                    continue;
                }
            }

            // Try to get last activity timestamp from commands.json first
            let last_activity_ms = match CommandRecord::load_all(&dir) {
                Ok(commands) => {
                    // Get last command timestamp (ended_at or started_at)
                    commands
                        .last()
                        .and_then(|cmd| cmd.ended_at.or(Some(cmd.started_at)))
                }
                Err(e) => {
                    tracing::warn!("failed to load commands.json from {:?}: {}", dir, e);
                    None
                }
            };

            // If no commands or commands.json doesn't exist, try meta.json
            let last_activity_ms = match last_activity_ms {
                Some(ms) => Some(ms),
                None => {
                    match SessionMeta::load(&dir) {
                        Ok(meta) => {
                            // Use same logic as infer_last_active: ended_at or started_at
                            meta.ended_at
                                .as_deref()
                                .and_then(parse_rfc3339_ms)
                                .or_else(|| parse_rfc3339_ms(&meta.started_at))
                        }
                        Err(e) => {
                            tracing::warn!("failed to load meta.json from {:?}: {}", dir, e);
                            None
                        }
                    }
                }
            };

            match last_activity_ms {
                Some(ms) => {
                    let age = Duration::from_millis(now_ms.saturating_sub(ms));
                    if age >= max_age {
                        match std::fs::remove_dir_all(&dir) {
                            Ok(_) => {
                                tracing::info!("cleaned up expired session directory: {:?}", dir);
                                cleaned += 1;
                            }
                            Err(e) => {
                                tracing::error!("failed to delete expired session directory {:?}: {}", dir, e);
                            }
                        }
                    }
                }
                None => {
                    // No timestamp found - could be corrupt or empty directory
                    // We'll skip it for safety
                    tracing::debug!("skipping directory with no timestamp: {:?}", dir);
                    continue;
                }
            }
        }

        cleaned
    }

    pub async fn get_session_context(&self, session_id: &str) -> Result<String> {
        self.get_session_context_with_limit(session_id, self.context_config.completion.max_context_chars).await
    }

    /// Get session context for chat (without history, only recent commands with output).
    /// This is used for LLM chat requests where we only want recent commands.
    pub async fn get_chat_context(&self, session_id: &str, max_context_chars: Option<usize>) -> Result<String> {
        // Clone data under brief locks
        let (commands, stream_path, hostnames) = {
            let sessions = self.sessions.read().await;
            let session = sessions
                .get(session_id)
                .ok_or_else(|| anyhow!("session not found: {}", session_id))?;

            let cmds = session.commands.read().await.clone();
            let path = session.dir.join("stream.bin");
            let meta = session.meta.read().await;
            let mut hostnames = HashMap::new();
            if let Some(h) = meta.attrs.get("hostname") {
                hostnames.insert(session_id.to_string(), h.clone());
            }
            (cmds, path, hostnames)
        };

        // Build context outside all locks - expensive I/O happens here
        let reader = FileStreamReader { stream_path };
        let cc = &self.context_config;

        // Build context with NO history (only detailed commands with output)
        self.build_context_with_limit(
            &commands,
            &reader,
            &hostnames,
            session_id,
            cc.completion.detailed_commands,
            0, // No history for chat
            cc.completion.min_current_session_commands,
            cc.completion.max_line_width,
            max_context_chars,
        )
        .await
    }

    /// Get all sessions context for chat (without history, only recent commands with output).
    /// This is used for LLM chat requests where we only want recent commands with output.
    pub async fn get_all_sessions_chat_context(&self, current_session_id: &str, max_context_chars: Option<usize>) -> Result<String> {
        let cc = &self.context_config;

        // Snapshot session Arcs under brief read lock
        let session_entries: Vec<_> = {
            let sessions = self.sessions.read().await;
            sessions.iter().map(|(sid, s)| (sid.clone(), s.clone())).collect()
        };

        let mut all_commands = Vec::new();
        let mut offset_to_path: HashMap<(u64, u64), PathBuf> = HashMap::new();
        let mut hostnames: HashMap<String, String> = HashMap::new();
        for (sid, session) in &session_entries {
            let stream_path = session.dir.join("stream.bin");
            let meta = session.meta.read().await;
            if let Some(h) = meta.attrs.get("hostname") {
                hostnames.insert(sid.clone(), h.clone());
            }
            let commands = session.commands.read().await;
            for cmd in commands.iter() {
                offset_to_path
                    .insert((cmd.stream_offset, cmd.stream_length), stream_path.clone());
            }
            all_commands.extend(commands.clone());
        }

        all_commands.sort_by_key(|c| c.started_at);

        if all_commands.is_empty() {
            return Ok(String::new());
        }

        // Build context outside all locks
        let reader = MultiSessionReader {
            readers: offset_to_path,
        };

        // Build context with NO history (only detailed commands with output)
        self.build_context_with_limit(
            &all_commands,
            &reader,
            &hostnames,
            current_session_id,
            cc.completion.detailed_commands,
            0, // No history for chat
            cc.completion.min_current_session_commands,
            cc.completion.max_line_width,
            max_context_chars,
        )
        .await
    }

    /// Get all commands and a stream reader across all sessions (for tool-use).
    pub async fn get_all_commands_with_reader(&self) -> (Vec<CommandRecord>, Arc<dyn StreamReader>) {
        let session_entries: Vec<_> = {
            let sessions = self.sessions.read().await;
            sessions.values().cloned().collect()
        };

        let mut all_commands = Vec::new();
        let mut offset_to_path: HashMap<(u64, u64), PathBuf> = HashMap::new();
        for session in &session_entries {
            let stream_path = session.dir.join("stream.bin");
            let commands = session.commands.read().await;
            for cmd in commands.iter() {
                offset_to_path.insert((cmd.stream_offset, cmd.stream_length), stream_path.clone());
            }
            all_commands.extend(commands.clone());
        }
        all_commands.sort_by_key(|c| c.started_at);

        let reader: Arc<dyn StreamReader> = Arc::new(MultiSessionReader { readers: offset_to_path });
        (all_commands, reader)
    }

    /// Get session context with explicit max_context_chars limit (overrides config)
    pub async fn get_session_context_with_limit(&self, session_id: &str, max_context_chars: Option<usize>) -> Result<String> {
        // Clone data under brief locks
        let (commands, stream_path, hostnames) = {
            let sessions = self.sessions.read().await;
            let session = sessions
                .get(session_id)
                .ok_or_else(|| anyhow!("session not found: {}", session_id))?;

            let cmds = session.commands.read().await.clone();
            let path = session.dir.join("stream.bin");
            let meta = session.meta.read().await;
            let mut hostnames = HashMap::new();
            if let Some(h) = meta.attrs.get("hostname") {
                hostnames.insert(session_id.to_string(), h.clone());
            }
            (cmds, path, hostnames)
        };

        // Build context outside all locks - expensive I/O happens here
        let reader = FileStreamReader { stream_path };
        let cc = &self.context_config;

        // Build context with character limit handling
        self.build_context_with_limit(
            &commands,
            &reader,
            &hostnames,
            session_id,
            cc.completion.detailed_commands,
            cc.completion.history_commands,
            cc.completion.min_current_session_commands,
            cc.completion.max_line_width,
            max_context_chars,
        )
        .await
    }

    /// Build context with automatic reduction of command count if character limit is exceeded
    #[allow(clippy::too_many_arguments)]
    async fn build_context_with_limit(
        &self,
        commands: &[CommandRecord],
        reader: &dyn StreamReader,
        hostnames: &HashMap<String, String>,
        current_session_id: &str,
        detailed_commands: usize,
        history_commands: usize,
        min_current_session_commands: usize,
        max_line_width: usize,
        max_context_chars: Option<usize>,
    ) -> Result<String> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let formatter = GroupedFormatter::new(current_session_id, now_ms, self.context_config.completion.head_lines, self.context_config.completion.tail_lines);

        // Start with the original values
        let mut current_detailed = detailed_commands;
        let mut current_history = history_commands;

        // If no character limit, build directly
        if max_context_chars.is_none() {
            let total = current_detailed + current_history;
            let strategy = RecentCommands::new(total)
                .with_current_session(current_session_id, min_current_session_commands);
            return omnish_context::build_context_with_session(
                &strategy,
                &formatter,
                commands,
                reader,
                hostnames,
                current_detailed,
                max_line_width,
                Some(current_session_id),
                min_current_session_commands,
            )
            .await;
        }

        let max_chars = max_context_chars.unwrap();
        let mut context = String::new();
        let mut reduced = false;

        // Try building context, reducing by 1/4 each iteration if limit exceeded
        loop {
            let total = current_detailed + current_history;
            // Ensure we have at least some commands
            if total == 0 {
                break;
            }

            let strategy = RecentCommands::new(total)
                .with_current_session(current_session_id, min_current_session_commands);

            context = omnish_context::build_context_with_session(
                &strategy,
                &formatter,
                commands,
                reader,
                hostnames,
                current_detailed,
                max_line_width,
                Some(current_session_id),
                min_current_session_commands,
            )
            .await?;

            if context.chars().count() <= max_chars {
                break;
            }

            // Reduce by 1/4, but ensure we don't go below minimums
            let reduction = (total / 4).max(1);
            if reduction >= total {
                break; // Can't reduce further
            }

            // Reduce proportionally - keep the ratio between detailed and history
            let ratio = if current_detailed + current_history > 0 {
                current_detailed as f64 / (current_detailed + current_history) as f64
            } else {
                0.0
            };

            let new_total = total - reduction;
            current_detailed = (new_total as f64 * ratio) as usize;
            current_history = new_total - current_detailed;

            // Ensure minimums
            if current_detailed > 0 && current_detailed < min_current_session_commands {
                current_detailed = min_current_session_commands.min(new_total);
                current_history = new_total.saturating_sub(current_detailed);
            }

            reduced = true;
        }

        if reduced {
            tracing::debug!(
                "context reduced: detailed={}, history={} (limit={})",
                current_detailed,
                current_history,
                max_chars
            );
        }

        Ok(context)
    }

    /// Collect commands from all sessions where `started_at >= since_ms`.
    /// Returns `(hostname, CommandRecord)` pairs sorted by `started_at`.
    pub async fn collect_recent_commands(&self, since_ms: u64) -> Vec<(String, CommandRecord)> {
        // Snapshot session Arcs under brief read lock
        let session_arcs: Vec<_> = {
            let sessions = self.sessions.read().await;
            sessions.values().cloned().collect()
        };

        let mut result = Vec::new();
        for session in &session_arcs {
            let meta = session.meta.read().await;
            let hostname = meta.attrs.get("hostname").cloned().unwrap_or_default();
            let commands = session.commands.read().await;
            for cmd in commands.iter() {
                if cmd.started_at >= since_ms {
                    result.push((hostname.clone(), cmd.clone()));
                }
            }
        }
        result.sort_by_key(|(_, cmd)| cmd.started_at);
        result
    }

    pub async fn get_all_sessions_context(&self, current_session_id: &str) -> Result<String> {
        self.get_all_sessions_context_with_limit(current_session_id, self.context_config.completion.max_context_chars).await
    }

    /// Get all sessions context with explicit max_context_chars limit (overrides config)
    pub async fn get_all_sessions_context_with_limit(&self, current_session_id: &str, max_context_chars: Option<usize>) -> Result<String> {
        let cc = &self.context_config;

        // Snapshot session Arcs under brief read lock
        let session_entries: Vec<_> = {
            let sessions = self.sessions.read().await;
            sessions.iter().map(|(sid, s)| (sid.clone(), s.clone())).collect()
        };

        let mut all_commands = Vec::new();
        let mut offset_to_path: HashMap<(u64, u64), PathBuf> = HashMap::new();
        let mut hostnames: HashMap<String, String> = HashMap::new();
        for (sid, session) in &session_entries {
            let stream_path = session.dir.join("stream.bin");
            let meta = session.meta.read().await;
            if let Some(h) = meta.attrs.get("hostname") {
                hostnames.insert(sid.clone(), h.clone());
            }
            let commands = session.commands.read().await;
            for cmd in commands.iter() {
                offset_to_path
                    .insert((cmd.stream_offset, cmd.stream_length), stream_path.clone());
            }
            all_commands.extend(commands.clone());
        }

        all_commands.sort_by_key(|c| c.started_at);

        if all_commands.is_empty() {
            return Ok(String::new());
        }

        // Build context outside all locks
        let reader = MultiSessionReader {
            readers: offset_to_path,
        };

        // Build context with character limit handling
        self.build_context_with_limit(
            &all_commands,
            &reader,
            &hostnames,
            current_session_id,
            cc.completion.detailed_commands,
            cc.completion.history_commands,
            cc.completion.min_current_session_commands,
            cc.completion.max_line_width,
            max_context_chars,
        )
        .await
    }

    /// Build completion-specific context optimized for KV cache hit rate.
    ///
    /// Uses elastic window: detailed count stays in [detailed_min, detailed_max].
    /// New commands append to the end (prefix-stable). When count exceeds
    /// `detailed_max`, it resets to `detailed_min` by evicting oldest into history.
    /// Uses `CompletionFormatter` for interleaved, uniform-label formatting.
    pub async fn build_completion_context(
        &self,
        current_session_id: &str,
        max_context_chars: Option<usize>,
    ) -> Result<String> {
        let sections = self
            .build_completion_sections(current_session_id, max_context_chars)
            .await?;
        if sections.stable_prefix.is_empty() && sections.remainder.is_empty() {
            Ok(String::new())
        } else {
            Ok(format!("{}{}", sections.stable_prefix, sections.remainder))
        }
    }

    /// Build completion context split into a cacheable `stable_prefix` and a
    /// non-cacheable `remainder`. The prefix contains history plus the warmup
    /// portion of recent commands; the remainder contains tail commands, the
    /// closing `</recent>`, and the system-reminder trailer.
    ///
    /// The `recent_frozen_until` timestamp controls the split: commands with
    /// `started_at <= recent_frozen_until` go into stable_prefix; newer ones
    /// into remainder. This value is advanced only at warmup time, so
    /// stable_prefix stays byte-identical between warmups -> Anthropic's KV
    /// cache hits across consecutive completion requests.
    pub async fn build_completion_sections(
        &self,
        current_session_id: &str,
        max_context_chars: Option<usize>,
    ) -> Result<CompletionSections> {
        let cc = &self.context_config.completion;

        // Snapshot session Arcs under brief read lock
        let session_entries: Vec<_> = {
            let sessions = self.sessions.read().await;
            sessions.iter().map(|(sid, s)| (sid.clone(), s.clone())).collect()
        };

        let mut all_commands = Vec::new();
        let mut offset_to_path: HashMap<(u64, u64), PathBuf> = HashMap::new();
        let mut hostnames: HashMap<String, String> = HashMap::new();
        let mut live_cwd: Option<String> = None;
        for (sid, session) in &session_entries {
            let stream_path = session.dir.join("stream.bin");
            let meta = session.meta.read().await;
            if let Some(h) = meta.attrs.get("hostname") {
                hostnames.insert(sid.clone(), h.clone());
            }
            if sid == current_session_id {
                if let Some(cwd) = meta.attrs.get("shell_cwd") {
                    live_cwd = Some(omnish_context::shorten_home(cwd));
                }
            }
            let commands = session.commands.read().await;
            for cmd in commands.iter() {
                offset_to_path
                    .insert((cmd.stream_offset, cmd.stream_length), stream_path.clone());
            }
            all_commands.extend(commands.clone());
        }

        all_commands.sort_by_key(|c| c.started_at);

        if all_commands.is_empty() {
            return Ok(CompletionSections::default());
        }

        // Filter meaningful (non-empty command_line) and sort by started_at
        let meaningful: Vec<&CommandRecord> = all_commands
            .iter()
            .filter(|c| c.command_line.is_some())
            .collect();

        if meaningful.is_empty() {
            return Ok(CompletionSections::default());
        }

        // --- Frozen history split ---
        // Between resets, history_frozen_until is stable, so the History section
        // is byte-identical across consecutive requests -> KV cache prefix reuse.
        let (selected_commands, detailed_count) = {
            let mut frozen = self.history_frozen_until.write().await;

            let cutoff_ts = match *frozen {
                Some(ts) => ts,
                None => {
                    // First call: take latest (history + detailed_min) commands
                    let total_desired = cc.history_commands + cc.detailed_min;
                    let start = meaningful.len().saturating_sub(total_desired);
                    let selected = &meaningful[start..];
                    let split = selected.len().saturating_sub(cc.detailed_min);
                    let ts = if split > 0 {
                        selected[split - 1].started_at
                    } else {
                        0
                    };
                    *frozen = Some(ts);
                    ts
                }
            };

            // Split by frozen timestamp
            let mut detailed: Vec<&CommandRecord> = meaningful
                .iter()
                .filter(|c| c.started_at > cutoff_ts)
                .copied()
                .collect();

            // Elastic reset: if detailed exceeds max, move oldest into history
            let final_cutoff = if detailed.len() > cc.detailed_max {
                // Keep latest detailed_min as detailed
                let new_split = detailed.len() - cc.detailed_min;
                let new_ts = detailed[new_split - 1].started_at;
                *frozen = Some(new_ts);
                // Re-filter with new cutoff
                detailed = meaningful
                    .iter()
                    .filter(|c| c.started_at > new_ts)
                    .copied()
                    .collect();
                new_ts
            } else {
                cutoff_ts
            };

            // History: latest N commands with started_at <= cutoff
            let history_pool: Vec<&CommandRecord> = meaningful
                .iter()
                .filter(|c| c.started_at <= final_cutoff)
                .copied()
                .collect();
            let history_start = history_pool.len().saturating_sub(cc.history_commands);
            let history = &history_pool[history_start..];

            // Combine history + detailed into a single sorted list
            let mut selected: Vec<CommandRecord> = history
                .iter()
                .chain(detailed.iter())
                .map(|c| (*c).clone())
                .collect();
            selected.sort_by_key(|c| c.started_at);

            let det_count = detailed.len();
            (selected, det_count)
        };

        let reader = MultiSessionReader {
            readers: offset_to_path,
        };
        let formatter = CompletionFormatter::new(
            current_session_id,
            cc.head_lines,
            cc.tail_lines,
        ).with_live_cwd(live_cwd);

        let total = selected_commands.len();
        // Lazy-initialize recent_frozen_until to the latest command's timestamp.
        // Without this, the first build sees `None` and places every command in
        // stable_prefix; the next build (after a new command arrives) would
        // sweep that new command into stable_prefix too, breaking byte-stability
        // and triggering an Anthropic cache miss. Freezing on first use pins
        // stable_prefix so subsequent new commands land in remainder.
        let warmup_cutoff = {
            let mut frozen = self.recent_frozen_until.write().await;
            if frozen.is_none() {
                *frozen = selected_commands.iter().map(|c| c.started_at).max();
            }
            *frozen
        };

        // Render sections for the current (history, detailed) counts. When a
        // character limit is set, reduce detailed first, then history, until
        // it fits.
        let max_chars = max_context_chars;
        let mut current_detailed = detailed_count;
        let mut current_history = total.saturating_sub(detailed_count);

        loop {
            let current_total = current_detailed + current_history;
            if current_total == 0 {
                return Ok(CompletionSections::default());
            }

            let strategy = RecentCommands::new(current_total);
            let (hist_ctx, det_ctx) = omnish_context::build_command_contexts_with_session(
                &strategy,
                &selected_commands,
                &reader,
                &hostnames,
                current_detailed,
                cc.max_line_width,
                Some(current_session_id),
                0,
            )
            .await?;
            let sections = formatter.format_sections(&hist_ctx, &det_ctx, warmup_cutoff);

            let fits = match max_chars {
                None => true,
                Some(limit) => {
                    let total_len = sections.stable_prefix.chars().count()
                        + sections.remainder.chars().count();
                    total_len <= limit
                }
            };
            if fits {
                return Ok(sections);
            }

            // Reduce detailed first to preserve history prefix stability
            if current_detailed > 1 {
                let reduction = (current_detailed / 4).max(1);
                current_detailed = current_detailed.saturating_sub(reduction);
            } else if current_history > 0 {
                let reduction = (current_history / 4).max(1);
                current_history = current_history.saturating_sub(reduction);
            } else {
                return Ok(sections);
            }
        }
    }

    /// Build completion context and compare with cached version.
    /// Returns `Some(new_context)` if the prefix changed enough to warrant a KV cache warmup
    /// (shared prefix ratio < 0.66), `None` otherwise.
    /// Always updates the cached context.
    pub async fn check_and_warmup_context(
        &self,
        session_id: &str,
        max_context_chars: Option<usize>,
    ) -> Result<Option<String>> {
        self.check_and_warmup_sections(session_id, max_context_chars)
            .await
            .map(|opt| opt.map(|s| format!("{}{}", s.stable_prefix, s.remainder)))
    }

    /// Sections-aware variant of `check_and_warmup_context`.
    ///
    /// Returns `Some(sections)` when the completion prefix has drifted enough
    /// to warrant a warmup request. Before returning, advances
    /// `recent_frozen_until` to the timestamp of the most recent command and
    /// rebuilds the sections so the returned `stable_prefix` sweeps in all
    /// current recent commands. Subsequent completion requests that reuse the
    /// same `recent_frozen_until` then see a byte-identical `stable_prefix`,
    /// so Anthropic's KV cache hits on the cached breakpoint.
    pub async fn check_and_warmup_sections(
        &self,
        session_id: &str,
        max_context_chars: Option<usize>,
    ) -> Result<Option<CompletionSections>> {
        let new_sections = self
            .build_completion_sections(session_id, max_context_chars)
            .await?;
        let new_context = if new_sections.stable_prefix.is_empty()
            && new_sections.remainder.is_empty()
        {
            String::new()
        } else {
            format!("{}{}", new_sections.stable_prefix, new_sections.remainder)
        };

        let mut cached = self.last_completion_context.write().await;
        let old_context = std::mem::replace(&mut *cached, new_context.clone());
        drop(cached);

        if old_context.is_empty() {
            // First build - no previous context to compare against
            return Ok(None);
        }

        let common_prefix_len = old_context
            .bytes()
            .zip(new_context.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let ratio = common_prefix_len as f64 / old_context.len() as f64;

        if ratio >= 0.66 {
            return Ok(None);
        }

        tracing::debug!(
            "KV cache warmup needed: prefix ratio={:.3} (common={}/old={})",
            ratio,
            common_prefix_len,
            old_context.len()
        );

        // Advance recent_frozen_until to the latest command's started_at so the
        // warmup's stable_prefix sweeps in all current recent commands. After
        // this, consecutive completion requests reuse the same cutoff and see
        // a byte-identical stable_prefix.
        let latest_ts = self.latest_command_started_at().await;
        if let Some(ts) = latest_ts {
            let mut frozen = self.recent_frozen_until.write().await;
            *frozen = Some(ts);
        }

        // Rebuild sections with the advanced cutoff so the returned payload
        // matches what subsequent requests will send.
        let rebuilt = self
            .build_completion_sections(session_id, max_context_chars)
            .await?;

        // Keep last_completion_context in sync with the rebuilt form so the
        // next ratio comparison starts from the new baseline.
        {
            let mut cached = self.last_completion_context.write().await;
            *cached = if rebuilt.stable_prefix.is_empty() && rebuilt.remainder.is_empty() {
                String::new()
            } else {
                format!("{}{}", rebuilt.stable_prefix, rebuilt.remainder)
            };
        }

        Ok(Some(rebuilt))
    }

    /// Return the maximum `started_at` across all registered sessions, or
    /// `None` if no commands exist.
    async fn latest_command_started_at(&self) -> Option<u64> {
        let session_entries: Vec<_> = {
            let sessions = self.sessions.read().await;
            sessions.values().cloned().collect()
        };
        let mut latest: Option<u64> = None;
        for session in &session_entries {
            let commands = session.commands.read().await;
            if let Some(last) = commands.last() {
                let ts = last.started_at;
                latest = Some(match latest {
                    Some(prev) => prev.max(ts),
                    None => ts,
                });
            }
        }
        latest
    }

    /// Get the last completion context (read-only) for logging/analytics purposes.
    /// Returns the cached context from the previous build, or empty string if no previous context.
    pub async fn get_last_completion_context(&self) -> String {
        let cached = self.last_completion_context.read().await;
        cached.clone()
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_load_existing_restores_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        // Note: completions_dir is no longer needed - SessionManager handles it internally

        // Register a session and add a command via normal flow
        {
            let mgr = SessionManager::new(base.clone(), Default::default());
            mgr.register("sess1", None, Default::default())
                .await
                .unwrap();
            mgr.write_io("sess1", 100, 0, b"$ ls\n").await.unwrap();
            mgr.write_io("sess1", 200, 1, b"file.txt\n").await.unwrap();
            mgr.receive_command(
                "sess1",
                CommandRecord {
                    command_id: "cmd1".into(),
                    session_id: "sess1".into(),
                    command_line: Some("ls".into()),
                    cwd: Some("/tmp".into()),
                    started_at: 100,
                    ended_at: Some(200),
                    output_summary: "file.txt".into(),
                    stream_offset: 0,
                    stream_length: 0,
                    exit_code: None,
                },
            )
            .await
            .unwrap();
            // Drop the manager (simulates daemon shutdown)
        }

        // Create a new manager on the same directory and load existing sessions
        let mgr2 = SessionManager::new(base, Default::default());
        let count = mgr2.load_existing().await.unwrap();
        assert_eq!(count, 1);

        let commands = mgr2.get_commands("sess1").await.unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command_line.as_deref(), Some("ls"));

        let active = mgr2.list_active().await;
        assert!(active.contains(&"sess1".to_string()));
    }

    #[tokio::test]
    async fn test_format_sessions_list_shows_only_active_with_dead_stats() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let mgr = SessionManager::new(base, Default::default());

        // Register three sessions with commands, all on the same host (default "?")
        mgr.register("active1", None, Default::default())
            .await
            .unwrap();
        mgr.register("active2", None, Default::default())
            .await
            .unwrap();
        mgr.register("dead1", None, Default::default())
            .await
            .unwrap();

        // Add commands to all sessions
        mgr.receive_command(
            "active1",
            CommandRecord {
                command_id: "cmd1".into(),
                session_id: "active1".into(),
                command_line: Some("ls".into()),
                cwd: Some("/tmp".into()),
                started_at: 100,
                ended_at: Some(200),
                output_summary: "".into(),
                stream_offset: 0,
                stream_length: 0,
                exit_code: None,
            },
        )
        .await
        .unwrap();

        mgr.receive_command(
            "active2",
            CommandRecord {
                command_id: "cmd2".into(),
                session_id: "active2".into(),
                command_line: Some("pwd".into()),
                cwd: Some("/home".into()),
                started_at: 300,
                ended_at: Some(400),
                output_summary: "".into(),
                stream_offset: 0,
                stream_length: 0,
                exit_code: None,
            },
        )
        .await
        .unwrap();

        mgr.receive_command(
            "dead1",
            CommandRecord {
                command_id: "cmd3".into(),
                session_id: "dead1".into(),
                command_line: Some("echo dead".into()),
                cwd: Some("/var".into()),
                started_at: 500,
                ended_at: Some(600),
                output_summary: "".into(),
                stream_offset: 0,
                stream_length: 0,
                exit_code: None,
            },
        )
        .await
        .unwrap();

        // End the dead session
        mgr.end_session("dead1").await.unwrap();

        // Test with active1 as current
        let output = mgr.format_sessions_list("active1").await;

        // Should only show active sessions
        assert!(output.contains("* active1"), "Should highlight current active session: {}", output);
        assert!(output.contains("  active2 [active]"), "Should show other active session: {}", output);
        assert!(!output.contains("dead1"), "Should not show dead sessions in list: {}", output);

        // Should show dead session statistics for the host
        assert!(output.contains("1 dead session(s), 1 command(s)"), "Should show dead session stats: {}", output);

        // All sessions should have [active] status, not [ended]
        assert!(!output.contains("[ended]"), "Should not show ended status: {}", output);
        assert!(output.contains("[active]"), "Should show active status: {}", output);
    }

    #[tokio::test]
    async fn test_format_sessions_list_with_multiple_dead_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let mgr = SessionManager::new(base, Default::default());

        // Create 1 active and 2 dead sessions, all on the same host
        mgr.register("active1", None, Default::default())
            .await
            .unwrap();
        mgr.register("dead1", None, Default::default())
            .await
            .unwrap();
        mgr.register("dead2", None, Default::default())
            .await
            .unwrap();

        // Add commands
        for (session, cmd_count) in &[("active1", 1), ("dead1", 3), ("dead2", 2)] {
            for i in 0..*cmd_count {
                mgr.receive_command(
                    session,
                    CommandRecord {
                        command_id: format!("cmd{}_{}", session, i),
                        session_id: session.to_string(),
                        command_line: Some(format!("command{}", i)),
                        cwd: Some("/tmp".into()),
                        started_at: 100 + i as u64,
                        ended_at: Some(200 + i as u64),
                        output_summary: "".into(),
                        stream_offset: 0,
                        stream_length: 0,
                        exit_code: None,
                    },
                )
                .await
                .unwrap();
            }
        }

        // End dead sessions
        mgr.end_session("dead1").await.unwrap();
        mgr.end_session("dead2").await.unwrap();

        let output = mgr.format_sessions_list("active1").await;

        // Should show correct dead session statistics
        assert!(output.contains("2 dead session(s), 5 command(s)"),
                "Should aggregate dead session stats: {}", output);
        assert!(output.contains("* active1 [active] cmds=1/1"),
                "Should show active session with command count: {}", output);
    }

    #[tokio::test]
    async fn test_format_sessions_list_demo_output() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let mgr = SessionManager::new(base, Default::default());

        // Create sessions on different hosts
        let mut attrs = std::collections::HashMap::new();
        attrs.insert("hostname".to_string(), "workstation".to_string());
        mgr.register("workstation_active", None, attrs.clone()).await.unwrap();

        attrs.insert("hostname".to_string(), "server".to_string());
        mgr.register("server_active1", None, attrs.clone()).await.unwrap();
        mgr.register("server_active2", None, attrs.clone()).await.unwrap();
        mgr.register("server_dead", None, attrs.clone()).await.unwrap();

        // Add commands
        mgr.receive_command("workstation_active", CommandRecord {
            command_id: "cmd1".into(),
            session_id: "workstation_active".into(),
            command_line: Some("ls -la".into()),
            cwd: Some("/home/user".into()),
            started_at: 1000,
            ended_at: Some(1100),
            output_summary: "".into(),
            stream_offset: 0,
            stream_length: 0,
            exit_code: Some(0),
        }).await.unwrap();

        mgr.receive_command("server_active1", CommandRecord {
            command_id: "cmd2".into(),
            session_id: "server_active1".into(),
            command_line: Some("ps aux".into()),
            cwd: Some("/var/log".into()),
            started_at: 2000,
            ended_at: Some(2100),
            output_summary: "".into(),
            stream_offset: 0,
            stream_length: 0,
            exit_code: Some(0),
        }).await.unwrap();

        mgr.receive_command("server_active2", CommandRecord {
            command_id: "cmd3".into(),
            session_id: "server_active2".into(),
            command_line: Some("docker ps".into()),
            cwd: Some("/opt/app".into()),
            started_at: 3000,
            ended_at: Some(3100),
            output_summary: "".into(),
            stream_offset: 0,
            stream_length: 0,
            exit_code: Some(0),
        }).await.unwrap();

        mgr.receive_command("server_dead", CommandRecord {
            command_id: "cmd4".into(),
            session_id: "server_dead".into(),
            command_line: Some("old command".into()),
            cwd: Some("/tmp".into()),
            started_at: 500,
            ended_at: Some(600),
            output_summary: "".into(),
            stream_offset: 0,
            stream_length: 0,
            exit_code: Some(0),
        }).await.unwrap();

        // End the dead session
        mgr.end_session("server_dead").await.unwrap();

        // Show sessions list
        let output = mgr.format_sessions_list("workstation_active").await;

        // Print the output for demonstration
        println!("=== Demo Sessions Output ===");
        println!("{}", output);
        println!("===========================");

        // Verify basic structure
        assert!(output.contains("[workstation]"));
        assert!(output.contains("[server]"));
        assert!(output.contains("* workstation_active [active]"));
        assert!(output.contains("  server_active1 [active]"));
        assert!(output.contains("  server_active2 [active]"));
        assert!(!output.contains("server_dead")); // Should not show dead sessions
        assert!(output.contains("1 dead session(s), 1 command(s)")); // Should show dead stats
    }

    #[tokio::test]
    async fn test_max_context_chars_reduces_commands() {
        use omnish_common::config::{CompletionContextConfig, ContextConfig};

        let dir = tempfile::tempdir().unwrap();

        // First, test WITHOUT limit to see how big the context would be
        let cc_no_limit = ContextConfig {
            completion: CompletionContextConfig {
                detailed_commands: 30,
                history_commands: 100,
                head_lines: 20,
                tail_lines: 20,
                max_line_width: 512,
                min_current_session_commands: 5,
                max_context_chars: None,
                detailed_min: 20,
                detailed_max: 30,
            },
        };
        let mgr_no_limit = SessionManager::new(dir.path().to_path_buf(), cc_no_limit);
        mgr_no_limit.register("sess1", None, Default::default())
            .await
            .unwrap();

        for i in 0..20 {
            mgr_no_limit.receive_command(
                "sess1",
                CommandRecord {
                    command_id: format!("cmd{}", i),
                    session_id: "sess1".into(),
                    command_line: Some(format!("command{}", i)),
                    cwd: Some("/tmp".into()),
                    started_at: 1000 + i as u64,
                    ended_at: Some(2000 + i as u64),
                    output_summary: format!("output{}", i),
                    stream_offset: 0,
                    stream_length: 0,
                    exit_code: Some(0),
                },
            )
            .await
            .unwrap();
        }

        let ctx_no_limit = mgr_no_limit.get_session_context("sess1").await.unwrap();
        eprintln!("Without limit: {} chars", ctx_no_limit.chars().count());

        // Now test WITH limit
        let cc_limited = ContextConfig {
            completion: CompletionContextConfig {
                detailed_commands: 30,
                history_commands: 100,
                head_lines: 20,
                tail_lines: 20,
                max_line_width: 512,
                min_current_session_commands: 5,
                max_context_chars: Some(200), // Small limit
                detailed_min: 20,
                detailed_max: 30,
            },
        };
        let mgr_limited = SessionManager::new(dir.path().to_path_buf(), cc_limited);
        mgr_limited.register("sess1", None, Default::default())
            .await
            .unwrap();

        for i in 0..20 {
            mgr_limited.receive_command(
                "sess1",
                CommandRecord {
                    command_id: format!("cmd{}", i),
                    session_id: "sess1".into(),
                    command_line: Some(format!("command{}", i)),
                    cwd: Some("/tmp".into()),
                    started_at: 1000 + i as u64,
                    ended_at: Some(2000 + i as u64),
                    output_summary: format!("output{}", i),
                    stream_offset: 0,
                    stream_length: 0,
                    exit_code: Some(0),
                },
            )
            .await
            .unwrap();
        }

        let ctx_limited = mgr_limited.get_session_context("sess1").await.unwrap();
        let char_count = ctx_limited.chars().count();

        eprintln!("With limit (200): {} chars", char_count);
        eprintln!("Context:\n{}", ctx_limited);

        // Context should be under the limit
        assert!(
            char_count <= 200,
            "Context {} chars should be under 200 limit",
            char_count
        );

        // With limit, should have fewer commands than without limit
        assert!(
            char_count < ctx_no_limit.chars().count(),
            "Limited context ({}) should be smaller than unlimited ({})",
            char_count,
            ctx_no_limit.chars().count()
        );
    }

    /// Regression test for the KV cache miss triggered by a newly completed
    /// command: when `recent_frozen_until` starts as `None`, the first build
    /// must freeze the cutoff so the next new command lands in `remainder`
    /// instead of sweeping into `stable_prefix` and invalidating the Anthropic
    /// prompt cache.
    #[tokio::test]
    async fn test_stable_prefix_byte_stable_after_new_command() {
        use omnish_common::config::{CompletionContextConfig, ContextConfig};

        let dir = tempfile::tempdir().unwrap();
        let cc = ContextConfig {
            completion: CompletionContextConfig {
                detailed_commands: 30,
                history_commands: 100,
                head_lines: 20,
                tail_lines: 20,
                max_line_width: 512,
                min_current_session_commands: 5,
                max_context_chars: None,
                detailed_min: 20,
                detailed_max: 30,
            },
        };
        let mgr = SessionManager::new(dir.path().to_path_buf(), cc);
        mgr.register("sess1", None, Default::default())
            .await
            .unwrap();

        for i in 0..5 {
            mgr.receive_command(
                "sess1",
                CommandRecord {
                    command_id: format!("cmd{}", i),
                    session_id: "sess1".into(),
                    command_line: Some(format!("command{}", i)),
                    cwd: Some("/tmp".into()),
                    started_at: 1000 + i as u64,
                    ended_at: Some(2000 + i as u64),
                    output_summary: format!("output{}", i),
                    stream_offset: 0,
                    stream_length: 0,
                    exit_code: Some(0),
                },
            )
            .await
            .unwrap();
        }

        let before = mgr.build_completion_sections("sess1", None).await.unwrap();
        assert!(!before.stable_prefix.is_empty(), "stable_prefix should not be empty");

        mgr.receive_command(
            "sess1",
            CommandRecord {
                command_id: "cmd_new".into(),
                session_id: "sess1".into(),
                command_line: Some("new_command".into()),
                cwd: Some("/tmp".into()),
                started_at: 9999,
                ended_at: Some(10000),
                output_summary: "new_output".into(),
                stream_offset: 0,
                stream_length: 0,
                exit_code: Some(0),
            },
        )
        .await
        .unwrap();

        let after = mgr.build_completion_sections("sess1", None).await.unwrap();

        assert_eq!(
            before.stable_prefix, after.stable_prefix,
            "stable_prefix must be byte-identical after a new command (Anthropic cache key)"
        );
        assert!(
            after.remainder.contains("new_command"),
            "new command should land in remainder, not stable_prefix, got remainder: {:?}",
            after.remainder
        );
        assert!(
            !after.stable_prefix.contains("new_command"),
            "new command must not leak into stable_prefix"
        );
    }

    #[tokio::test]
    async fn test_cleanup_expired_dirs() {
        use std::time::{Duration, SystemTime, UNIX_EPOCH};

        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let mgr = SessionManager::new(base.clone(), Default::default());

        // Create a mock session directory with old commands.json
        // SessionManager's base_dir is base/sessions (created by SessionManager::new)
        // We need to create test directory under base/sessions
        // Real session directories have format: timestamp_sessionid
        // Use a fake timestamp that's clearly old
        let session_dir = base.join("sessions").join("2020-01-01T00-00-00Z_test_session");
        std::fs::create_dir_all(&session_dir).unwrap();

        // Create commands.json with old timestamp (3 days ago)
        let old_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64 - 3 * 24 * 3600 * 1000;

        let commands = vec![CommandRecord {
            command_id: "cmd1".into(),
            session_id: "test_session".into(),
            command_line: Some("ls".into()),
            cwd: Some("/tmp".into()),
            started_at: old_timestamp,
            ended_at: Some(old_timestamp + 1000),
            output_summary: "".into(),
            stream_offset: 0,
            stream_length: 0,
            exit_code: None,
        }];

        CommandRecord::save_all(&commands, &session_dir).unwrap();

        // Note: CommandRecord::load_all only requires commands.json
        // meta.json and stream.bin are not needed for cleanup logic

        // Clean up with 48-hour threshold
        let cleaned = mgr.cleanup_expired_dirs(Duration::from_secs(48 * 3600)).await;
        assert_eq!(cleaned, 1);
        assert!(!session_dir.exists());
    }

    #[tokio::test]
    async fn test_cleanup_expired_dirs_comprehensive() {
        use std::time::{Duration, SystemTime, UNIX_EPOCH};

        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let mgr = SessionManager::new(base.clone(), Default::default());

        // Test 1: Directory with only meta.json (no commands.json) - should be cleaned up if old
        let old_meta_dir = base.join("sessions").join("2020-01-01T00-00-00Z_meta_only_session");
        std::fs::create_dir_all(&old_meta_dir).unwrap();

        // Create meta.json with old timestamp
        let old_meta = SessionMeta {
            session_id: "meta_only_session".into(),
            parent_session_id: None,
            started_at: "2020-01-01T00:00:00Z".into(),
            ended_at: None,
            attrs: Default::default(),
        };
        old_meta.save(&old_meta_dir).unwrap();

        // Test 2: Active session in memory - should NOT be cleaned up even if directory is old
        mgr.register("active_session", None, Default::default())
            .await
            .unwrap();

        // Get the actual directory name created by register()
        let loaded_sessions = mgr.sessions.read().await;
        let active_session = loaded_sessions.get("active_session").unwrap();
        let active_session_dir = active_session.dir.clone();
        drop(loaded_sessions); // Release lock

        // Manually create an old commands.json in the active session directory
        // to simulate an old directory that would normally be cleaned up
        let old_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64 - 3 * 24 * 3600 * 1000;

        let old_commands = vec![CommandRecord {
            command_id: "old_cmd".into(),
            session_id: "active_session".into(),
            command_line: Some("old_command".into()),
            cwd: Some("/tmp".into()),
            started_at: old_timestamp,
            ended_at: Some(old_timestamp + 1000),
            output_summary: "".into(),
            stream_offset: 0,
            stream_length: 0,
            exit_code: None,
        }];

        CommandRecord::save_all(&old_commands, &active_session_dir).unwrap();

        // Test 3: Recent session directory - should NOT be cleaned up
        let recent_dir = base.join("sessions").join("2026-01-01T00-00-00Z_recent_session");
        std::fs::create_dir_all(&recent_dir).unwrap();

        let recent_commands = vec![CommandRecord {
            command_id: "recent_cmd".into(),
            session_id: "recent_session".into(),
            command_line: Some("recent_command".into()),
            cwd: Some("/tmp".into()),
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64 - 3600 * 1000, // 1 hour ago
            ended_at: Some(SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64 - 3600 * 1000 + 1000),
            output_summary: "".into(),
            stream_offset: 0,
            stream_length: 0,
            exit_code: None,
        }];

        CommandRecord::save_all(&recent_commands, &recent_dir).unwrap();

        // Run cleanup with 48-hour threshold
        let cleaned = mgr.cleanup_expired_dirs(Duration::from_secs(48 * 3600)).await;

        // Should clean up: old_meta_dir (old meta.json only)
        // Should NOT clean up: active_session_dir (active in memory), recent_dir (recent commands)
        assert_eq!(cleaned, 1, "Should clean up 1 old directory (cleaned={})", cleaned);
        assert!(!old_meta_dir.exists(), "Old meta-only directory should be cleaned up");
        assert!(active_session_dir.exists(), "Active session directory should NOT be cleaned up");
        assert!(recent_dir.exists(), "Recent session directory should NOT be cleaned up");
    }

    #[tokio::test]
    async fn test_cleanup_expired_dirs_edge_cases() {
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let mgr = SessionManager::new(base.clone(), Default::default());

        // Get the actual sessions directory path
        let sessions_dir = base.join("sessions");

        // Test 1: Empty commands.json
        let empty_dir = sessions_dir.join("2020-01-01T00-00-00Z_empty_session");
        std::fs::create_dir_all(&empty_dir).unwrap();
        std::fs::write(empty_dir.join("commands.json"), "[]").unwrap();

        // Should skip (no timestamp to check)
        let cleaned = mgr.cleanup_expired_dirs(Duration::from_secs(48 * 3600)).await;
        assert_eq!(cleaned, 0);
        assert!(empty_dir.exists()); // Should still exist

        // Test 2: Corrupted commands.json
        let corrupt_dir = sessions_dir.join("2020-01-01T00-00-00Z_corrupt_session");
        std::fs::create_dir_all(&corrupt_dir).unwrap();
        std::fs::write(corrupt_dir.join("commands.json"), "not json").unwrap();

        let cleaned = mgr.cleanup_expired_dirs(Duration::from_secs(48 * 3600)).await;
        assert_eq!(cleaned, 0);
        assert!(corrupt_dir.exists()); // Should still exist (skip on error)

        // Test 3: Missing commands.json (only directory)
        let missing_dir = sessions_dir.join("2020-01-01T00-00-00Z_missing_session");
        std::fs::create_dir_all(&missing_dir).unwrap();

        let cleaned = mgr.cleanup_expired_dirs(Duration::from_secs(48 * 3600)).await;
        assert_eq!(cleaned, 0);
        assert!(missing_dir.exists()); // Should still exist
    }

    #[tokio::test]
    async fn test_load_existing_cleans_up_expired_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();

        // Create a manager to get proper session directory structure
        let mgr1 = SessionManager::new(base.clone(), Default::default());

        // Register a fresh session first
        mgr1.register("fresh_session", None, Default::default())
            .await
            .unwrap();

        let loaded_sessions = mgr1.sessions.read().await;
        let fresh_session = loaded_sessions.get("fresh_session").unwrap();
        let fresh_dir = fresh_session.dir.clone();
        drop(loaded_sessions);

        let fresh_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64 - 12 * 3600 * 1000; // 12 hours ago

        let fresh_commands = vec![CommandRecord {
            command_id: "cmd2".into(),
            session_id: "fresh_session".into(),
            command_line: Some("pwd".into()),
            cwd: Some("/tmp".into()),
            started_at: fresh_timestamp,
            ended_at: Some(fresh_timestamp + 1000),
            output_summary: "".into(),
            stream_offset: 0,
            stream_length: 0,
            exit_code: None,
        }];

        CommandRecord::save_all(&fresh_commands, &fresh_dir).unwrap();

        // Drop the first manager to release locks
        drop(mgr1);

        // Now manually create an expired session directory that load_existing cannot load
        // We'll create it with invalid/corrupt data so load_existing will fail to load it
        // but cleanup_expired_dirs will still be able to detect it's expired and delete it
        let expired_dir = base.join("sessions").join("2020-01-01T00-00-00Z_expired_session");
        std::fs::create_dir_all(&expired_dir).unwrap();

        // Create commands.json with old timestamp (3 days ago) but valid format
        use std::time::{SystemTime, UNIX_EPOCH};
        let old_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64 - 3 * 24 * 3600 * 1000;

        let commands = vec![CommandRecord {
            command_id: "cmd1".into(),
            session_id: "expired_session".into(),
            command_line: Some("ls".into()),
            cwd: Some("/tmp".into()),
            started_at: old_timestamp,
            ended_at: Some(old_timestamp + 1000),
            output_summary: "".into(),
            stream_offset: 0,
            stream_length: 0,
            exit_code: None,
        }];

        CommandRecord::save_all(&commands, &expired_dir).unwrap();

        // Create new manager and load existing - should clean up expired but keep fresh
        let mgr2 = SessionManager::new(base.clone(), Default::default());
        let count = mgr2.load_existing().await.unwrap();

        // Fresh session should be loaded, expired should be deleted
        assert_eq!(count, 1); // Only fresh session loaded
        assert!(!expired_dir.exists());
        assert!(fresh_dir.exists());
    }

    #[tokio::test]
    async fn test_list_clients_dedupes_by_deploy_addr_and_hostname() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path().to_path_buf(), Default::default());

        let mut a = HashMap::new();
        a.insert("hostname".to_string(), "box1".to_string());
        a.insert("client_addr".to_string(), "alice@box1".to_string());
        mgr.register("s1", None, a).await.unwrap();

        // Same host reached by a different user - should appear as a
        // separate menu entry because deploy target differs.
        let mut b = HashMap::new();
        b.insert("hostname".to_string(), "box1".to_string());
        b.insert("client_addr".to_string(), "bob@box1".to_string());
        mgr.register("s2", None, b).await.unwrap();

        // Legacy client with no client_addr - falls back to hostname,
        // should collapse with any other client that reports hostname
        // as its deploy target.
        let mut c = HashMap::new();
        c.insert("hostname".to_string(), "box2".to_string());
        mgr.register("s3", None, c).await.unwrap();

        let clients = mgr.list_clients().await;
        assert_eq!(clients.len(), 3);
        assert_eq!(clients[0], ("alice@box1".to_string(), "box1".to_string(), true));
        assert_eq!(clients[1], ("bob@box1".to_string(), "box1".to_string(), true));
        assert_eq!(clients[2], ("box2".to_string(), "box2".to_string(), true));
    }

    #[tokio::test]
    async fn test_list_clients_includes_persisted_history_with_inactive_flag() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();

        // First daemon run: register a client, then drop the manager
        // (simulates daemon shutdown). The session goes away from memory
        // but clients_history.json persists on disk.
        {
            let mgr = SessionManager::new(base.clone(), Default::default());
            let mut a = HashMap::new();
            a.insert("hostname".to_string(), "box1".to_string());
            a.insert("client_addr".to_string(), "alice@box1".to_string());
            mgr.register("s1", None, a).await.unwrap();
            assert!(base.join("clients.json").exists());
        }

        // Second daemon run: no in-memory sessions, but the persisted
        // history should still surface the client with active=false.
        let mgr = SessionManager::new(base.clone(), Default::default());
        let clients = mgr.list_clients().await;
        assert_eq!(clients, vec![
            ("alice@box1".to_string(), "box1".to_string(), false),
        ]);

        // Forget removes it from the menu.
        let removed = mgr.forget_client_addr("alice@box1").await;
        assert_eq!(removed, 1);
        let clients = mgr.list_clients().await;
        assert!(clients.is_empty());
    }

    #[tokio::test]
    async fn test_list_clients_overlays_active_on_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();

        // Seed history: addrA was seen previously but no live session now.
        {
            let mgr = SessionManager::new(base.clone(), Default::default());
            let mut a = HashMap::new();
            a.insert("hostname".to_string(), "boxA".to_string());
            a.insert("client_addr".to_string(), "alice@boxA".to_string());
            mgr.register("s_old", None, a).await.unwrap();
        }

        // Fresh daemon, register addrB live; list should show A inactive, B active.
        let mgr = SessionManager::new(base, Default::default());
        let mut b = HashMap::new();
        b.insert("hostname".to_string(), "boxB".to_string());
        b.insert("client_addr".to_string(), "alice@boxB".to_string());
        mgr.register("s_new", None, b).await.unwrap();

        let clients = mgr.list_clients().await;
        assert_eq!(clients.len(), 2);
        let by_addr: HashMap<&str, bool> = clients.iter()
            .map(|(a, _, active)| (a.as_str(), *active))
            .collect();
        assert!(!by_addr["alice@boxA"]);
        assert!(by_addr["alice@boxB"]);
    }
}
