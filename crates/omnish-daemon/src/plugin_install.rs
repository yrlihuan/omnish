//! Issue #589: install a plugin from a URL or local tar.gz archive.
//!
//! Triggered from the config menu `plugins → Install plugin`. The daemon
//! downloads the archive (http/https) or reads a local path, validates
//! that it contains exactly one top-level directory (plugin name) holding
//! a `tool.json`, and merges the contents into
//! `{omnish_dir}/plugins/<name>/`. Files present in the destination but
//! absent from the archive are preserved (e.g. `tool.override.json` a user
//! hand-edited).

use omnish_protocol::message::{Message, NoticeLevel};
use omnish_transport::rpc_server::PushRegistry;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const INSTALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Reject archives whose compressed bytes exceed 10 MiB. Large plugins are
/// out of scope; we want to fail fast rather than stream unbounded data.
const MAX_ARCHIVE_SIZE: u64 = 10 * 1024 * 1024;

/// Download, validate, and merge a plugin archive. Returns immediately; the
/// outcome is broadcast as a `NoticePush` through `push_registry`.
pub fn spawn_install_plugin(url: String, omnish_dir: PathBuf, push_registry: PushRegistry) {
    tokio::spawn(async move {
        let outcome = tokio::time::timeout(
            INSTALL_TIMEOUT,
            install_plugin_inner(&url, &omnish_dir),
        ).await;
        let (level, text) = match outcome {
            Ok(Ok(name)) => (NoticeLevel::Info, format!("Plugin '{}' installed", name)),
            Ok(Err(e)) => (NoticeLevel::Error, format!("Install plugin failed: {}", e)),
            Err(_) => (
                NoticeLevel::Error,
                format!("Install plugin timed out after {}s", INSTALL_TIMEOUT.as_secs()),
            ),
        };
        broadcast_notice(&push_registry, level, text).await;
    });
}

async fn install_plugin_inner(url: &str, omnish_dir: &Path) -> anyhow::Result<String> {
    let bytes = fetch_bytes(url).await?;
    if bytes.len() as u64 > MAX_ARCHIVE_SIZE {
        anyhow::bail!(
            "archive too large ({} bytes, limit {})",
            bytes.len(),
            MAX_ARCHIVE_SIZE,
        );
    }

    let tmp = tempfile::tempdir()?;
    let extract_root = tmp.path().to_path_buf();
    let bytes_for_extract = bytes.clone();
    tokio::task::spawn_blocking(move || extract_tar_gz(&bytes_for_extract, &extract_root))
        .await??;

    let name = validate_structure(tmp.path())?;
    let src_dir = tmp.path().join(&name);
    let plugins_dir = omnish_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir)?;
    let dest = plugins_dir.join(&name);
    merge_dir(&src_dir, &dest)?;
    Ok(name)
}

async fn fetch_bytes(url: &str) -> anyhow::Result<Vec<u8>> {
    if url.starts_with("http://") || url.starts_with("https://") {
        let resp = reqwest::get(url).await?.error_for_status()?;
        // Pre-check content-length when the server advertises it - avoids
        // buffering a large body just to reject it.
        if let Some(len) = resp.content_length() {
            if len > MAX_ARCHIVE_SIZE {
                anyhow::bail!("archive too large ({} bytes, limit {})", len, MAX_ARCHIVE_SIZE);
            }
        }
        let bytes = resp.bytes().await?;
        Ok(bytes.to_vec())
    } else {
        let path = Path::new(url);
        if !path.is_file() {
            anyhow::bail!("local path not found or not a file: {}", url);
        }
        Ok(tokio::fs::read(path).await?)
    }
}

/// Unpack a gzipped tar into `to`. Rejects entries with absolute paths or
/// any `..` component so we can't escape the extraction root.
fn extract_tar_gz(bytes: &[u8], to: &Path) -> anyhow::Result<()> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    archive.set_preserve_permissions(true);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path.is_absolute()
            || path.components().any(|c| matches!(c, Component::ParentDir))
        {
            anyhow::bail!("archive contains unsafe path: {}", path.display());
        }
        entry.unpack_in(to)?;
    }
    Ok(())
}

/// The archive must contain exactly one top-level directory (ignoring
/// dotfiles), and that directory must contain a `tool.json`. The directory
/// name becomes the plugin name.
fn validate_structure(tmp: &Path) -> anyhow::Result<String> {
    let mut top_dirs = Vec::new();
    let mut has_root_file = false;
    for entry in std::fs::read_dir(tmp)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let ft = entry.file_type()?;
        if ft.is_dir() {
            top_dirs.push(name);
        } else if ft.is_file() {
            has_root_file = true;
        }
    }
    if has_root_file {
        anyhow::bail!("archive must contain a single top-level directory, not files");
    }
    if top_dirs.is_empty() {
        anyhow::bail!("archive is empty");
    }
    if top_dirs.len() > 1 {
        anyhow::bail!(
            "archive must contain exactly one top-level directory, found {}",
            top_dirs.len()
        );
    }
    let name = top_dirs.into_iter().next().unwrap();
    if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        anyhow::bail!("invalid plugin directory name: {}", name);
    }
    if !tmp.join(&name).join("tool.json").is_file() {
        anyhow::bail!("archive top-level directory '{}' missing tool.json", name);
    }
    Ok(name)
}

/// Copy every file in `src` into `dest` (creating subdirs), overwriting
/// existing files. Files in `dest` that aren't mirrored in `src` are left
/// alone - this is the "preserve tool.override.json" requirement.
fn merge_dir(src: &Path, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    merge_recursive(src, dest)
}

fn merge_recursive(src: &Path, dest: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dest.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            std::fs::create_dir_all(&dst_path)?;
            merge_recursive(&src_path, &dst_path)?;
        } else if ft.is_file() {
            if let Some(parent) = dst_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if dst_path.exists() {
                // Remove the existing file first so copy preserves the
                // source's permissions (otherwise std::fs::copy keeps the
                // destination's pre-existing permissions).
                std::fs::remove_file(&dst_path)?;
            }
            std::fs::copy(&src_path, &dst_path)?;
        }
        // Symlinks and other types are skipped intentionally.
    }
    Ok(())
}

/// The kind tag attached to every install-plugin notice. Clients that
/// didn't initiate an install filter these out via `PendingNotices`.
const NOTICE_KIND: &str = "install_plugin";

async fn broadcast_notice(registry: &PushRegistry, level: NoticeLevel, text: String) {
    let senders: Vec<_> = {
        let map = registry.lock().await;
        map.values().cloned().collect()
    };
    for tx in senders {
        let msg = Message::NoticePush {
            level: level.clone(),
            text: text.clone(),
            kind: Some(NOTICE_KIND.to_string()),
        };
        let _ = tx.send(msg).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut tar = tar::Builder::new(&mut gz);
            for (path, data) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_mtime(0);
                header.set_cksum();
                tar.append_data(&mut header, path, *data).unwrap();
            }
            tar.finish().unwrap();
        }
        gz.finish().unwrap()
    }

    #[test]
    fn validate_structure_accepts_single_dir_with_tool_json() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("my_plugin")).unwrap();
        std::fs::write(tmp.path().join("my_plugin/tool.json"), "{}").unwrap();
        assert_eq!(validate_structure(tmp.path()).unwrap(), "my_plugin");
    }

    #[test]
    fn validate_structure_rejects_missing_tool_json() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("my_plugin")).unwrap();
        std::fs::write(tmp.path().join("my_plugin/readme.txt"), "x").unwrap();
        let err = validate_structure(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("tool.json"), "err: {}", err);
    }

    #[test]
    fn validate_structure_rejects_multiple_top_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("a")).unwrap();
        std::fs::create_dir(tmp.path().join("b")).unwrap();
        std::fs::write(tmp.path().join("a/tool.json"), "{}").unwrap();
        std::fs::write(tmp.path().join("b/tool.json"), "{}").unwrap();
        let err = validate_structure(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("exactly one"), "err: {}", err);
    }

    #[test]
    fn validate_structure_rejects_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let err = validate_structure(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("empty"), "err: {}", err);
    }

    #[test]
    fn validate_structure_rejects_root_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("loose.txt"), "x").unwrap();
        let err = validate_structure(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("single top-level directory"), "err: {}", err);
    }

    #[test]
    fn validate_structure_ignores_dotfiles() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".DS_Store"), "x").unwrap();
        std::fs::create_dir(tmp.path().join("my_plugin")).unwrap();
        std::fs::write(tmp.path().join("my_plugin/tool.json"), "{}").unwrap();
        assert_eq!(validate_structure(tmp.path()).unwrap(), "my_plugin");
    }

    #[test]
    fn merge_dir_overwrites_and_preserves() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();

        // source: tool.json (new) + script.sh (new)
        std::fs::write(src.path().join("tool.json"), "{\"v\":2}").unwrap();
        std::fs::write(src.path().join("script.sh"), "#!/bin/sh\nnew\n").unwrap();

        // dest pre-populated: tool.json (old, should be overwritten),
        // tool.override.json (should be preserved), script.sh (old → overwritten)
        std::fs::write(dest.path().join("tool.json"), "{\"v\":1}").unwrap();
        std::fs::write(dest.path().join("tool.override.json"), "{\"desc\":\"mine\"}").unwrap();
        std::fs::write(dest.path().join("script.sh"), "#!/bin/sh\nold\n").unwrap();

        merge_dir(src.path(), dest.path()).unwrap();

        assert_eq!(std::fs::read_to_string(dest.path().join("tool.json")).unwrap(), "{\"v\":2}");
        assert_eq!(std::fs::read_to_string(dest.path().join("script.sh")).unwrap(), "#!/bin/sh\nnew\n");
        // Preserved:
        assert_eq!(std::fs::read_to_string(dest.path().join("tool.override.json")).unwrap(),
                   "{\"desc\":\"mine\"}");
    }

    #[test]
    fn merge_dir_recurses_into_subdirectories() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        std::fs::create_dir(src.path().join("lib")).unwrap();
        std::fs::write(src.path().join("lib/helper.sh"), "new").unwrap();
        std::fs::create_dir(dest.path().join("lib")).unwrap();
        std::fs::write(dest.path().join("lib/helper.sh"), "old").unwrap();
        std::fs::write(dest.path().join("lib/keep.txt"), "keep").unwrap();

        merge_dir(src.path(), dest.path()).unwrap();

        assert_eq!(std::fs::read_to_string(dest.path().join("lib/helper.sh")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(dest.path().join("lib/keep.txt")).unwrap(), "keep");
    }

    #[test]
    fn extract_tar_gz_unpacks_valid_archive() {
        let bytes = make_tar_gz(&[
            ("my_plugin/tool.json", b"{}"),
            ("my_plugin/run.sh", b"#!/bin/sh\n"),
        ]);
        let tmp = tempfile::tempdir().unwrap();
        extract_tar_gz(&bytes, tmp.path()).unwrap();
        assert!(tmp.path().join("my_plugin/tool.json").is_file());
        assert!(tmp.path().join("my_plugin/run.sh").is_file());
    }

    // We don't unit-test the `..` component guard in extract_tar_gz because
    // the `tar` crate's Builder API itself refuses to construct archives
    // with unsafe paths (see `paths in archives must not have '..'` in its
    // source). The check remains in the extractor as defense-in-depth for
    // archives produced by other tooling that might be less strict.

    #[test]
    fn install_plugin_end_to_end_via_local_path() {
        // Produce a valid archive and save it to a file; drive
        // install_plugin_inner through that path.
        let archive_bytes = make_tar_gz(&[
            ("plug/tool.json", b"{\"name\":\"plug\"}"),
            ("plug/run.sh", b"#!/bin/sh\necho hi\n"),
        ]);
        let archive_file = tempfile::NamedTempFile::new().unwrap();
        archive_file.as_file().write_all(&archive_bytes).unwrap();
        let archive_path = archive_file.path().to_path_buf();

        let omnish = tempfile::tempdir().unwrap();
        // Pre-existing plugin with an override the user customized.
        let plug_dir = omnish.path().join("plugins/plug");
        std::fs::create_dir_all(&plug_dir).unwrap();
        std::fs::write(plug_dir.join("tool.override.json"), "{\"desc\":\"mine\"}").unwrap();
        std::fs::write(plug_dir.join("tool.json"), "{\"name\":\"plug\",\"v\":\"old\"}").unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let name = rt.block_on(install_plugin_inner(
            archive_path.to_str().unwrap(),
            omnish.path(),
        )).unwrap();
        assert_eq!(name, "plug");
        assert_eq!(
            std::fs::read_to_string(plug_dir.join("tool.json")).unwrap(),
            "{\"name\":\"plug\"}"
        );
        assert_eq!(
            std::fs::read_to_string(plug_dir.join("tool.override.json")).unwrap(),
            "{\"desc\":\"mine\"}"
        );
        assert!(plug_dir.join("run.sh").is_file());
    }
}
