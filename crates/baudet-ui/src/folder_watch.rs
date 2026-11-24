use crate::panels::{Changes, IGNORED_DIRS};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(175);

enum Request {
    Changed,
    Refresh,
    Stop,
}

pub(crate) struct FolderWatch {
    root: String,
    requests: Sender<Request>,
    updates: Receiver<Changes>,
}

impl FolderWatch {
    pub(crate) fn start(root: &str, ctx: &egui::Context) -> Self {
        let (requests, inbox) = mpsc::channel();
        let (publish, updates) = mpsc::channel();
        let events = requests.clone();
        let worker_root = root.to_owned();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let path = PathBuf::from(&worker_root);
            let watched_path = path.canonicalize().unwrap_or(path);
            let event_root = watched_path.clone();
            let pending = Arc::new(AtomicBool::new(false));
            let event_pending = Arc::clone(&pending);
            let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
                if event.map_or(true, |event| relevant(&event_root, &event))
                    && !event_pending.swap(true, Ordering::Relaxed)
                {
                    let _ = events.send(Request::Changed);
                }
            })
            .ok();
            if let Some(watcher) = &mut watcher {
                let _ = watcher.watch(&watched_path, RecursiveMode::Recursive);
            }

            loop {
                let mut changes = Changes::default();
                changes.refresh(Some(&worker_root));
                if publish.send(changes).is_err() {
                    return;
                }
                ctx.request_repaint();

                match inbox.recv() {
                    Ok(Request::Refresh) => {}
                    Ok(Request::Changed) if coalesce(&inbox) => pending.store(false, Ordering::Relaxed),
                    _ => return,
                }
            }
        });
        Self { root: root.into(), requests, updates }
    }

    pub(crate) fn root(&self) -> &str {
        &self.root
    }

    pub(crate) fn refresh(&self) {
        let _ = self.requests.send(Request::Refresh);
    }

    pub(crate) fn latest(&self) -> Option<Changes> {
        self.updates.try_iter().last()
    }
}

impl Drop for FolderWatch {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Stop);
    }
}

fn coalesce(inbox: &Receiver<Request>) -> bool {
    let deadline = Instant::now() + DEBOUNCE;
    loop {
        match inbox.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Request::Changed) if Instant::now() < deadline => {}
            Ok(Request::Refresh | Request::Changed) | Err(mpsc::RecvTimeoutError::Timeout) => return true,
            Ok(Request::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return false,
        }
    }
}

fn relevant(root: &Path, event: &Event) -> bool {
    !matches!(event.kind, EventKind::Access(_))
        && (event.paths.is_empty()
            || event.paths.iter().any(|path| {
                path.strip_prefix(root).is_ok_and(|relative| {
                    !relative.iter().any(|part| part == ".git" || IGNORED_DIRS.iter().any(|name| part == *name))
                })
            }))
}
