# Protocol-Channel Binary Distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Distribute updated binaries from daemon to clients over the existing protocol channel, replacing SSH-based deploy.

**Architecture:** Client periodically sends `UpdateCheck` to daemon; daemon replies with version info; client requests download; daemon streams the release package in chunks; client extracts binary and writes to disk; existing mtime polling triggers `execvp()`.

**Tech Stack:** Rust, bincode (serde), tokio, SHA-256 (sha2 crate)

**Spec:** `docs/superpowers/specs/2026-03-25-protocol-update-distribution-design.md`

---

### Task 1: Add protocol message types

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs`
- Modify: `crates/omnish-protocol/Cargo.toml` (if sha2 needed here — likely not, checksum is just a String)

- [ ] **Step 1: Add 4 new message variants to the `Message` enum (line 44)**

After `ConfigUpdateResult` (line 75), add:

```rust
    UpdateCheck {
        os: String,
        arch: String,
        current_version: String,
    },
    UpdateInfo {
        latest_version: String,
        available: bool,
    },
    UpdateRequest {
        os: String,
        arch: String,
        version: String,
    },
    UpdateChunk {
        seq: u32,
        total_size: u64,
        checksum: String,
        data: Vec<u8>,
        done: bool,
        error: Option<String>,
    },
```

- [ ] **Step 2: Bump PROTOCOL_VERSION (line 8)**

```rust
pub const PROTOCOL_VERSION: u32 = 10;
```

- [ ] **Step 3: Update the variant guard test**

In the `message_variant_guard` test (~line 580), update `EXPECTED_VARIANT_COUNT` from 28 to 32. Add the 4 new variants to the `variants` vec:

```rust
Message::UpdateCheck { os: "linux".into(), arch: "x86_64".into(), current_version: "0.1.0".into() },
Message::UpdateInfo { latest_version: "0.2.0".into(), available: true },
Message::UpdateRequest { os: "linux".into(), arch: "x86_64".into(), version: "0.2.0".into() },
Message::UpdateChunk { seq: 0, total_size: 1024, checksum: "abc".into(), data: vec![1,2,3], done: false, error: None },
```

Also add them to the match arms in the guard.

- [ ] **Step 4: Run tests**

Run: `cargo test -p omnish-protocol --release`
Expected: all tests pass, including round-trip serialization of new variants.

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-protocol/
git commit -m "feat(protocol): add UpdateCheck/UpdateInfo/UpdateRequest/UpdateChunk messages (#346)"
```

---

### Task 2: Add package cache module to daemon

**Files:**
- Create: `crates/omnish-daemon/src/update_cache.rs`
- Modify: `crates/omnish-daemon/src/lib.rs` (add `pub mod update_cache;`)
- Modify: `crates/omnish-daemon/Cargo.toml` (add `sha2` dependency)

This module manages the `~/.omnish/updates/` directory: caching packages, checking versions, downloading on-demand, and streaming chunks.

- [ ] **Step 1: Add sha2 dependency**

In `crates/omnish-daemon/Cargo.toml`, add:
```toml
sha2 = "0.10"
```

- [ ] **Step 2: Create update_cache.rs with UpdateCache struct**

```rust
use anyhow::Result;
use sha2::{Sha256, Digest};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Manages the package cache at ~/.omnish/updates/{os}-{arch}/
pub struct UpdateCache {
    cache_dir: PathBuf,
    /// Tracks in-flight background downloads to deduplicate.
    /// Uses Arc so tokio tasks can hold a reference.
    downloading: Arc<Mutex<HashSet<(String, String)>>>,
}

impl UpdateCache {
    pub fn new(omnish_dir: &Path) -> Self {
        let cache_dir = omnish_dir.join("updates");
        Self {
            cache_dir,
            downloading: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Return the directory for a given platform
    fn platform_dir(&self, os: &str, arch: &str) -> PathBuf {
        self.cache_dir.join(format!("{}-{}", os, arch))
    }

    /// Find the cached package for a platform, return (version, path) if exists.
    /// When multiple versions are cached, returns the one with the highest semver.
    pub fn cached_package(&self, os: &str, arch: &str) -> Option<(String, PathBuf)> {
        let dir = self.platform_dir(os, arch);
        if !dir.exists() {
            return None;
        }
        let mut best: Option<(String, PathBuf)> = None;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(version) = Self::extract_version(&name, os, arch) {
                    let dominated = best.as_ref().map_or(false, |(v, _)| {
                        Self::compare_versions(&version, v) != std::cmp::Ordering::Greater
                    });
                    if !dominated {
                        best = Some((version, entry.path()));
                    }
                }
            }
        }
        best
    }

    /// Extract version from filename: omnish-{version}-{os}-{arch}.tar.gz
    fn extract_version(filename: &str, os: &str, arch: &str) -> Option<String> {
        let suffix = format!("-{}-{}.tar.gz", os, arch);
        let prefix = "omnish-";
        if filename.starts_with(prefix) && filename.ends_with(&suffix) {
            let version = &filename[prefix.len()..filename.len() - suffix.len()];
            if !version.is_empty() {
                return Some(version.to_string());
            }
        }
        None
    }

    /// Compare two version strings using semver-style numeric comparison.
    /// Falls back to lexicographic comparison for non-numeric components.
    fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
        let a_parts: Vec<&str> = a.split('.').collect();
        let b_parts: Vec<&str> = b.split('.').collect();
        for (ap, bp) in a_parts.iter().zip(b_parts.iter()) {
            match (ap.parse::<u64>(), bp.parse::<u64>()) {
                (Ok(an), Ok(bn)) => match an.cmp(&bn) {
                    std::cmp::Ordering::Equal => continue,
                    ord => return ord,
                },
                _ => match ap.cmp(bp) {
                    std::cmp::Ordering::Equal => continue,
                    ord => return ord,
                },
            }
        }
        a_parts.len().cmp(&b_parts.len())
    }

    /// Check if the cached version is newer than the client's version
    pub fn check_update(&self, os: &str, arch: &str, current_version: &str) -> Option<String> {
        let (cached_version, _) = self.cached_package(os, arch)?;
        if Self::compare_versions(&cached_version, current_version) == std::cmp::Ordering::Greater {
            Some(cached_version)
        } else {
            None
        }
    }

    /// Cache a package file for the daemon's own platform after self-update.
    /// Copies the tar.gz from the source path into the cache directory.
    pub fn cache_package(&self, os: &str, arch: &str, version: &str, source: &Path) -> Result<()> {
        let dir = self.platform_dir(os, arch);
        std::fs::create_dir_all(&dir)?;
        // Remove old packages for this platform
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        let dest = dir.join(format!("omnish-{}-{}-{}.tar.gz", version, os, arch));
        std::fs::copy(source, &dest)?;
        tracing::info!("cached update package: {}", dest.display());
        Ok(())
    }

    /// Compute SHA-256 checksum of a file
    pub fn checksum(path: &Path) -> Result<String> {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 65536];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 { break; }
            hasher.update(&buf[..n]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    /// Start a background download for a platform if not already in progress.
    /// Returns true if a download was kicked off, false if already in progress.
    pub fn start_background_download(
        &self,
        os: String,
        arch: String,
        check_url: Option<String>,
        omnish_dir: PathBuf,
    ) -> bool {
        let key = (os.clone(), arch.clone());
        {
            let mut dl = self.downloading.lock().unwrap();
            if dl.contains(&key) {
                return false;
            }
            dl.insert(key.clone());
        }

        let downloading = Arc::clone(&self.downloading);
        let cache_dir = self.cache_dir.clone();
        tokio::spawn(async move {
            let result = Self::download_package(&omnish_dir, &cache_dir, &os, &arch, check_url.as_deref()).await;
            match result {
                Ok(()) => tracing::info!("background download complete: {}-{}", os, arch),
                Err(e) => tracing::warn!("background download failed for {}-{}: {}", os, arch, e),
            }
            downloading.lock().unwrap().remove(&key);
        });
        true
    }

    async fn download_package(
        omnish_dir: &Path,
        cache_dir: &Path,
        os: &str,
        arch: &str,
        check_url: Option<&str>,
    ) -> Result<()> {
        // Use install.sh with --dir (local) or fetch from GitHub releases
        // For cross-platform: construct URL like
        //   https://github.com/{repo}/releases/download/{version}/omnish-{version}-{os}-{arch}.tar.gz
        // For local dir: look for omnish-*-{os}-{arch}.tar.gz
        //
        // First, determine latest version from check_url or GitHub API
        // Then download the tar.gz for the target platform
        // Save to cache_dir/{os}-{arch}/omnish-{version}-{os}-{arch}.tar.gz
        //
        // Placeholder — actual implementation depends on release infrastructure
        anyhow::bail!("cross-platform download not yet implemented")
    }

    /// Check if a platform download is in progress
    pub fn is_downloading(&self, os: &str, arch: &str) -> bool {
        let dl = self.downloading.lock().unwrap();
        dl.contains(&(os.to_string(), arch.to_string()))
    }
}
```

- [ ] **Step 4: Add `pub mod update_cache;` to lib.rs**

In `crates/omnish-daemon/src/lib.rs`, add:
```rust
pub mod update_cache;
```

- [ ] **Step 5: Add unit tests for version extraction and caching**

In `update_cache.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_version_valid() {
        assert_eq!(
            UpdateCache::extract_version("omnish-0.5.0-linux-x86_64.tar.gz", "linux", "x86_64"),
            Some("0.5.0".to_string())
        );
    }

    #[test]
    fn extract_version_invalid() {
        assert_eq!(
            UpdateCache::extract_version("other-file.tar.gz", "linux", "x86_64"),
            None
        );
    }

    #[test]
    fn check_update_newer() {
        let dir = tempfile::tempdir().unwrap();
        let cache = UpdateCache::new(dir.path());

        // Create a fake cached package
        let platform_dir = dir.path().join("updates/linux-x86_64");
        std::fs::create_dir_all(&platform_dir).unwrap();
        std::fs::write(platform_dir.join("omnish-0.5.0-linux-x86_64.tar.gz"), b"fake").unwrap();

        assert_eq!(cache.check_update("linux", "x86_64", "0.4.0"), Some("0.5.0".to_string()));
        assert_eq!(cache.check_update("linux", "x86_64", "0.5.0"), None);
        assert_eq!(cache.check_update("linux", "x86_64", "0.6.0"), None);
    }

    #[test]
    fn compare_versions_semver() {
        use std::cmp::Ordering;
        assert_eq!(UpdateCache::compare_versions("0.10.0", "0.9.0"), Ordering::Greater);
        assert_eq!(UpdateCache::compare_versions("0.5.0", "0.5.0"), Ordering::Equal);
        assert_eq!(UpdateCache::compare_versions("1.0.0", "0.99.99"), Ordering::Greater);
        assert_eq!(UpdateCache::compare_versions("0.4.0", "0.5.0"), Ordering::Less);
    }

    #[test]
    fn checksum_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        std::fs::write(&path, b"hello world").unwrap();
        let sum = UpdateCache::checksum(&path).unwrap();
        // SHA-256 of "hello world"
        assert_eq!(sum, "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9");
    }
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p omnish-daemon --release -- update_cache`
Expected: all 5 tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-daemon/src/update_cache.rs crates/omnish-daemon/src/lib.rs crates/omnish-daemon/Cargo.toml
git commit -m "feat(daemon): add UpdateCache for package caching and version checking (#346)"
```

---

### Task 3: Handle UpdateCheck and UpdateRequest on the daemon server

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs` (~line 403, handle_message match)
- Modify: `crates/omnish-daemon/src/main.rs` (pass UpdateCache to DaemonServer)
- Modify: `crates/omnish-daemon/src/server.rs` (DaemonServer struct + ServerOpts, ~line 124)

- [ ] **Step 1: Add `update_cache` field to DaemonServer**

In `server.rs`, add `update_cache: Arc<UpdateCache>` to the `DaemonServer` struct (line ~124) and update `DaemonServer::new()` to accept it.

- [ ] **Step 2: Pass UpdateCache through handle_message**

Add `update_cache: &Arc<UpdateCache>` and `check_url: &Option<String>` parameters to `handle_message()` (line ~381). Pass them through from the server dispatch closure.

- [ ] **Step 3: Add UpdateCheck handler in the match (line ~403)**

```rust
Message::UpdateCheck { os, arch, current_version } => {
    let reply = if let Some(latest) = update_cache.check_update(&os, &arch, &current_version) {
        Message::UpdateInfo { latest_version: latest, available: true }
    } else {
        // Kick off background download if not already downloading
        if !update_cache.is_downloading(&os, &arch) {
            update_cache.start_background_download(
                os, arch,
                check_url.clone(),
                opts.config_path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            );
        }
        Message::UpdateInfo { latest_version: String::new(), available: false }
    };
    let _ = tx.send(reply).await;
}
```

- [ ] **Step 4: Add UpdateRequest handler (streaming)**

**Important:** The RPC server only sends the `Ack` end-of-stream sentinel when the handler sends 2+ messages (`count > 1` in `rpc_server.rs:327`). Since the client uses `call_stream()`, error paths must also send 2+ messages to ensure the client's stream terminates. We use a helper `send_update_error` macro that sends an error chunk followed by a done chunk.

```rust
Message::UpdateRequest { os, arch, version } => {
    // Helper: send error + done (2 messages) so call_stream Ack sentinel is sent
    macro_rules! send_update_error {
        ($tx:expr, $seq:expr, $msg:expr) => {{
            let _ = $tx.send(Message::UpdateChunk {
                seq: $seq, total_size: 0, checksum: String::new(),
                data: vec![], done: false,
                error: Some($msg),
            }).await;
            let _ = $tx.send(Message::UpdateChunk {
                seq: $seq + 1, total_size: 0, checksum: String::new(),
                data: vec![], done: true, error: None,
            }).await;
        }};
    }

    let cached = update_cache.cached_package(&os, &arch);
    match cached {
        Some((cached_ver, path)) if cached_ver == version => {
            match std::fs::File::open(&path) {
                Ok(mut file) => {
                    use std::io::Read;
                    let total_size = file.metadata().map(|m| m.len()).unwrap_or(0);
                    let checksum = match UpdateCache::checksum(&path) {
                        Ok(c) => c,
                        Err(e) => {
                            send_update_error!(tx, 0, format!("checksum error: {}", e));
                            return;
                        }
                    };
                    let mut seq = 0u32;
                    let mut buf = vec![0u8; 65536];
                    loop {
                        let n = match file.read(&mut buf) {
                            Ok(n) => n,
                            Err(e) => {
                                send_update_error!(tx, seq, format!("read error: {}", e));
                                return;
                            }
                        };
                        let done = n == 0;
                        let chunk = Message::UpdateChunk {
                            seq,
                            total_size: if seq == 0 { total_size } else { 0 },
                            checksum: if seq == 0 { checksum.clone() } else { String::new() },
                            data: if done { vec![] } else { buf[..n].to_vec() },
                            done,
                            error: None,
                        };
                        let _ = tx.send(chunk).await;
                        if done { break; }
                        seq += 1;
                    }
                }
                Err(e) => {
                    send_update_error!(tx, 0, format!("open error: {}", e));
                }
            }
        }
        _ => {
            send_update_error!(tx, 0, "not available or version mismatch".into());
        }
    }
}
```

- [ ] **Step 5: Initialize UpdateCache in daemon main.rs**

In `crates/omnish-daemon/src/main.rs`, before creating DaemonServer (~line 335):

```rust
let update_cache = Arc::new(omnish_daemon::update_cache::UpdateCache::new(&omnish_dir));
```

Pass it to `DaemonServer::new(...)`.

- [ ] **Step 6: Build and verify**

Run: `cargo build -p omnish-daemon --release`
Expected: compiles without errors.

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-daemon/src/server.rs crates/omnish-daemon/src/main.rs
git commit -m "feat(daemon): handle UpdateCheck and UpdateRequest messages (#346)"
```

---

### Task 4: Cache own platform package after self-update

**Files:**
- Modify: `crates/omnish-daemon/src/auto_update.rs`

After `install.sh --upgrade` succeeds (line ~64), before the deploy phase, cache the downloaded package for the daemon's own platform.

- [ ] **Step 1: Pass UpdateCache to auto_update job**

Update `create_auto_update_job` signature to accept `update_cache: Arc<UpdateCache>`.

- [ ] **Step 2: After successful install.sh, cache own platform package**

After line 64 (successful install), before deploy phase (line 72):

```rust
// Cache own platform package for protocol-channel distribution
{
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    // Look for the downloaded tar.gz in the check_url directory or /tmp
    // install.sh extracts to $TMPDIR, but also the --dir source has the tar.gz
    if let Some(ref url) = check_url {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            // Local directory — find the tar.gz
            let dir = std::path::Path::new(url);
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.ends_with(&format!("-{}-{}.tar.gz", os, arch)) {
                        if let Err(e) = update_cache.cache_package(
                            os, arch,
                            omnish_common::VERSION,
                            &entry.path(),
                        ) {
                            tracing::warn!("failed to cache own platform package: {}", e);
                        }
                        break;
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 3: Update caller in main.rs**

Pass `Arc::clone(&update_cache)` to `create_auto_update_job()`.

- [ ] **Step 4: Build and verify**

Run: `cargo build -p omnish-daemon --release`
Expected: compiles without errors.

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/auto_update.rs crates/omnish-daemon/src/main.rs
git commit -m "feat(daemon): cache own platform package after self-update (#346)"
```

---

### Task 5: Client-side update polling and download

**Files:**
- Modify: `crates/omnish-client/src/main.rs`
- Modify: `crates/omnish-client/Cargo.toml` (add `sha2`, `flate2`, `tar` dependencies)

- [ ] **Step 1: Add dependencies**

In `crates/omnish-client/Cargo.toml`:
```toml
sha2 = "0.10"
flate2 = "1"
tar = "0.4"
```

- [ ] **Step 2: Add update polling constants and state**

Near the existing auto-update constants (~line 546):

```rust
const UPDATE_POLL_INTERVAL: Duration = Duration::from_secs(60);
let mut last_update_poll = Instant::now();
let update_in_progress = Arc::new(AtomicBool::new(false));
```

- [ ] **Step 3: Add UpdateCheck polling in the main loop**

After the existing mtime check block (~line 613), add:

```rust
// Protocol-based update polling
if auto_update_enabled.load(Ordering::Relaxed)
    && last_update_poll.elapsed() >= UPDATE_POLL_INTERVAL
    && !update_in_progress.load(Ordering::Relaxed)
{
    last_update_poll = Instant::now();
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let ver = omnish_common::VERSION.to_string();
    if let Some(ref rpc) = rpc_client {
        match rpc.call(Message::UpdateCheck {
            os: os.clone(), arch: arch.clone(), current_version: ver,
        }).await {
            Ok(Message::UpdateInfo { latest_version, available: true }) => {
                // Start background download
                update_in_progress.store(true, Ordering::Relaxed);
                let rpc = rpc.clone();
                let uip = Arc::clone(&update_in_progress);
                tokio::spawn(async move {
                    if let Err(e) = download_and_extract_update(
                        &rpc, &os, &arch, &latest_version,
                    ).await {
                        tracing::warn!("update download failed: {}", e);
                    }
                    uip.store(false, Ordering::Relaxed);
                });
            }
            _ => {} // No update or error, ignore
        }
    }
}
```

The client main loop is `async fn main()` under `#[tokio::main]` and already `.await`s on `rpc.call()` for completions and chat. `UpdateCheck` is a fast single round-trip (same pattern as CompletionRequest) done inline. The `UpdateRequest` + chunk streaming runs in a `tokio::spawn` background task.

- [ ] **Step 4: Implement download_and_extract_update function**

```rust
async fn download_and_extract_update(
    rpc: &RpcClient,
    os: &str,
    arch: &str,
    version: &str,
) -> anyhow::Result<()> {
    let mut rx = rpc.call_stream(Message::UpdateRequest {
        os: os.to_string(),
        arch: arch.to_string(),
        version: version.to_string(),
    }).await?;

    let omnish_dir = omnish_common::config::omnish_dir();
    let tmp_dir = omnish_dir.join("tmp");
    std::fs::create_dir_all(&tmp_dir)?;
    let tmp_file = tmp_dir.join(format!("update-{}.tar.gz", version));
    let mut file = std::fs::File::create(&tmp_file)?;

    let mut expected_checksum = String::new();
    let mut total_size = 0u64;

    use std::io::Write;
    while let Some(msg) = rx.recv().await {
        match msg {
            Message::UpdateChunk { seq, total_size: ts, checksum, data, done, error } => {
                if let Some(err) = error {
                    let _ = std::fs::remove_file(&tmp_file);
                    anyhow::bail!("server error: {}", err);
                }
                if seq == 0 {
                    expected_checksum = checksum;
                    total_size = ts;
                }
                if !data.is_empty() {
                    file.write_all(&data)?;
                }
                if done {
                    break;
                }
            }
            _ => break,
        }
    }
    drop(file);

    // Verify checksum
    if !expected_checksum.is_empty() {
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        let mut f = std::fs::File::open(&tmp_file)?;
        use std::io::Read;
        let mut buf = [0u8; 65536];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 { break; }
            hasher.update(&buf[..n]);
        }
        let actual = format!("{:x}", hasher.finalize());
        if actual != expected_checksum {
            let _ = std::fs::remove_file(&tmp_file);
            anyhow::bail!("checksum mismatch: expected={}, actual={}", expected_checksum, actual);
        }
    }

    // Extract and replace binary (blocking file I/O — use spawn_blocking)
    let tmp_file_clone = tmp_file.clone();
    tokio::task::spawn_blocking(move || extract_and_install(&tmp_file_clone))
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {}", e))??;
    let _ = std::fs::remove_file(&tmp_file);
    tracing::info!("update {} installed, mtime polling will trigger restart", version);
    Ok(())
}

fn extract_and_install(tar_gz_path: &Path) -> anyhow::Result<()> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let file = std::fs::File::open(tar_gz_path)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    let current_exe = std::env::current_exe()?;
    // Handle " (deleted)" suffix
    let exe_path = {
        let s = current_exe.to_string_lossy().to_string();
        s.strip_suffix(" (deleted)")
            .map(PathBuf::from)
            .unwrap_or(current_exe)
    };
    let exe_name = exe_path.file_name()
        .ok_or_else(|| anyhow::anyhow!("cannot determine binary name"))?
        .to_string_lossy();

    let tmp_bin = exe_path.with_extension("new");
    let mut found = false;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        // Look for the client binary in the archive (e.g., omnish-0.5.0/bin/omnish)
        if let Some(name) = path.file_name() {
            if name.to_string_lossy() == *exe_name {
                entry.unpack(&tmp_bin)?;
                found = true;
                break;
            }
        }
    }

    if !found {
        anyhow::bail!("client binary '{}' not found in archive", exe_name);
    }

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_bin, std::fs::Permissions::from_mode(0o755))?;
    }

    // Atomic rename
    std::fs::rename(&tmp_bin, &exe_path)?;

    Ok(())
}
```

- [ ] **Step 5: Build and verify**

Run: `cargo build -p omnish-client --release`
Expected: compiles without errors.

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-client/
git commit -m "feat(client): add protocol-based update polling and download (#346)"
```

---

### Task 6: Wire everything together and final build

**Files:**
- Verify all modified files compile together

- [ ] **Step 1: Full workspace build**

Run: `cargo build --release`
Expected: clean build, no errors.

- [ ] **Step 2: Run all tests**

Run: `cargo test --release`
Expected: all tests pass.

- [ ] **Step 3: Final commit (if any fixups needed)**

```bash
git commit -m "fix: wire up protocol update distribution (#346)"
```
