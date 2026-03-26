use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

/// Per-host transfer cooldown (60 seconds).
/// Short enough to allow quick retries on failure, long enough to prevent
/// multiple clients on the same machine from hammering the daemon concurrently.
const TRANSFER_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

/// Grace period after daemon startup before responding to UpdateCheck requests.
/// Gives the daemon time to finish its own update cycle first.
const STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// Manages the package cache at ~/.omnish/updates/{os}-{arch}/
pub struct UpdateCache {
    cache_dir: PathBuf,
    /// Cached latest version per platform — refreshed by `scan_updates()`
    latest_versions: Mutex<HashMap<(String, String), String>>,
    /// Known client platforms — populated by UpdateCheck messages
    known_platforms: Mutex<HashSet<(String, String)>>,
    /// Per-host transfer lock: only one transfer per host within cooldown period
    transfer_locks: Mutex<HashMap<String, Instant>>,
    /// When the cache was created (daemon startup time)
    startup_time: Instant,
}

impl UpdateCache {
    pub fn new(omnish_dir: &Path) -> Self {
        let cache_dir = omnish_dir.join("updates");

        // Seed local hostname into transfer_locks so local clients wait
        // TRANSFER_COOLDOWN after daemon startup before downloading updates.
        let mut initial_locks = HashMap::new();
        if let Ok(hostname) = nix::unistd::gethostname() {
            if let Ok(name) = hostname.into_string() {
                initial_locks.insert(name, Instant::now());
            }
        }

        let cache = Self {
            cache_dir,
            latest_versions: Mutex::new(HashMap::new()),
            known_platforms: Mutex::new(HashSet::new()),
            transfer_locks: Mutex::new(initial_locks),
            startup_time: Instant::now(),
        };
        cache.scan_updates();
        cache
    }

    /// Whether the startup grace period has elapsed.
    pub fn past_startup_grace(&self) -> bool {
        self.startup_time.elapsed() >= STARTUP_GRACE
    }

    /// Return the directory for a given platform
    fn platform_dir(&self, os: &str, arch: &str) -> PathBuf {
        self.cache_dir.join(format!("{}-{}", os, arch))
    }

    /// Find the cached package for a platform, return (version, path) if exists.
    /// When multiple versions are cached, returns the one with the highest semver.
    pub fn cached_package(&self, os: &str, arch: &str) -> Option<(String, PathBuf)> {
        self.cached_package_best(os, arch, None)
    }

    /// Find a cached package for a platform.
    /// If `prefer_version` is set and a matching package exists, return it;
    /// otherwise return the highest version.
    fn cached_package_best(&self, os: &str, arch: &str, prefer_version: Option<&str>) -> Option<(String, PathBuf)> {
        let dir = self.platform_dir(os, arch);
        if !dir.exists() {
            return None;
        }
        let mut best: Option<(String, PathBuf)> = None;
        let mut preferred: Option<(String, PathBuf)> = None;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(version) = omnish_common::update::extract_version(&name, os, arch) {
                    if let Some(pv) = prefer_version {
                        if omnish_common::update::compare_versions(&version, pv) == std::cmp::Ordering::Equal {
                            preferred = Some((version.clone(), entry.path()));
                        }
                    }
                    let dominated = best.as_ref().is_some_and(|(v, _)| {
                        omnish_common::update::compare_versions(&version, v) != std::cmp::Ordering::Greater
                    });
                    if !dominated {
                        best = Some((version, entry.path()));
                    }
                }
            }
        }
        preferred.or(best)
    }


    /// Check if a newer version is available (uses cached scan results).
    pub fn check_update(&self, os: &str, arch: &str, current_version: &str) -> Option<String> {
        let versions = self.latest_versions.lock().unwrap();
        let cached_version = versions.get(&(os.to_string(), arch.to_string()))?;
        if omnish_common::update::compare_versions(cached_version, current_version) == std::cmp::Ordering::Greater {
            Some(cached_version.clone())
        } else {
            None
        }
    }

    /// Check if a newer version is available and return (version, checksum).
    /// Prefers a package matching the daemon's own version; falls back to latest.
    pub fn check_update_with_checksum(&self, os: &str, arch: &str, current_version: &str) -> Option<(String, String)> {
        let daemon_version = omnish_common::VERSION;
        let (version, path) = self.cached_package_best(os, arch, Some(daemon_version))?;
        if omnish_common::update::compare_versions(&version, current_version) != std::cmp::Ordering::Greater {
            return None;
        }
        let checksum = omnish_common::update::checksum(&path).unwrap_or_default();
        Some((version, checksum))
    }

    /// Record a client platform from an UpdateCheck message.
    pub fn register_platform(&self, os: &str, arch: &str) {
        self.known_platforms.lock().unwrap().insert((os.to_string(), arch.to_string()));
    }

    /// Return all known client platforms (for the auto-update task to download).
    pub fn known_platforms(&self) -> HashSet<(String, String)> {
        self.known_platforms.lock().unwrap().clone()
    }

    /// Try to acquire transfer lock for a host.
    /// Returns true if this host may proceed (no other transfer in the last 5 minutes).
    pub fn try_acquire_transfer(&self, hostname: &str) -> bool {
        let now = Instant::now();
        let mut locks = self.transfer_locks.lock().unwrap();
        if let Some(last) = locks.get(hostname) {
            if now.duration_since(*last) < TRANSFER_COOLDOWN {
                return false;
            }
        }
        locks.insert(hostname.to_string(), now);
        true
    }

    /// Scan the updates directory and refresh the latest version per platform.
    pub fn scan_updates(&self) {
        let mut versions = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(&self.cache_dir) {
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let dir_name = entry.file_name().to_string_lossy().to_string();
                // Parse "{os}-{arch}" directory name
                if let Some((os, arch)) = dir_name.split_once('-') {
                    if let Some((version, _)) = self.cached_package(os, arch) {
                        versions.insert((os.to_string(), arch.to_string()), version);
                    }
                }
            }
        }
        *self.latest_versions.lock().unwrap() = versions;
    }

    /// Cache a package file for the daemon's own platform after self-update.
    /// Copies the tar.gz from the source path into the cache directory.
    /// Keeps the latest `MAX_CACHED_PACKAGES` versions, removing older ones.
    pub fn cache_package(&self, os: &str, arch: &str, version: &str, source: &Path) -> Result<()> {
        let dir = self.platform_dir(os, arch);
        std::fs::create_dir_all(&dir)?;
        let dest = dir.join(format!("omnish-{}-{}-{}.tar.gz", version, os, arch));
        std::fs::copy(source, &dest)?;
        tracing::info!("cached update package: {}", dest.display());
        omnish_common::update::prune_packages(&dir, os, arch, omnish_common::update::MAX_CACHED_PACKAGES);
        Ok(())
    }


    /// Download a platform package from a local directory.
    /// Finds the highest version in the source, then copies it to the cache
    /// (tmp first, then atomic rename).
    pub fn download_from_local_dir(&self, source_dir: &Path, os: &str, arch: &str) -> Result<bool> {
        let suffix = format!("-{}-{}.tar.gz", os, arch);
        // Scan all matching files to find the highest version
        let mut best: Option<(String, PathBuf)> = None;
        let entries = std::fs::read_dir(source_dir)?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("omnish-") && name.ends_with(&suffix) {
                if let Some(version) = omnish_common::update::extract_version(&name, os, arch) {
                    let dominated = best.as_ref().is_some_and(|(v, _)| {
                        omnish_common::update::compare_versions(&version, v) != std::cmp::Ordering::Greater
                    });
                    if !dominated {
                        best = Some((version, entry.path()));
                    }
                }
            }
        }
        let (version, source_path) = match best {
            Some(b) => b,
            None => return Ok(false),
        };
        // Check if we already have this version cached
        if let Some((cached_ver, _)) = self.cached_package(os, arch) {
            if omnish_common::update::compare_versions(&cached_ver, &version) != std::cmp::Ordering::Less {
                return Ok(false); // Already have same or newer
            }
        }
        // Copy to tmp, then mv to target
        let filename = source_path.file_name().unwrap().to_string_lossy().to_string();
        let platform_dir = self.platform_dir(os, arch);
        std::fs::create_dir_all(&platform_dir)?;
        let tmp_path = platform_dir.join(format!(".tmp-{}", filename));
        std::fs::copy(&source_path, &tmp_path)?;
        let dest = platform_dir.join(&filename);
        std::fs::rename(&tmp_path, &dest)?;
        tracing::info!("cached update package: {}", dest.display());
        omnish_common::update::prune_packages(&platform_dir, os, arch, omnish_common::update::MAX_CACHED_PACKAGES);
        Ok(true)
    }

    /// Download platform packages from a GitHub releases API endpoint.
    ///
    /// `api_url` should be e.g. `https://api.github.com/repos/owner/repo/releases/latest`.
    /// Fetches the release JSON, finds matching assets for each (os, arch) pair,
    /// and downloads them to the cache.
    pub async fn download_from_github(
        &self,
        api_url: &str,
        platforms: &[(String, String)],
        client: &reqwest::Client,
    ) -> Vec<(String, String, anyhow::Result<bool>)> {
        let mut results = Vec::new();

        // Fetch release metadata
        let resp = match client
            .get(api_url)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "omnish-daemon")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                for (os, arch) in platforms {
                    results.push((os.clone(), arch.clone(), Err(anyhow::anyhow!("request failed: {}", e))));
                }
                return results;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            for (os, arch) in platforms {
                results.push((os.clone(), arch.clone(), Err(anyhow::anyhow!("HTTP {}", status))));
            }
            return results;
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                for (os, arch) in platforms {
                    results.push((os.clone(), arch.clone(), Err(anyhow::anyhow!("json parse: {}", e))));
                }
                return results;
            }
        };

        // Extract version from tag_name (strip leading 'v')
        let tag = body["tag_name"].as_str().unwrap_or("");
        let version = tag.strip_prefix('v').unwrap_or(tag);
        if version.is_empty() {
            for (os, arch) in platforms {
                results.push((os.clone(), arch.clone(), Err(anyhow::anyhow!("no tag_name in release"))));
            }
            return results;
        }

        let assets = match body["assets"].as_array() {
            Some(a) => a,
            None => {
                for (os, arch) in platforms {
                    results.push((os.clone(), arch.clone(), Err(anyhow::anyhow!("no assets in release"))));
                }
                return results;
            }
        };

        // Map platform names to GitHub asset OS names
        fn github_os(os: &str) -> &str {
            match os {
                "macos" => "macos",
                _ => os,
            }
        }

        for (os, arch) in platforms {
            let suffix = format!("-{}-{}.tar.gz", github_os(os), arch);
            // Find matching asset
            let asset = assets.iter().find(|a| {
                a["name"].as_str().is_some_and(|n| n.starts_with("omnish-") && n.ends_with(&suffix))
            });
            let asset = match asset {
                Some(a) => a,
                None => {
                    // No asset for this platform — not an error
                    results.push((os.clone(), arch.clone(), Ok(false)));
                    continue;
                }
            };

            let asset_name = asset["name"].as_str().unwrap_or("");
            let asset_version = match omnish_common::update::extract_version(asset_name, github_os(os), arch) {
                Some(v) => v,
                None => {
                    results.push((os.clone(), arch.clone(), Err(anyhow::anyhow!("cannot parse version from {}", asset_name))));
                    continue;
                }
            };

            // Check if we already have this version cached
            if let Some((cached_ver, _)) = self.cached_package(os, arch) {
                if omnish_common::update::compare_versions(&cached_ver, &asset_version) != std::cmp::Ordering::Less {
                    results.push((os.clone(), arch.clone(), Ok(false)));
                    continue;
                }
            }

            let download_url = match asset["browser_download_url"].as_str() {
                Some(u) => u,
                None => {
                    results.push((os.clone(), arch.clone(), Err(anyhow::anyhow!("no download URL for {}", asset_name))));
                    continue;
                }
            };

            // Download to tmp, then atomic rename
            let platform_dir = self.platform_dir(os, arch);
            if let Err(e) = std::fs::create_dir_all(&platform_dir) {
                results.push((os.clone(), arch.clone(), Err(e.into())));
                continue;
            }
            let tmp_path = platform_dir.join(format!(".tmp-{}", asset_name));

            let dl_result: anyhow::Result<bool> = async {
                let resp = client.get(download_url)
                    .header("User-Agent", "omnish-daemon")
                    .send()
                    .await?;
                if !resp.status().is_success() {
                    anyhow::bail!("download HTTP {}", resp.status());
                }
                let bytes = resp.bytes().await?;
                tokio::fs::write(&tmp_path, &bytes).await?;
                // Remove old packages
                if let Ok(old_entries) = std::fs::read_dir(&platform_dir) {
                    for old in old_entries.flatten() {
                        let old_name = old.file_name().to_string_lossy().to_string();
                        if old_name != format!(".tmp-{}", asset_name) && old_name.ends_with(".tar.gz") {
                            let _ = std::fs::remove_file(old.path());
                        }
                    }
                }
                let dest = platform_dir.join(asset_name);
                std::fs::rename(&tmp_path, &dest)?;
                tracing::info!("cached update package: {}", dest.display());
                Ok(true)
            }.await;

            if dl_result.is_err() {
                let _ = std::fs::remove_file(&tmp_path);
            }
            results.push((os.clone(), arch.clone(), dl_result));
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_version_valid() {
        assert_eq!(
            omnish_common::update::extract_version("omnish-0.5.0-linux-x86_64.tar.gz", "linux", "x86_64"),
            Some("0.5.0".to_string())
        );
    }

    #[test]
    fn extract_version_invalid() {
        assert_eq!(
            omnish_common::update::extract_version("other-file.tar.gz", "linux", "x86_64"),
            None
        );
    }

    #[test]
    fn normalize_version_strips_hash() {
        use omnish_common::update::normalize_version;
        assert_eq!(normalize_version("0.8.4-71-gdf067f6"), "0.8.4.71");
        assert_eq!(normalize_version("0.5.0"), "0.5.0");
        assert_eq!(normalize_version("0.8.4-71"), "0.8.4.71");
        assert_eq!(normalize_version("1.0.0-3-gabcdef0"), "1.0.0.3");
    }

    #[test]
    fn compare_versions_semver() {
        use std::cmp::Ordering;
        use omnish_common::update::compare_versions;
        assert_eq!(compare_versions("0.10.0", "0.9.0"), Ordering::Greater);
        assert_eq!(compare_versions("0.5.0", "0.5.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.0.0", "0.99.99"), Ordering::Greater);
        assert_eq!(compare_versions("0.4.0", "0.5.0"), Ordering::Less);
        // git describe versions
        assert_eq!(compare_versions("0.8.4-71-gdf067f6", "0.8.4"), Ordering::Greater);
        assert_eq!(compare_versions("0.8.4-71-gdf067f6", "0.8.4-72-gabcdef0"), Ordering::Less);
        assert_eq!(compare_versions("0.8.4-71-gdf067f6", "0.8.5"), Ordering::Less);
    }

    #[test]
    fn check_update_newer() {
        let dir = tempfile::tempdir().unwrap();

        // Create a fake cached package before constructing cache (scan runs in new())
        let platform_dir = dir.path().join("updates/linux-x86_64");
        std::fs::create_dir_all(&platform_dir).unwrap();
        std::fs::write(platform_dir.join("omnish-0.5.0-linux-x86_64.tar.gz"), b"fake").unwrap();

        let cache = UpdateCache::new(dir.path());
        assert_eq!(cache.check_update("linux", "x86_64", "0.4.0"), Some("0.5.0".to_string()));
        assert_eq!(cache.check_update("linux", "x86_64", "0.5.0"), None);
        assert_eq!(cache.check_update("linux", "x86_64", "0.6.0"), None);
    }

    #[test]
    fn checksum_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        std::fs::write(&path, b"hello world").unwrap();
        let sum = omnish_common::update::checksum(&path).unwrap();
        // SHA-256 of "hello world"
        assert_eq!(sum, "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9");
    }
}
