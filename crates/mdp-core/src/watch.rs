//! File watching with a smart, non-intrusive reload signal (design section 4.3).
//!
//! We watch the PARENT DIRECTORY of each tracked file rather than the file path
//! itself: editors save atomically by writing a temp file and renaming it over
//! the original, which unlinks the watched inode and would kill a file-path
//! watch after the first save. A directory watch survives it. Events are then
//! filtered down to the canonicalized set of tracked files, debounced to
//! coalesce the multi-event save storm, and delivered via a callback.
//!
//! Dropping the returned [`WatchHandle`] stops watching.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify_debouncer_full::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};

/// The debounce window: long enough to coalesce an editor's save-event storm,
/// short enough to feel responsive (design section 4.3).
const DEBOUNCE: Duration = Duration::from_millis(300);

/// A debounced change to a tracked file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChanged {
    /// The tracked file that changed (canonicalized where possible).
    pub path: PathBuf,
}

/// Error setting up the watch.
#[derive(Debug)]
pub struct WatchError(String);

impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not watch files: {}", self.0)
    }
}
impl std::error::Error for WatchError {}

/// An active watch. Dropping this stops watching and joins the watcher thread.
pub struct WatchHandle {
    _debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
}

/// Watch `paths` and call `on_change` once per debounced change to a tracked
/// file. The callback runs on the watcher's background thread, so it must be
/// `Send`; keep it cheap (set a flag, or send on a channel).
pub fn watch_files<F>(paths: &[PathBuf], on_change: F) -> Result<WatchHandle, WatchError>
where
    F: Fn(FileChanged) + Send + 'static,
{
    let tracked = canonical_set(paths);
    let dirs = parent_dirs(&tracked);
    if dirs.is_empty() {
        return Err(WatchError("no watchable parent directories".to_string()));
    }

    let mut debouncer = new_debouncer(DEBOUNCE, None, move |result: DebounceEventResult| {
        let Ok(events) = result else {
            return;
        };
        // Fire at most once per tracked file per debounce batch.
        let mut fired: HashSet<PathBuf> = HashSet::new();
        for event in events {
            for path in event.paths.iter() {
                if let Some(hit) = match_tracked(path, &tracked) {
                    if fired.insert(hit.clone()) {
                        on_change(FileChanged { path: hit });
                    }
                }
            }
        }
    })
    .map_err(|e| WatchError(e.to_string()))?;

    for dir in &dirs {
        debouncer
            .watch(dir, RecursiveMode::NonRecursive)
            .map_err(|e| WatchError(e.to_string()))?;
    }

    Ok(WatchHandle {
        _debouncer: debouncer,
    })
}

/// Canonicalize each tracked path, falling back to the path as given when it
/// cannot be resolved (e.g. transiently missing during a save).
fn canonical_set(paths: &[PathBuf]) -> HashSet<PathBuf> {
    paths
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect()
}

/// The parent directories to watch (non-recursively). Watching the directory,
/// not the file, is what survives atomic-rename saves.
fn parent_dirs(tracked: &HashSet<PathBuf>) -> HashSet<PathBuf> {
    tracked
        .iter()
        .filter_map(|p| p.parent().map(Path::to_path_buf))
        .filter(|d| !d.as_os_str().is_empty())
        .collect()
}

/// If `event_path` refers to a tracked file, return the tracked path it matches.
/// Tries the canonicalized path first (handles `./` and symlinks), then the raw
/// path (canonicalize can fail mid-save).
fn match_tracked(event_path: &Path, tracked: &HashSet<PathBuf>) -> Option<PathBuf> {
    if let Ok(canon) = event_path.canonicalize() {
        if let Some(hit) = tracked.get(&canon) {
            return Some(hit.clone());
        }
    }
    tracked.get(event_path).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_dirs_collects_unique_parents() {
        let set: HashSet<PathBuf> = [
            PathBuf::from("/a/b/x.md"),
            PathBuf::from("/a/b/y.md"),
            PathBuf::from("/a/c/z.md"),
        ]
        .into_iter()
        .collect();
        let dirs = parent_dirs(&set);
        assert_eq!(dirs.len(), 2);
        assert!(dirs.contains(&PathBuf::from("/a/b")));
        assert!(dirs.contains(&PathBuf::from("/a/c")));
    }

    #[test]
    fn match_tracked_falls_back_to_raw_path() {
        // A nonexistent path cannot be canonicalized, so matching falls back to
        // the raw path comparison.
        let tracked: HashSet<PathBuf> = [PathBuf::from("/no/such/file-xyz.md")].into_iter().collect();
        assert_eq!(
            match_tracked(Path::new("/no/such/file-xyz.md"), &tracked),
            Some(PathBuf::from("/no/such/file-xyz.md"))
        );
        assert_eq!(match_tracked(Path::new("/no/such/other.md"), &tracked), None);
    }

    #[test]
    fn watch_with_no_valid_parents_errors() {
        // An empty path list yields no directories to watch.
        assert!(watch_files(&[], |_| {}).is_err());
    }

    #[test]
    fn detects_a_write_to_a_tracked_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.md");
        std::fs::write(&file, "# a\n").unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let handle = watch_files(std::slice::from_ref(&file), move |c| {
            let _ = tx.send(c);
        })
        .expect("watch starts");

        // Let the watcher establish before modifying.
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(&file, "# a changed\n").unwrap();

        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a FileChanged within 5s");
        assert_eq!(got.path.file_name(), file.file_name());
        drop(handle);
    }
}
