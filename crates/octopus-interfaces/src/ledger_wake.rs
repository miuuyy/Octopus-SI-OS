use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecursiveMode, Watcher};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::{watch, Notify};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub enum LedgerWakeSource {
    #[default]
    Filesystem,
    Host(HostedLedgerWake),
}

#[derive(Clone)]
pub struct HostedLedgerWake(Arc<HostChannel>);

struct HostChannel {
    request: Notify,
    pending: AtomicBool,
    changed: watch::Sender<Result<(), String>>,
}

impl Default for HostedLedgerWake {
    fn default() -> Self {
        Self(Arc::new(HostChannel {
            request: Notify::new(),
            pending: AtomicBool::new(false),
            changed: watch::channel(Ok(())).0,
        }))
    }
}

impl HostedLedgerWake {
    pub async fn requested(&self) {
        self.0.request.notified().await;
    }

    pub fn acknowledge(&self) -> Result<(), String> {
        if !self.0.pending.swap(false, Ordering::AcqRel) {
            return Err("unsolicited ledger wake from the life host".into());
        }
        self.0.changed.send_modify(|_| {});
        Ok(())
    }

    fn request(&self) {
        if !self.0.pending.swap(true, Ordering::AcqRel) {
            self.0.request.notify_one();
        }
    }
}

struct FileWatch {
    _watcher: notify::RecommendedWatcher,
    changed: watch::Sender<Result<(), String>>,
}

fn ledger_event_reports_a_change(event: &notify::Event) -> bool {
    event.need_rescan()
        || !matches!(
            event.kind,
            EventKind::Access(
                AccessKind::Read | AccessKind::Open(_) | AccessKind::Close(AccessMode::Read)
            )
        )
}

impl FileWatch {
    fn acquire(path: &Path) -> Result<Arc<Self>, String> {
        static FILES: OnceLock<Mutex<BTreeMap<PathBuf, Weak<FileWatch>>>> = OnceLock::new();
        let mut files = FILES
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| "ledger watch registry lock poisoned")?;
        if let Some(watch) = files.get(path).and_then(Weak::upgrade) {
            if watch.changed.borrow().is_ok() {
                return Ok(watch);
            }
        }
        files.retain(|_, file| file.strong_count() != 0);
        if !path.is_file() {
            return Err(format!(
                "ledger '{}' must exist before an interface reader starts",
                path.display()
            ));
        }
        let (changed, _) = watch::channel(Ok::<(), String>(()));
        let events = changed.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let change = match event {
                    Ok(event) if !ledger_event_reports_a_change(&event) => return,
                    Ok(_) => Ok(()),
                    Err(error) => Err(error.to_string()),
                };
                events.send_if_modified(|previous| {
                    if previous.is_err() {
                        return false;
                    }
                    *previous = change;
                    true
                });
            })
            .map_err(|error| {
                format!(
                    "ledger '{}' filesystem watcher cannot be created: {error}",
                    path.display()
                )
            })?;
        watcher
            .watch(path, RecursiveMode::NonRecursive)
            .map_err(|error| {
                format!(
                    "ledger file '{}' cannot be watched: {error}",
                    path.display()
                )
            })?;
        let file = Arc::new(Self {
            _watcher: watcher,
            changed,
        });
        files.insert(path.to_owned(), Arc::downgrade(&file));
        Ok(file)
    }
}

enum Subscription {
    File { _watch: Arc<FileWatch> },
    Host(HostedLedgerWake),
}

pub struct LedgerChangeWatch {
    source: Subscription,
    events: watch::Receiver<Result<(), String>>,
}

impl LedgerWakeSource {
    pub fn subscribe(&self, path: &Path) -> Result<LedgerChangeWatch, String> {
        match self {
            Self::Filesystem => {
                let file = FileWatch::acquire(path)?;
                Ok(LedgerChangeWatch {
                    events: file.changed.subscribe(),
                    source: Subscription::File { _watch: file },
                })
            }
            Self::Host(host) => Ok(LedgerChangeWatch {
                events: host.0.changed.subscribe(),
                source: Subscription::Host(host.clone()),
            }),
        }
    }
}

impl LedgerChangeWatch {
    async fn changed(&mut self) -> Result<(), String> {
        self.events.borrow().clone()?;
        if let Subscription::Host(host) = &self.source {
            if !self
                .events
                .has_changed()
                .map_err(|error| error.to_string())?
            {
                host.request();
            }
        }
        self.events
            .changed()
            .await
            .map_err(|error| error.to_string())?;
        self.events.borrow_and_update().clone()
    }

    pub async fn wait(&mut self, cancel: &CancellationToken, path: &Path) -> Result<(), String> {
        tokio::select! {
            _ = cancel.cancelled() => Ok(()),
            result = self.changed() => result.map_err(|error| format!("ledger '{}' wake failed: {error}", path.display())),
        }
    }
}

pub struct LedgerWakeHost<'a> {
    granted_path: Option<&'a Path>,
    watch: Option<LedgerChangeWatch>,
    waiting: bool,
}

impl<'a> LedgerWakeHost<'a> {
    pub fn new(granted_path: Option<&'a Path>) -> Self {
        Self {
            granted_path,
            watch: None,
            waiting: false,
        }
    }

    pub fn request(&mut self) -> Result<(), String> {
        let path = self
            .granted_path
            .ok_or("life requested a ledger wake without a ledger read handle")?;
        if self.waiting {
            return Err("life already has a pending ledger wait".into());
        }
        if self.watch.is_none() {
            let mut watch = LedgerWakeSource::Filesystem.subscribe(path)?;
            watch.events.mark_changed();
            self.watch = Some(watch);
        }
        self.waiting = true;
        Ok(())
    }

    pub fn is_waiting(&self) -> bool {
        self.waiting
    }

    pub async fn changed(&mut self) -> Result<(), String> {
        self.watch
            .as_mut()
            .ok_or("life has not subscribed to its ledger")?
            .changed()
            .await?;
        self.waiting = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reading_the_ledger_does_not_wake_its_readers_but_writes_and_rescans_do() {
        use notify::event::{AccessKind, AccessMode, Flag, ModifyKind};
        for (kind, expected) in [
            (
                notify::EventKind::Access(AccessKind::Open(AccessMode::Any)),
                false,
            ),
            (notify::EventKind::Access(AccessKind::Read), false),
            (
                notify::EventKind::Access(AccessKind::Close(AccessMode::Read)),
                false,
            ),
            (
                notify::EventKind::Access(AccessKind::Close(AccessMode::Write)),
                true,
            ),
            (notify::EventKind::Modify(ModifyKind::Any), true),
            (
                notify::EventKind::Remove(notify::event::RemoveKind::File),
                true,
            ),
        ] {
            assert_eq!(
                super::ledger_event_reports_a_change(&notify::Event::new(kind)),
                expected,
                "{kind:?}"
            );
        }
        let read = notify::EventKind::Access(AccessKind::Read);
        assert!(super::ledger_event_reports_a_change(
            &notify::Event::new(read).set_flag(Flag::Rescan)
        ));
    }
}
