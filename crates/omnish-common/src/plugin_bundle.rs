//! Issue #588: shared plugin bundle packaging used by both the daemon
//! (to produce a bundle for clients to mirror) and the client (to compute
//! the checksum of its own local `plugins/` so it can tell whether the
//! daemon's state differs).
//!
//! The bundle is a deterministic gzip-compressed tar of `plugins_dir/`:
//! entries sorted by relative path, fixed mtime/uid/gid, normalized mode
//! (0755 for dirs/executables, 0644 for data), no dotfiles, no symlinks.
//! Identical input directories yield byte-identical output and checksum.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// In-memory bundle plus its SHA-256 hex digest. An empty `bytes` + empty
/// `checksum` means no bundle was produced (e.g. `plugins_dir` does not
/// exist yet).
#[derive(Default, Clone)]
pub struct Bundle {
    pub bytes: Vec<u8>,
    pub checksum: String,
}

/// Package `plugins_dir` into a deterministic tar.gz + SHA-256. Missing
/// directories return an empty Bundle (not an error). See module docs for
/// what determinism guarantees.
pub fn build_bundle(plugins_dir: &Path) -> std::io::Result<Bundle> {
    if !plugins_dir.exists() {
        return Ok(Bundle::default());
    }
    let gz = encode_tar_gz(plugins_dir)?;
    let checksum = hex_sha256(&gz);
    Ok(Bundle { bytes: gz, checksum })
}

fn encode_tar_gz(plugins_dir: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Write;

    // Collect (relative_path, absolute_path, is_dir, is_executable), sort by
    // relative_path so the tar contents are byte-identical across rebuilds
    // regardless of filesystem enumeration order.
    let mut entries: Vec<(PathBuf, PathBuf, bool, bool)> = Vec::new();
    walk(plugins_dir, plugins_dir, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
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
        if name_str.starts_with('.') {
            // Skip .bundle_checksum (legacy client-local state, harmless to
            // exclude now) and .git/.DS_Store-style metadata.
            continue;
        }
        let path = entry.path();
        let rel = path.strip_prefix(root).map(|p| p.to_path_buf()).unwrap_or(path.clone());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
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
    fn checksum_stable_across_builds() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/tool.json"), b"{\"x\":1}", false);
        write_file(&dir.path().join("a/script"), b"#!/bin/sh\n", true);

        let b1 = build_bundle(dir.path()).unwrap();
        let b2 = build_bundle(dir.path()).unwrap();
        assert!(!b1.checksum.is_empty());
        assert_eq!(b1.checksum, b2.checksum);
        assert_eq!(b1.bytes, b2.bytes);
    }

    #[test]
    fn checksum_changes_on_content_change() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/tool.json"), b"v1", false);
        let b1 = build_bundle(dir.path()).unwrap();
        write_file(&dir.path().join("a/tool.json"), b"v2", false);
        let b2 = build_bundle(dir.path()).unwrap();
        assert_ne!(b1.checksum, b2.checksum);
    }

    #[test]
    fn missing_dir_yields_empty_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let b = build_bundle(&dir.path().join("does_not_exist")).unwrap();
        assert_eq!(b.checksum, "");
        assert!(b.bytes.is_empty());
    }

    #[test]
    fn dotfiles_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/tool.json"), b"keep", false);
        write_file(&dir.path().join("a/.hidden"), b"skip", false);
        write_file(&dir.path().join(".git/HEAD"), b"skip", false);

        let b = build_bundle(dir.path()).unwrap();
        let cursor = std::io::Cursor::new(&b.bytes);
        let gz = flate2::read::GzDecoder::new(cursor);
        let mut ar = tar::Archive::new(gz);
        let entries: Vec<String> = ar.entries().unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(entries.iter().any(|p| p == "a/tool.json"));
        assert!(!entries.iter().any(|p| p.contains(".hidden") || p.starts_with(".git")));
    }
}
