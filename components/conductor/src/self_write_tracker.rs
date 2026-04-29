use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// Tracks paths that the conductor has written itself, so the file watcher can
/// distinguish self-triggered events (no-op) from genuine external edits (must rebuild).
///
/// Each entry records the mtime observed immediately after the write and the wall-clock
/// instant of the write. An event is suppressed only when:
///   - the path was recorded within the retention window, AND
///   - the current mtime matches the recorded mtime
///
/// A genuine external edit will (almost always) produce a different mtime, so it will
/// still be processed even if it races with the retention window.
#[derive(Default)]
pub struct SelfWriteTracker {
    inner: Mutex<HashMap<PathBuf, (SystemTime, Instant)>>,
    retention: Duration,
}

impl SelfWriteTracker {
    /// Create a tracker with the given retention window.
    ///
    /// Events are suppressed for at most `retention` after the write.
    pub fn new(retention: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            retention,
        }
    }

    /// Record that the conductor wrote `path` and the resulting mtime is `mtime`.
    ///
    /// Expired entries are evicted eagerly on every call so the map stays small.
    pub fn record(&self, path: PathBuf, mtime: SystemTime) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let retention = self.retention;
        inner.retain(|_, (_, recorded_at)| now.duration_since(*recorded_at) < retention);
        inner.insert(path, (mtime, now));
    }

    /// Returns `true` if the watcher should ignore an event for `path` whose current
    /// on-disk mtime is `current_mtime`.
    ///
    /// Suppression requires both:
    /// - the path was recorded within the retention window, AND
    /// - the mtime matches (guards against an external write that happened to race).
    #[allow(dead_code)]
    pub fn should_ignore(&self, path: &Path, current_mtime: SystemTime) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match inner.get(path) {
            Some((recorded_mtime, recorded_at)) => {
                Instant::now().duration_since(*recorded_at) < self.retention
                    && *recorded_mtime == current_mtime
            }
            None => false,
        }
    }
}

/// Write `content` to `path` and immediately record the resulting mtime in `tracker`.
///
/// Use this instead of `std::fs::write` for all source-directory writes (schemas/,
/// content/, templates/) so the file watcher can suppress the resulting events.
pub fn write_source_file(
    path: &Path,
    content: &[u8],
    tracker: &SelfWriteTracker,
) -> std::io::Result<()> {
    std::fs::write(path, content)?;
    let mtime = std::fs::metadata(path)?.modified()?;
    tracker.record(path.to_path_buf(), mtime);
    Ok(())
}

/// Walk all files under `dir` recursively and record each one's current mtime in `tracker`.
///
/// Used after bulk operations (e.g. `ScaffoldSite`) that write many files through
/// APIs we cannot intercept at the individual-write level.
#[allow(dead_code)]
pub fn record_dir_recursive(dir: &Path, tracker: &SelfWriteTracker) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            record_dir_recursive(&path, tracker);
        } else if let Ok(meta) = std::fs::metadata(&path)
            && let Ok(mtime) = meta.modified()
        {
            tracker.record(path, mtime);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn record_then_should_ignore_same_mtime() {
        let tracker = SelfWriteTracker::new(Duration::from_secs(5));
        let path = PathBuf::from("/site/content/post.md");
        let mtime = SystemTime::now();

        tracker.record(path.clone(), mtime);

        assert!(
            tracker.should_ignore(&path, mtime),
            "same mtime should be ignored after record"
        );
    }

    #[test]
    fn should_not_ignore_different_mtime() {
        let tracker = SelfWriteTracker::new(Duration::from_secs(5));
        let path = PathBuf::from("/site/content/post.md");
        let mtime_x = SystemTime::now();
        let mtime_y = mtime_x + Duration::from_secs(1);

        tracker.record(path.clone(), mtime_x);

        assert!(
            !tracker.should_ignore(&path, mtime_y),
            "different mtime (genuine external edit) should not be ignored"
        );
    }

    #[test]
    fn should_not_ignore_after_expiry() {
        let tracker = SelfWriteTracker::new(Duration::from_millis(50));
        let path = PathBuf::from("/site/content/post.md");
        let mtime = SystemTime::now();

        tracker.record(path.clone(), mtime);

        // Wait for the retention window to expire
        std::thread::sleep(Duration::from_millis(100));

        assert!(
            !tracker.should_ignore(&path, mtime),
            "should not ignore after retention window expires"
        );
    }

    #[test]
    fn should_not_ignore_unrecorded_path() {
        let tracker = SelfWriteTracker::new(Duration::from_secs(5));
        let path = PathBuf::from("/site/content/never-written.md");
        let mtime = SystemTime::now();

        assert!(
            !tracker.should_ignore(&path, mtime),
            "path never recorded should not be ignored"
        );
    }
}
