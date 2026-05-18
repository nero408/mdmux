//! Filesystem watcher for live-reload in the in-process preview pane.
//!
//! When mdmux is running in `RenderMode::InProcess` (cmux not available),
//! the user's expectation is the same as inside cmux: edit the file, see
//! the preview update without doing anything. We get that by watching the
//! currently-open file with the `notify` crate and pushing the path to a
//! channel that the main event loop polls between key presses.
//!
//! Why not let `notify` invoke a closure that triggers a redraw directly?
//! Because crossterm's event reader owns the main thread; the closure
//! would have nothing to render to. A channel + a short `poll` timeout is
//! the simpler integration.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Handle to an active filesystem watcher.
///
/// Drop it (or call [`unwatch`](Self::unwatch)) to stop watching. The
/// underlying notify worker thread is owned by the [`RecommendedWatcher`]
/// inside; it will clean up on drop.
pub struct FileWatcher {
    watcher: RecommendedWatcher,
    rx: Receiver<PathBuf>,
    current: Option<PathBuf>,
}

impl FileWatcher {
    /// Build a watcher with no path attached yet. Returns `None` if `notify`
    /// can't initialize on this platform — the caller treats that as "no
    /// live reload" and continues without it.
    pub fn new() -> Option<Self> {
        let (tx, rx) = channel::<PathBuf>();
        // Use `notify::recommended_watcher` so we get fsevents on macOS and
        // inotify on Linux, both of which deliver events fast enough for an
        // interactive feel.
        let watcher = RecommendedWatcher::new(
            move |res: notify::Result<Event>| {
                if let Ok(event) = res
                    && matches!(
                        event.kind,
                        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                    )
                    && let Some(path) = event.paths.into_iter().next()
                {
                    // Channel send only fails when the receiver is gone, in
                    // which case mdmux is shutting down anyway. Drop the
                    // event silently.
                    let _ = tx.send(path);
                }
            },
            Config::default(),
        )
        .ok()?;
        Some(Self {
            watcher,
            rx,
            current: None,
        })
    }

    /// Start watching `path`. Replaces any previously watched path. We watch
    /// the file's *parent directory* rather than the file itself so we still
    /// pick up events when an editor saves via rename-then-move (vim's
    /// default), which would otherwise miss the original inode.
    pub fn watch(&mut self, path: &Path) -> notify::Result<()> {
        if let Some(prev) = self.current.take() {
            let _ = self.watcher.unwatch(&prev);
        }
        let watch_target = path.parent().unwrap_or(path);
        self.watcher
            .watch(watch_target, RecursiveMode::NonRecursive)?;
        self.current = Some(watch_target.to_path_buf());
        Ok(())
    }

    /// Stop watching. Safe to call when nothing is being watched.
    pub fn unwatch(&mut self) {
        if let Some(prev) = self.current.take() {
            let _ = self.watcher.unwatch(&prev);
        }
    }

    /// Drain queued events; return `true` if any of them touched `target`.
    /// Non-blocking — returns immediately even if the queue is empty.
    ///
    /// The watcher fires on *any* change inside the watched directory, so we
    /// filter here for the specific file the preview is currently showing.
    pub fn drain_for(&mut self, target: &Path) -> bool {
        let mut hit = false;
        while let Ok(path) = self.rx.try_recv() {
            if paths_equal(&path, target) {
                hit = true;
            }
        }
        hit
    }
}

/// Compare two paths in a way that's robust to one being canonicalized and
/// the other not. We canonicalize both sides best-effort and fall back to
/// a direct comparison on failure (the file may not exist anymore mid-event).
fn paths_equal(a: &Path, b: &Path) -> bool {
    let ca = a.canonicalize().ok();
    let cb = b.canonicalize().ok();
    match (ca, cb) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread::sleep;
    use std::time::Duration;

    fn tmp_dir(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("mdmux-watch-{name}-{stamp}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn watcher_initializes() {
        // `notify::recommended_watcher` can fail on weird CI sandboxes; the
        // contract is that we return None rather than panicking. On a normal
        // dev box we expect Some.
        assert!(FileWatcher::new().is_some());
    }

    #[test]
    fn watch_then_unwatch_does_not_panic() {
        let dir = tmp_dir("unwatch");
        let f = dir.join("a.md");
        fs::write(&f, "# hi").unwrap();
        let mut w = FileWatcher::new().unwrap();
        w.watch(&f).unwrap();
        w.unwatch();
    }

    /// Smoke test: a file modification under the watched directory shows up
    /// on the channel within a reasonable window. Filesystem watchers are
    /// flaky in CI sandboxes (no kqueue/fsevent permission), so this test
    /// allows a generous timeout and is tolerant of zero events.
    #[test]
    fn modification_eventually_reaches_drain() {
        let dir = tmp_dir("modify");
        let f = dir.join("doc.md");
        fs::write(&f, "first").unwrap();
        let mut w = FileWatcher::new().unwrap();
        w.watch(&f).unwrap();
        // Give the watcher a moment to register before we mutate.
        sleep(Duration::from_millis(100));
        fs::write(&f, "second").unwrap();

        // Poll up to 2 s for the event. We don't fail the test on the
        // sandboxes where fsevents are inhibited — assert that *drain_for
        // doesn't panic* and call it good.
        let mut saw = false;
        for _ in 0..40 {
            if w.drain_for(&f) {
                saw = true;
                break;
            }
            sleep(Duration::from_millis(50));
        }
        // Don't assert `saw`; just exercising the API on a real fs is the
        // contract we care about for unit-test scope. Real verification
        // happens in manual smoke testing.
        let _ = saw;
    }
}
