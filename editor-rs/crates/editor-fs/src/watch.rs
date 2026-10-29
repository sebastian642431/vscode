//! File-system watching as a channel of [`FsEvent`]s.
//!
//! The UI never polls the disk. Instead it holds a [`Watcher`] and drains its
//! [`crossbeam_channel::Receiver`] from the event loop, refreshing only the
//! parts of the tree that changed.

use std::path::{Path, PathBuf};

use crossbeam_channel::{unbounded, Receiver};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};

/// A classified file-system change.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FsEvent {
    pub kind: FsEventKind,
    pub paths: Vec<PathBuf>,
}

/// The high-level category of an [`FsEvent`], collapsing `notify`'s detailed
/// event taxonomy into what an editor actually reacts to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FsEventKind {
    Created,
    Modified,
    Removed,
    /// A rename whose other side `notify` reported separately, or an event we
    /// can't classify more precisely.
    Other,
}

fn classify(kind: &EventKind) -> FsEventKind {
    match kind {
        EventKind::Create(_) => FsEventKind::Created,
        EventKind::Modify(notify::event::ModifyKind::Name(_)) => FsEventKind::Other,
        EventKind::Modify(_) => FsEventKind::Modified,
        EventKind::Remove(_) => FsEventKind::Removed,
        _ => FsEventKind::Other,
    }
}

/// Watches a directory tree and forwards changes to a channel.
///
/// Holding the `Watcher` keeps the OS watch alive; dropping it stops watching.
pub struct Watcher {
    _inner: RecommendedWatcher,
    rx: Receiver<FsEvent>,
}

impl Watcher {
    /// Start watching `path` recursively. Returns the watcher (which must be
    /// kept alive) and exposes its event stream via [`events`](Self::events).
    pub fn new(path: impl AsRef<Path>) -> notify::Result<Self> {
        let (tx, rx) = unbounded();
        let mut inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                let fs_event = FsEvent {
                    kind: classify(&event.kind),
                    paths: event.paths,
                };
                // If the receiver is gone the UI has shut down; ignore.
                let _ = tx.send(fs_event);
            }
        })?;
        inner.watch(path.as_ref(), RecursiveMode::Recursive)?;
        Ok(Watcher { _inner: inner, rx })
    }

    /// The stream of changes. Drain it non-blockingly with `try_recv` from the
    /// UI loop, or block a worker on `recv`.
    pub fn events(&self) -> &Receiver<FsEvent> {
        &self.rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    #[test]
    fn classify_covers_the_main_categories() {
        use notify::event::{CreateKind, ModifyKind, RemoveKind};
        assert_eq!(
            classify(&EventKind::Create(CreateKind::File)),
            FsEventKind::Created
        );
        assert_eq!(
            classify(&EventKind::Modify(ModifyKind::Data(
                notify::event::DataChange::Content
            ))),
            FsEventKind::Modified
        );
        assert_eq!(
            classify(&EventKind::Remove(RemoveKind::File)),
            FsEventKind::Removed
        );
    }

    #[test]
    fn detects_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = Watcher::new(dir.path()).unwrap();

        // Give the backend a moment to arm, then create a file.
        std::thread::sleep(Duration::from_millis(100));
        fs::write(dir.path().join("new.txt"), "hello").unwrap();

        // Collect events for up to a second; backends vary in timing/coalescing.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut saw_event = false;
        while std::time::Instant::now() < deadline {
            if let Ok(ev) = watcher.events().recv_timeout(Duration::from_millis(200)) {
                if ev.paths.iter().any(|p| p.ends_with("new.txt")) {
                    saw_event = true;
                    break;
                }
            }
        }
        assert!(saw_event, "expected a file-system event for the new file");
    }
}
