use anyhow::Result;
use sha2::{Sha256, Digest};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

/// Per-platform transfer cooldown (5 minutes).
const TRANSFER_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(300);

/// Manages the package cache at ~/.omnish/updates/{os}-{arch}/
pub struct UpdateCache {
    cache_dir: PathBuf,
    /// Cached latest version per platform — refreshed by `scan_updates()`
    latest_versions: Mutex<HashMap<(String, String), String>>,
    /// Known client platforms — populated by UpdateCheck messages
    known_platforms: Mutex<HashSet<(String, String)>>,
    /// Per-host transfer lock: only one transfer per host within cooldown period
    transfer_locks: Mutex<HashMap<String, Instant>>,
}

impl UpdateCache {
    pub fn new(omnish_dir: &Path) -> Self {
        let cache_dir = omnish_dir.join("updates");
        let cache = Self {
            cache_dir,
            latest_versions: Mutex::new(HashMap::new()),
            known_platforms: Mutex::new(HashSet::new()),
            transfer_locks: Mutex::new(HashMap::new()),
        };
        cache.scan_updates();
        cache
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
                    let dominated = best.as_ref().is_some_and(|(v, _)| {
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

    /// Normalize a version string for comparison.
    /// Strips git commit hash suffix (e.g. "0.8.4-71-gdf067f6" → "0.8.4.71")
    /// and replaces '-' with '.' so all components are dot-separated.
    fn normalize_version(v: &str) -> String {
        // Strip "-g<hex>" suffix from git describe output
        let v = if let Some(pos) = v.rfind("-g") {
            let suffix = &v[pos + 2..];
            if suffix.chars().all(|c| c.is_ascii_hexdigit()) {
                &v[..pos]
            } else {
                v
            }
        } else {
            v
        };
        v.replace('-', ".")
    }

    /// Compare two version strings using numeric tuple comparison.
    /// Normalizes first (strips git hash, replaces '-' with '.').
    fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
        let a_norm = Self::normalize_version(a);
        let b_norm = Self::normalize_version(b);
        let a_parts: Vec<&str> = a_norm.split('.').collect();
        let b_parts: Vec<&str> = b_norm.split('.').collect();
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

    /// Check if a newer version is available (uses cached scan results).
    pub fn check_update(&self, os: &str, arch: &str, current_version: &str) -> Option<String> {
        let versions = self.latest_versions.lock().unwrap();
        let cached_version = versions.get(&(os.to_string(), arch.to_string()))?;
        if Self::compare_versions(cached_version, current_version) == std::cmp::Ordering::Greater {
            Some(cached_version.clone())
        } else {
            None
        }
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

    /// Download a platform package from a local directory.
    /// Downloads to a temp file first, then atomically moves to the cache.
    pub fn download_from_local_dir(&self, source_dir: &Path, os: &str, arch: &str) -> Result<bool> {
        let suffix = format!("-{}-{}.tar.gz", os, arch);
        let entries = std::fs::read_dir(source_dir)?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("omnish-") && name.ends_with(&suffix) {
                let version = Self::extract_version(&name, os, arch);
                let version = match version {
                    Some(v) => v,
                    None => continue,
                };
                // Check if we already have this version cached
                if let Some((cached_ver, _)) = self.cached_package(os, arch) {
                    if Self::compare_versions(&cached_ver, &version) != std::cmp::Ordering::Less {
                        return Ok(false); // Already have same or newer
                    }
                }
                // Copy to tmp, then mv to target
                let platform_dir = self.platform_dir(os, arch);
                std::fs::create_dir_all(&platform_dir)?;
                let tmp_path = platform_dir.join(format!(".tmp-{}", name));
                std::fs::copy(entry.path(), &tmp_path)?;
                // Remove old packages
                if let Ok(old_entries) = std::fs::read_dir(&platform_dir) {
                    for old in old_entries.flatten() {
                        let old_name = old.file_name().to_string_lossy().to_string();
                        if old_name != format!(".tmp-{}", name) && old_name.ends_with(".tar.gz") {
                            let _ = std::fs::remove_file(old.path());
                        }
                    }
                }
                let dest = platform_dir.join(&name);
                std::fs::rename(&tmp_path, &dest)?;
                tracing::info!("cached update package: {}", dest.display());
                return Ok(true);
            }
        }
        Ok(false)
    }
}

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
    fn normalize_version_strips_hash() {
        assert_eq!(UpdateCache::normalize_version("0.8.4-71-gdf067f6"), "0.8.4.71");
        assert_eq!(UpdateCache::normalize_version("0.5.0"), "0.5.0");
        assert_eq!(UpdateCache::normalize_version("0.8.4-71"), "0.8.4.71");
        assert_eq!(UpdateCache::normalize_version("1.0.0-3-gabcdef0"), "1.0.0.3");
    }

    #[test]
    fn compare_versions_semver() {
        use std::cmp::Ordering;
        assert_eq!(UpdateCache::compare_versions("0.10.0", "0.9.0"), Ordering::Greater);
        assert_eq!(UpdateCache::compare_versions("0.5.0", "0.5.0"), Ordering::Equal);
        assert_eq!(UpdateCache::compare_versions("1.0.0", "0.99.99"), Ordering::Greater);
        assert_eq!(UpdateCache::compare_versions("0.4.0", "0.5.0"), Ordering::Less);
        // git describe versions
        assert_eq!(UpdateCache::compare_versions("0.8.4-71-gdf067f6", "0.8.4"), Ordering::Greater);
        assert_eq!(UpdateCache::compare_versions("0.8.4-71-gdf067f6", "0.8.4-72-gabcdef0"), Ordering::Less);
        assert_eq!(UpdateCache::compare_versions("0.8.4-71-gdf067f6", "0.8.5"), Ordering::Less);
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
        let sum = UpdateCache::checksum(&path).unwrap();
        // SHA-256 of "hello world"
        assert_eq!(sum, "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9");
    }
}
