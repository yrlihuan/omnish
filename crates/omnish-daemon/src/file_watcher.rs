#[cfg(not(target_os = "linux"))]
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tokio::sync::watch;

pub struct FileWatcher {
    inner: Mutex<WatcherInner>,
    #[cfg(target_os = "linux")]
    inotify: nix::sys::inotify::Inotify,
}

struct WatcherInner {
    watches: Vec<(PathBuf, watch::Sender<()>)>,
    #[cfg(not(target_os = "linux"))]
    mtimes: HashMap<PathBuf, std::time::SystemTime>,
}

impl Default for FileWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl FileWatcher {
    pub fn new() -> Self {
        #[cfg(target_os = "linux")]
        let inotify = {
            use nix::sys::inotify::{InitFlags, Inotify};
            Inotify::init(InitFlags::IN_NONBLOCK)
                .expect("failed to init inotify")
        };

        Self {
            inner: Mutex::new(WatcherInner {
                watches: Vec::new(),
                #[cfg(not(target_os = "linux"))]
                mtimes: HashMap::new(),
            }),
            #[cfg(target_os = "linux")]
            inotify,
        }
    }

    /// Register a path to watch. Can be called at any time.
    /// Returns a Receiver that fires on change.
    ///
    /// For files: watches the **parent directory** via inotify and filters by
    /// filename. This survives editor save-and-rename patterns (vim, sed -i)
    /// that create a new inode. For directories: watches the directory directly.
    pub fn watch(&self, path: PathBuf) -> watch::Receiver<()> {
        let (tx, rx) = watch::channel(());

        let mut inner = self.inner.lock().unwrap();

        #[cfg(target_os = "linux")]
        {
            use nix::sys::inotify::AddWatchFlags;
            // For files, watch the parent directory and filter by filename.
            // For directories, watch the directory itself.
            let (watch_target, flags) = if path.is_dir() {
                // Directory watches care about the full lifecycle of children:
                // IN_CLOSE_WRITE/IN_MOVED_TO for file additions or rewrites,
                // IN_CREATE for mkdir of a new subdirectory (e.g. a new plugin
                // being dropped in), IN_DELETE/IN_MOVED_FROM for removal.
                (
                    path.clone(),
                    AddWatchFlags::IN_CLOSE_WRITE
                        | AddWatchFlags::IN_MOVED_TO
                        | AddWatchFlags::IN_CREATE
                        | AddWatchFlags::IN_DELETE
                        | AddWatchFlags::IN_MOVED_FROM,
                )
            } else {
                // File watches intentionally omit IN_CREATE: it fires when the
                // file is created but still empty (e.g. vim delete-then-recreate),
                // causing a spurious reload with an empty config.
                (
                    path.parent().unwrap_or(std::path::Path::new("/")).to_path_buf(),
                    AddWatchFlags::IN_CLOSE_WRITE | AddWatchFlags::IN_MOVED_TO,
                )
            };
            if let Err(e) = self.inotify.add_watch(&watch_target, flags) {
                tracing::warn!("failed to watch {}: {}", watch_target.display(), e);
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            if let Ok(meta) = std::fs::metadata(&path) {
                if let Ok(mtime) = meta.modified() {
                    inner.mtimes.insert(path.clone(), mtime);
                }
            }
        }

        inner.watches.push((path, tx));
        rx
    }

    /// Start the event loop. Takes &self so the watcher remains usable
    /// for dynamic watch registration.
    #[cfg(target_os = "linux")]
    pub async fn run(&self) {
        use std::os::fd::AsFd;
        use tokio::io::unix::AsyncFd;
        use tokio::io::Interest;

        let async_fd = match AsyncFd::with_interest(
            self.inotify.as_fd().try_clone_to_owned().unwrap(),
            Interest::READABLE,
        ) {
            Ok(fd) => fd,
            Err(e) => {
                tracing::warn!("failed to create AsyncFd for inotify: {}", e);
                return;
            }
        };

        tracing::info!("file watcher started (inotify)");

        loop {
            let mut guard = match async_fd.readable().await {
                Ok(g) => g,
                Err(e) => {
                    tracing::warn!("inotify readable error: {}", e);
                    break;
                }
            };

            // Collect changed filenames from inotify events
            let mut changed_names: Vec<String> = Vec::new();
            loop {
                match self.inotify.read_events() {
                    Ok(events) => {
                        if events.is_empty() {
                            break;
                        }
                        for event in &events {
                            if let Some(name) = &event.name {
                                changed_names.push(name.to_string_lossy().to_string());
                            } else {
                                // Directory-level event (no name) - treat as changed
                                changed_names.push(String::new());
                            }
                        }
                    }
                    Err(nix::errno::Errno::EAGAIN) => break,
                    Err(e) => {
                        tracing::warn!("inotify read error: {}", e);
                        break;
                    }
                }
            }

            guard.clear_ready();

            // Lock once, match event filenames against registered watches
            if !changed_names.is_empty() {
                let inner = self.inner.lock().unwrap();
                for (watched_path, sender) in &inner.watches {
                    let should_notify = if watched_path.is_dir() {
                        // Directory watch: any event in the dir triggers
                        !changed_names.is_empty()
                    } else {
                        // File watch: match by filename (parent dir is watched)
                        let file_name = watched_path.file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_default();
                        changed_names.iter().any(|n| n == &file_name)
                    };
                    if should_notify {
                        let _ = sender.send(());
                    }
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub async fn run(&self) {
        tracing::info!("file watcher started (polling, 5s interval)");

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            // Collect changes first, then apply - avoids borrow conflict
            let inner = self.inner.lock().unwrap();
            let mut updates: Vec<(PathBuf, Option<std::time::SystemTime>)> = Vec::new();
            let mut to_notify: Vec<usize> = Vec::new();

            for (i, (path, _)) in inner.watches.iter().enumerate() {
                let current_mtime = std::fs::metadata(path)
                    .ok()
                    .and_then(|m| m.modified().ok());
                let prev_mtime = inner.mtimes.get(path).copied();

                let changed = match (current_mtime, prev_mtime) {
                    (Some(cur), Some(prev)) => cur != prev,
                    (Some(_), None) => true,   // file appeared
                    (None, Some(_)) => true,   // file disappeared
                    (None, None) => false,
                };

                if changed {
                    updates.push((path.clone(), current_mtime));
                    to_notify.push(i);
                }
            }
            drop(inner); // release immutable borrow

            if !updates.is_empty() {
                let mut inner = self.inner.lock().unwrap();
                for (path, mtime) in updates {
                    if let Some(t) = mtime {
                        inner.mtimes.insert(path, t);
                    } else {
                        inner.mtimes.remove(&path);
                    }
                }
                for i in to_notify {
                    let _ = inner.watches[i].1.send(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_watch_returns_receiver() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.toml");
        std::fs::write(&path, "").unwrap();
        let fw = FileWatcher::new();
        let rx = fw.watch(path);
        assert!(!rx.has_changed().unwrap_or(true));
    }

    #[test]
    fn test_multiple_watches() {
        let tmp = tempfile::tempdir().unwrap();
        let fw = FileWatcher::new();
        let _rx1 = fw.watch(tmp.path().join("a.toml"));
        let _rx2 = fw.watch(tmp.path().join("b.toml"));
        let inner = fw.inner.lock().unwrap();
        assert_eq!(inner.watches.len(), 2);
    }
}
