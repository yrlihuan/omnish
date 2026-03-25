use anyhow::Result;
use sha2::{Sha256, Digest};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Manages the package cache at ~/.omnish/updates/{os}-{arch}/
pub struct UpdateCache {
    omnish_dir: PathBuf,
    cache_dir: PathBuf,
    /// Tracks in-flight background downloads to deduplicate.
    /// Uses Arc so tokio tasks can hold a reference.
    downloading: Arc<Mutex<HashSet<(String, String)>>>,
}

impl UpdateCache {
    pub fn new(omnish_dir: &Path) -> Self {
        let cache_dir = omnish_dir.join("updates");
        Self {
            omnish_dir: omnish_dir.to_path_buf(),
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
        let omnish_dir = self.omnish_dir.clone();
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
        _omnish_dir: &Path,
        _cache_dir: &Path,
        _os: &str,
        _arch: &str,
        _check_url: Option<&str>,
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
    fn compare_versions_semver() {
        use std::cmp::Ordering;
        assert_eq!(UpdateCache::compare_versions("0.10.0", "0.9.0"), Ordering::Greater);
        assert_eq!(UpdateCache::compare_versions("0.5.0", "0.5.0"), Ordering::Equal);
        assert_eq!(UpdateCache::compare_versions("1.0.0", "0.99.99"), Ordering::Greater);
        assert_eq!(UpdateCache::compare_versions("0.4.0", "0.5.0"), Ordering::Less);
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
    fn checksum_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        std::fs::write(&path, b"hello world").unwrap();
        let sum = UpdateCache::checksum(&path).unwrap();
        // SHA-256 of "hello world"
        assert_eq!(sum, "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9");
    }
}
