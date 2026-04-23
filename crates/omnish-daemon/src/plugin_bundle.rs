//! Issue #588: package `~/.omnish/plugins/` into an in-memory tarball and keep
//! it fresh as the directory changes, so connected clients can mirror the
//! daemon's plugin set via the existing update streaming path.
//!
//! The bundle is a gzip-compressed tar of `plugins/` - relative paths start
//! at `<plugin_name>/`, matching the on-disk layout at both ends. Files are
//! included verbatim (content + executable bit); no symlinks, no hidden
//! files (`.git`, etc.), no per-plugin size cap at this layer (the checksum
//! + streaming handles arbitrary sizes fine).
//!
//! `PluginBundler` exposes `snapshot()` returning `(bytes, checksum)`. The
//! checksum is a hex SHA-256 of the tar.gz bytes, used by the client to tell
//! whether its installed bundle matches the daemon's current one.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Cached bundle + its checksum. Empty bytes + empty checksum means no
/// bundle has been built yet (e.g. daemon just started and the plugins dir
/// does not exist).
#[derive(Default, Clone)]
pub struct Bundle {
    pub bytes: Vec<u8>,
    pub checksum: String,
}

pub struct PluginBundler {
    plugins_dir: PathBuf,
    state: RwLock<Bundle>,
}

impl PluginBundler {
    pub fn new(plugins_dir: PathBuf) -> Self {
        Self {
            plugins_dir,
            state: RwLock::new(Bundle::default()),
        }
    }

    /// Rebuild the in-memory bundle by scanning `plugins_dir`. Returns the
    /// new checksum (empty if the dir does not exist or has no files).
    /// Errors are logged and the previous snapshot is preserved.
    pub fn rebuild(&self) -> String {
        match build_bundle(&self.plugins_dir) {
            Ok(bundle) => {
                let checksum = bundle.checksum.clone();
                *self.state.write().unwrap() = bundle;
                checksum
            }
            Err(e) => {
                tracing::warn!("plugin_bundle: rebuild failed: {}", e);
                self.state.read().unwrap().checksum.clone()
            }
        }
    }

    /// Cheap cloned snapshot of the current bundle. Callers stream from the
    /// returned Vec via the shared chunk sender in `server.rs`.
    pub fn snapshot(&self) -> Bundle {
        self.state.read().unwrap().clone()
    }

    /// Current checksum without cloning the bytes. Fast-path for
    /// `PluginSyncCheck` handling.
    pub fn checksum(&self) -> String {
        self.state.read().unwrap().checksum.clone()
    }
}

fn build_bundle(plugins_dir: &Path) -> std::io::Result<Bundle> {
    if !plugins_dir.exists() {
        return Ok(Bundle::default());
    }
    let gz = flate2_like_gzip_encoder(plugins_dir)?;
    let checksum = hex_sha256(&gz);
    Ok(Bundle { bytes: gz, checksum })
}

/// Build a deterministic tar.gz of `plugins_dir`. Deterministic matters so
/// the checksum stabilizes when the directory hasn't changed - otherwise
/// every rebuild would yield a new hash and cause pointless re-downloads.
fn flate2_like_gzip_encoder(plugins_dir: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Write;

    // Collect (relative_path, absolute_path, is_dir, is_executable) sorted by
    // relative_path so the tar contents are byte-identical across rebuilds
    // regardless of the OS filesystem ordering.
    let mut entries: Vec<(PathBuf, PathBuf, bool, bool)> = Vec::new();
    walk(plugins_dir, plugins_dir, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Minimal gzip: we don't have flate2 here; use a raw tar and compress
    // via a tiny gzip wrapper built from the `tar` crate's output piped
    // through flate2. Add flate2 as a dep.
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        // Deterministic mtime so unchanged content hashes stably.
        const FIXED_MTIME: u64 = 0;
        for (rel, abs, is_dir, executable) in &entries {
            let mut header = tar::Header::new_gnu();
            let metadata = std::fs::metadata(abs)?;
            if *is_dir {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
            } else {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(metadata.len());
            }
            // Permissions: 0755 for executables and dirs, 0644 for data.
            let mode: u32 = if *is_dir || *executable { 0o755 } else { 0o644 };
            header.set_mode(mode);
            header.set_mtime(FIXED_MTIME);
            header.set_uid(0);
            header.set_gid(0);
            header.set_cksum();
            if *is_dir {
                builder.append_data(&mut header, rel, std::io::empty())?;
            } else {
                let file = std::fs::File::open(abs)?;
                builder.append_data(&mut header, rel, file)?;
            }
        }
        builder.finish()?;
    }

    // Wrap in gzip.
    let mut gz_bytes = Vec::new();
    {
        let mut encoder = flate2::write::GzEncoder::new(&mut gz_bytes, flate2::Compression::default());
        encoder.write_all(&tar_bytes)?;
        encoder.finish()?;
    }
    Ok(gz_bytes)
}

fn walk(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(PathBuf, PathBuf, bool, bool)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Skip dotfiles (e.g. .git, .DS_Store) and hidden dirs.
        if name_str.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let rel = path.strip_prefix(root).map(|p| p.to_path_buf()).unwrap_or(path.clone());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            // Plugins are expected to be regular files + directories. Symlinks
            // are skipped to keep the bundle self-contained.
            continue;
        }
        if ft.is_dir() {
            out.push((rel.clone(), path.clone(), true, false));
            walk(root, &path, out)?;
        } else if ft.is_file() {
            let executable = is_executable(&path);
            out.push((rel, path, false, executable));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_p: &Path) -> bool { false }

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    let mut s = String::with_capacity(64);
    for b in result { s.push_str(&format!("{:02x}", b)); }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(path: &Path, content: &[u8], exec: bool) {
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).unwrap(); }
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content).unwrap();
        #[cfg(unix)]
        if exec {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(path).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(path, perm).unwrap();
        }
        #[cfg(not(unix))]
        let _ = exec;
    }

    #[test]
    fn checksum_is_stable_across_rebuilds() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().to_path_buf();
        write_file(&plugins.join("a/tool.json"), b"{\"x\":1}", false);
        write_file(&plugins.join("a/script"), b"#!/bin/sh\n", true);

        let bundler = PluginBundler::new(plugins);
        let c1 = bundler.rebuild();
        let c2 = bundler.rebuild();
        assert!(!c1.is_empty());
        assert_eq!(c1, c2, "unchanged content must yield stable checksum");
    }

    #[test]
    fn checksum_changes_on_content_change() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().to_path_buf();
        write_file(&plugins.join("a/tool.json"), b"v1", false);

        let bundler = PluginBundler::new(plugins.clone());
        let c1 = bundler.rebuild();
        write_file(&plugins.join("a/tool.json"), b"v2", false);
        let c2 = bundler.rebuild();
        assert_ne!(c1, c2);
    }

    #[test]
    fn missing_dir_yields_empty_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let bundler = PluginBundler::new(dir.path().join("does_not_exist"));
        let checksum = bundler.rebuild();
        assert_eq!(checksum, "");
        assert!(bundler.snapshot().bytes.is_empty());
    }

    #[test]
    fn dotfiles_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().to_path_buf();
        write_file(&plugins.join("a/tool.json"), b"keep", false);
        write_file(&plugins.join("a/.hidden"), b"skip", false);
        write_file(&plugins.join(".git/HEAD"), b"skip", false);

        let bundler = PluginBundler::new(plugins);
        let c = bundler.rebuild();
        let bytes = bundler.snapshot().bytes;
        // Decompress and list entries.
        let cursor = std::io::Cursor::new(&bytes);
        let gz = flate2::read::GzDecoder::new(cursor);
        let mut ar = tar::Archive::new(gz);
        let entries: Vec<String> = ar.entries().unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(entries.iter().any(|p| p == "a/tool.json"));
        assert!(!entries.iter().any(|p| p.contains(".hidden") || p.starts_with(".git")));
        assert!(!c.is_empty());
    }
}
