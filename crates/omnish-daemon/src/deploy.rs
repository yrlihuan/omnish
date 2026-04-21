//! Client deployment via the bundled `deploy.sh` script.
//!
//! Spawns `bash $OMNISH_DIR/deploy.sh <user@host>` in the background and
//! pushes a NoticePush back to all connected clients reporting the result.

use omnish_protocol::message::{Message, NoticeLevel};
use omnish_transport::rpc_server::PushRegistry;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Upper bound on a deploy run. scp of the binaries over a slow link can take
/// tens of seconds; 2 minutes leaves headroom without letting a stuck task leak.
const DEPLOY_TIMEOUT: Duration = Duration::from_secs(120);

/// Max stderr lines to surface in the failure notice.
const STDERR_LINE_LIMIT: usize = 5;

/// Marker line deploy.sh emits on stderr to signal a partial-success condition.
/// Format: `OMNISH_DEPLOY_STATUS: <kind> <target>`.
const STATUS_MARKER: &str = "OMNISH_DEPLOY_STATUS:";

/// Outcome of a single `deploy.sh` run.
enum DeployOutcome {
    /// Binaries copied and client.toml written (or already present, or no addr
    /// configured). Nothing for the user to do.
    Ok,
    /// Binaries copied but the remote could not reach any daemon candidate, so
    /// no client.toml was written.
    ProbeFailed,
    /// Binaries copied but daemon uses a Unix socket; a remote client cannot
    /// use it, so no client.toml was written.
    UnixSocket,
    /// Deploy itself failed (scp error, missing script, ssh failure, etc.).
    Failed(String),
}

/// Validate an ssh target. Accepts either `host` (user defaults come from the
/// caller's ssh config / $USER) or `user@host`. Rejects empty segments and
/// shell metacharacters so the string is safe to pass as a single argv word.
pub fn parse_target(target: &str) -> Option<String> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    let bad = |s: &str| s.chars().any(|c| {
        c.is_whitespace() || matches!(c, '\'' | '"' | '`' | '$' | '\\' | ';' | '|' | '&' | '<' | '>' | '(' | ')')
    });
    match target.split_once('@') {
        Some((user, host)) => {
            if user.is_empty() || host.is_empty() { return None; }
            if bad(user) || bad(host) { return None; }
        }
        None => {
            if bad(target) { return None; }
        }
    }
    Some(target.to_string())
}

/// Spawn `deploy.sh` for the given target. Returns immediately; the result is
/// delivered as a NoticePush broadcast through `push_registry` when the
/// script exits.
///
/// `listen_addr` is the daemon's configured listen address (forwarded to
/// deploy.sh as `OMNISH_DAEMON_ADDR`). The script uses it to generate a
/// minimal `client.toml` on first deploy when the remote has none.
pub fn spawn_deploy(
    omnish_dir: PathBuf,
    target: String,
    listen_addr: String,
    push_registry: PushRegistry,
) {
    tokio::spawn(async move {
        let outcome = match tokio::time::timeout(
            DEPLOY_TIMEOUT,
            run_deploy(&omnish_dir, &target, &listen_addr),
        ).await {
            Ok(inner) => inner,
            Err(_) => DeployOutcome::Failed(format!("timed out after {}s", DEPLOY_TIMEOUT.as_secs())),
        };
        let (level, text) = match outcome {
            DeployOutcome::Ok => (NoticeLevel::Info, format!("Deployed to {}", target)),
            DeployOutcome::ProbeFailed => (
                NoticeLevel::Info,
                format!(
                    "Deployed to {}, but could not reach daemon from remote. \
                     Edit ~/.omnish/client.toml on {} and set daemon_addr manually",
                    target, target,
                ),
            ),
            DeployOutcome::UnixSocket => (
                NoticeLevel::Info,
                format!(
                    "Deployed to {}, but daemon uses a Unix socket. \
                     Configure daemon_addr manually",
                    target,
                ),
            ),
            DeployOutcome::Failed(err) => (
                NoticeLevel::Error,
                format!("Deploy to {} failed: {}", target, err),
            ),
        };
        broadcast_notice(&push_registry, level, text).await;
    });
}

async fn run_deploy(omnish_dir: &Path, target: &str, listen_addr: &str) -> DeployOutcome {
    let script = omnish_dir.join("deploy.sh");
    if !script.exists() {
        return DeployOutcome::Failed(format!("{} not found", script.display()));
    }

    // kill_on_drop ensures the child is terminated if the outer future is
    // cancelled (e.g. by the deploy timeout).
    let mut child = match Command::new("bash")
        .arg(&script)
        .arg(target)
        .env("OMNISH_HOME", omnish_dir)
        .env("OMNISH_DAEMON_ADDR", listen_addr)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return DeployOutcome::Failed(format!("spawn bash: {}", e)),
    };

    let stderr = child.stderr.take().expect("piped stderr");
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        let mut lines: Vec<String> = Vec::new();
        let mut status_kind: Option<String> = None;
        while let Ok(Some(line)) = reader.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }
            if let Some(rest) = trimmed.strip_prefix(STATUS_MARKER) {
                // First token after the marker is the status kind.
                let kind = rest.trim().split_whitespace().next().unwrap_or("").to_string();
                if !kind.is_empty() && status_kind.is_none() {
                    status_kind = Some(kind);
                }
                continue;
            }
            if lines.len() < STDERR_LINE_LIMIT {
                lines.push(trimmed.to_string());
            }
        }
        (lines.join(" | "), status_kind)
    });

    // Drain stdout so the pipe doesn't block the script.
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while reader.next_line().await.ok().flatten().is_some() {}
        });
    }

    let status = match child.wait().await {
        Ok(s) => s,
        Err(e) => return DeployOutcome::Failed(format!("wait: {}", e)),
    };
    let (stderr_msg, status_kind) = stderr_task.await.unwrap_or_default();

    if !status.success() {
        let msg = match status_kind.as_deref() {
            Some("connect_failed") => "could not connect".to_string(),
            Some("scp_failed") => "scp failed".to_string(),
            _ if !stderr_msg.is_empty() => stderr_msg,
            _ => format!("exit status {}", status),
        };
        return DeployOutcome::Failed(msg);
    }
    match status_kind.as_deref() {
        Some("probe_failed") => DeployOutcome::ProbeFailed,
        Some("unix_socket") => DeployOutcome::UnixSocket,
        _ => DeployOutcome::Ok,
    }
}

async fn broadcast_notice(registry: &PushRegistry, level: NoticeLevel, text: String) {
    let senders: Vec<_> = {
        let map = registry.lock().await;
        map.values().cloned().collect()
    };
    for tx in senders {
        let msg = Message::NoticePush { level: level.clone(), text: text.clone() };
        let _ = tx.send(msg).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_target_valid() {
        assert_eq!(parse_target("alice@host1"), Some("alice@host1".into()));
        assert_eq!(parse_target("  bob@server.local  "), Some("bob@server.local".into()));
        assert_eq!(parse_target("u@1.2.3.4"), Some("u@1.2.3.4".into()));
        // Host-only: ssh defaults user from config / $USER.
        assert_eq!(parse_target("host1"), Some("host1".into()));
        assert_eq!(parse_target("  server.local  "), Some("server.local".into()));
    }

    #[test]
    fn parse_target_invalid() {
        assert_eq!(parse_target(""), None);
        assert_eq!(parse_target("@host"), None);
        assert_eq!(parse_target("user@"), None);
        assert_eq!(parse_target("user@host;rm -rf /"), None);
        assert_eq!(parse_target("u s@host"), None);
        assert_eq!(parse_target("user@ho st"), None);
        assert_eq!(parse_target("user@$(evil)"), None);
        assert_eq!(parse_target("host;rm"), None);
        assert_eq!(parse_target("ho st"), None);
    }
}
