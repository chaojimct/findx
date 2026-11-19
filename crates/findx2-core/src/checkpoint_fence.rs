//! Central admission for every authoritative store write. This is NOT a runtime
//! producer-thread drain or an installer ACK. The service must integrate those
//! before enabling durable_prepare. No on-disk index format changes.
use crate::{IndexStore, SearchEngine};
use parking_lot::{Condvar, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::{
    ops::{Deref, DerefMut},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[derive(Default)]
struct Admission {
    closed: bool,
    active: usize,
    sequence: u64,
    overflowed: bool,
}
#[derive(Default)]
struct Gate {
    state: Mutex<Admission>,
    changed: Condvar,
}
pub(crate) struct AdmissionStore<T> {
    inner: RwLock<T>,
    gate: Gate,
}
impl<T> AdmissionStore<T> {
    pub(crate) fn new(value: T) -> Self {
        Self {
            inner: RwLock::new(value),
            gate: Gate::default(),
        }
    }
    pub(crate) fn read(&self) -> RwLockReadGuard<'_, T> {
        self.inner.read()
    }
    pub(crate) fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        self.inner.try_read()
    }
    pub(crate) fn write(&self) -> TrackedWriteGuard<'_, T> {
        let mut s = self.gate.state.lock();
        while s.closed {
            self.gate.changed.wait(&mut s);
        }
        s.active += 1;
        drop(s);
        let permit = WritePermit { gate: &self.gate };
        TrackedWriteGuard {
            guard: self.inner.write(),
            _permit: permit,
        }
    }
    fn close(&self, deadline: Instant, cancel: &AtomicBool) -> Result<ClosedAdmission<'_>, String> {
        let mut s = self.gate.state.lock();
        if s.closed {
            return Err("another checkpoint already owns admission".into());
        }
        s.closed = true;
        self.gate.changed.notify_all();
        loop {
            if cancel.load(Ordering::Acquire) || Instant::now() >= deadline || s.overflowed {
                s.closed = false;
                self.gate.changed.notify_all();
                return Err(
                    "checkpoint admission cancelled, timed out or sequence exhausted; no ACK"
                        .into(),
                );
            }
            if s.active == 0 {
                return Ok(ClosedAdmission {
                    gate: &self.gate,
                    sequence: s.sequence,
                });
            }
            self.gate.changed.wait_for(&mut s, Duration::from_millis(5));
        }
    }
}
struct WritePermit<'a> {
    gate: &'a Gate,
}
impl Drop for WritePermit<'_> {
    fn drop(&mut self) {
        let mut s = self.gate.state.lock();
        s.active -= 1;
        match s.sequence.checked_add(1) {
            Some(v) => s.sequence = v,
            None => s.overflowed = true,
        }
        self.gate.changed.notify_all();
    }
}
/// Raw mutable callers cannot bypass sequence/admission accounting.
pub struct TrackedWriteGuard<'a, T> {
    guard: RwLockWriteGuard<'a, T>,
    _permit: WritePermit<'a>,
}
impl<T> Deref for TrackedWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}
impl<T> DerefMut for TrackedWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}
struct ClosedAdmission<'a> {
    gate: &'a Gate,
    sequence: u64,
}
impl Drop for ClosedAdmission<'_> {
    fn drop(&mut self) {
        let mut s = self.gate.state.lock();
        s.closed = false;
        self.gate.changed.notify_all();
    }
}

pub struct FreezeRequest {
    pub protocol_version: u32,
    pub expected_instance: String,
    pub nonce: String,
}
/// Serializes only explicit non-ACK evidence. Fields cannot be fabricated by callers.
#[derive(Debug, serde::Serialize)]
pub struct FrozenCheckpointEvidence {
    protocol_version: u32,
    engine_instance: String,
    request_nonce: String,
    index_identity: String,
    mutation_sequence: u64,
    published_generation: u64,
    metadata_ready: bool,
    upgrade_ack: bool,
    runtime_drained: bool,
    process_stopped: bool,
    scope: &'static str,
}
/// Holds BOTH namespace and publication exclusion until dropped/aborted.
/// No producer can alter authoritative RAM or a v6 publisher replace this generation
/// while this value lives. Pending pre-captured batches/runtime threads are NOT drained.
pub struct FrozenCheckpoint<'a> {
    // Release publication exclusion before namespace/admission on abort.
    _publication: crate::persist_tx::PublicationLease,
    _store: RwLockWriteGuard<'a, IndexStore>,
    _admission: ClosedAdmission<'a>,
    evidence: FrozenCheckpointEvidence,
}
impl FrozenCheckpoint<'_> {
    pub fn evidence(&self) -> &FrozenCheckpointEvidence {
        &self.evidence
    }
}

impl SearchEngine {
    pub fn checkpoint_instance(&self) -> Result<String, String> {
        self.checkpoint_instance
            .as_ref()
            .map(|v| v.iter().map(|b| format!("{b:02x}")).collect())
            .ok_or_else(|| "OS instance entropy unavailable; no checkpoint receipt".into())
    }
    /// First product primitive for the future prepare state machine, not its ACK.
    /// RAM and dirty bookkeeping are NEVER drained/cleared here, including errors.
    /// Blocking OS writes cannot be force-cancelled: deadline/cancel is rechecked
    /// after IO and suppresses evidence even if a generation was already published.
    pub fn freeze_checkpoint(
        &self,
        index: &Path,
        request: FreezeRequest,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<FrozenCheckpoint<'_>, String> {
        self.freeze_checkpoint_inner(index, request, deadline, cancel, || {})
    }
    fn freeze_checkpoint_inner(
        &self,
        index: &Path,
        request: FreezeRequest,
        deadline: Instant,
        cancel: &AtomicBool,
        after_publish: impl FnOnce(),
    ) -> Result<FrozenCheckpoint<'_>, String> {
        let instance = self.checkpoint_instance()?;
        if request.protocol_version != 1
            || request.expected_instance != instance
            || request.nonce.len() < 32
            || request.nonce.len() > 128
            || !request
                .nonce
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(
                "unsupported version, wrong instance or invalid request nonce; no ACK".into(),
            );
        }
        let parent = index
            .parent()
            .ok_or("index has no parent")?
            .canonicalize()
            .map_err(|e| format!("index identity: {e}"))?;
        let identity = parent.join(index.file_name().ok_or("index has no filename")?);
        let admission = self.store.close(deadline, cancel)?;
        let store = self
            .store
            .inner
            .try_write_until(deadline)
            .ok_or("namespace drain timed out; no ACK")?;
        let publication = crate::persist_tx::PublicationLease::acquire(deadline, cancel)?;
        if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err("checkpoint cancelled/timed out before IO; no ACK".into());
        }
        let layout = crate::V6Layout::for_index(&identity);
        let generation = publication
            .save_full(&layout, &store)
            .map_err(|e| format!("checkpoint writer: {e}"))?;
        after_publish();
        if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(
                "checkpoint cancelled/timed out after IO; generation may be durable, but no ACK"
                    .into(),
            );
        }
        let pointer = crate::read_published(&layout)
            .map_err(|e| format!("checkpoint published pointer: {e}"))?
            .ok_or("writer did not publish a pointer")?;
        if pointer.manifest.generation != generation {
            return Err("actual writer/published generation mismatch; no ACK".into());
        }
        let evidence = FrozenCheckpointEvidence {
            protocol_version: 1,
            engine_instance: instance,
            request_nonce: request.nonce,
            index_identity: identity.to_string_lossy().into_owned(),
            mutation_sequence: admission.sequence,
            published_generation: generation,
            metadata_ready: store.metadata_ready,
            upgrade_ack: false,
            runtime_drained: false,
            process_stopped: false,
            scope: "frozen-authoritative-store-only",
        };
        Ok(FrozenCheckpoint {
            _publication: publication,
            _store: store,
            _admission: admission,
            evidence,
        })
    }
}

#[cfg(test)]
mod checkpoint_fence_tests {
    use super::*;
    use std::sync::{mpsc, Arc};
    fn fixture(label: &str) -> (std::path::PathBuf, Arc<SearchEngine>) {
        let dir = std::env::temp_dir().join(format!(
            "findx-checkpoint-fence-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = |id, parent, name: &str, dir| crate::RawEntry {
            file_id: id,
            file_id_128: None,
            parent_id: parent,
            name: name.into(),
            size: if dir { 0 } else { 10 },
            mtime: 0,
            ctime: 0,
            attrs: if dir { 0x10 } else { 0 },
            is_dir: dir,
        };
        let mut s = crate::IndexBuilder::new(b'F', 1, 7, 100)
            .build_from_raw(
                vec![raw(1, 100, "public.txt", false)],
                vec![raw(100, 0, "public-root", true)],
                true,
            )
            .unwrap();
        s.metadata_ready = false;
        s.set_track_mutations(true);
        let path = dir.join("index.bin");
        crate::v6_save_full(&crate::V6Layout::for_index(&path), &s).unwrap();
        let e = Arc::new(SearchEngine::new(s));
        e.enable_usage(16 * 1024 * 1024).unwrap();
        (path, e)
    }
    fn request(e: &SearchEngine) -> FreezeRequest {
        FreezeRequest {
            protocol_version: 1,
            expected_instance: e.checkpoint_instance().unwrap(),
            nonce: "0123456789abcdef0123456789abcdef".into(),
        }
    }
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(2)
    }
    #[test]
    fn checkpoint_fence_actual_generation_instance_nonce_and_raw_writer_exclusion() {
        let (path, e) = fixture("binding");
        {
            let mut g = e.index_store_mut();
            g.entries[0].size = 456;
        }
        let cancel = AtomicBool::new(false);
        let frozen = e
            .freeze_checkpoint(&path, request(&e), deadline(), &cancel)
            .unwrap();
        assert_eq!(frozen.evidence.mutation_sequence, 1);
        assert!(!frozen.evidence.upgrade_ack);
        assert!(!frozen.evidence.metadata_ready);
        assert!(!frozen.evidence.process_stopped);
        assert_eq!(
            frozen.evidence.engine_instance,
            e.checkpoint_instance().unwrap()
        );
        assert_eq!(frozen.evidence.request_nonce, request(&e).nonce);
        let layout = crate::V6Layout::for_index(&path);
        assert_eq!(
            crate::read_published(&layout)
                .unwrap()
                .unwrap()
                .manifest
                .generation,
            frozen.evidence.published_generation
        );
        assert_eq!(crate::v6_load(&layout).unwrap().entries[0].size, 456);
        let (tx, rx) = mpsc::channel();
        let w = e.clone();
        let handle = std::thread::spawn(move || {
            w.index_store_mut().entries[0].size = 789;
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
        drop(frozen);
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        handle.join().unwrap();
        assert_eq!(e.index_store().entries[0].size, 789);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn checkpoint_fence_true_writer_fault_stages_keep_ram_and_retry() {
        for (label, fault, expected) in [
            (
                "quota",
                crate::PersistFault::DiskFull { max_bytes: 0 },
                "disk full (injected quota",
            ),
            (
                "short",
                crate::PersistFault::ShortWrite {
                    file: "entries.dat".into(),
                    max_bytes: 0,
                },
                "injected short write",
            ),
            (
                "access",
                crate::PersistFault::AccessDenied {
                    file: "entries.dat".into(),
                },
                "injected access failure",
            ),
            (
                "sync",
                crate::PersistFault::AfterDataSync,
                "after data sync",
            ),
            (
                "prepublish",
                crate::PersistFault::BeforePublish,
                "before publish",
            ),
            ("rename", crate::PersistFault::RenameFail, "rename failure"),
        ] {
            let (path, e) = fixture(label);
            {
                e.index_store_mut().entries[0].size = 456;
            }
            let layout = crate::V6Layout::for_index(&path);
            let before = std::fs::read(crate::published_path(&layout)).unwrap();
            crate::set_persist_fault(fault);
            let result =
                e.freeze_checkpoint(&path, request(&e), deadline(), &AtomicBool::new(false));
            crate::clear_persist_fault();
            let error = match result {
                Err(e) => e,
                Ok(_) => panic!("fault produced evidence"),
            };
            assert!(error.contains(expected), "wrong stage {label}: {error}");
            assert_eq!(e.index_store().entries[0].size, 456);
            assert_eq!(
                std::fs::read(crate::published_path(&layout)).unwrap(),
                before
            );
            let f = e
                .freeze_checkpoint(&path, request(&e), deadline(), &AtomicBool::new(false))
                .unwrap();
            assert_eq!(crate::v6_load(&layout).unwrap().entries[0].size, 456);
            drop(f);
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }
    #[test]
    fn checkpoint_fence_cancel_timeout_and_instance_change_never_clear_ram() {
        let (path, e) = fixture("cancel");
        e.index_store_mut().entries[0].size = 456;
        let yes = AtomicBool::new(true);
        assert!(e
            .freeze_checkpoint(&path, request(&e), deadline(), &yes)
            .is_err());
        assert!(e
            .freeze_checkpoint(&path, request(&e), Instant::now(), &AtomicBool::new(false))
            .is_err());
        let mut req = request(&e);
        req.expected_instance = "stale-instance".into();
        assert!(e
            .freeze_checkpoint(&path, req, deadline(), &AtomicBool::new(false))
            .is_err());
        assert_eq!(e.index_store().entries[0].size, 456);
        let f = e
            .freeze_checkpoint(&path, request(&e), deadline(), &AtomicBool::new(false))
            .unwrap();
        drop(f);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn checkpoint_fence_admitted_writer_drain_timeout_then_retry() {
        let (path, e) = fixture("drain");
        let held = e.index_store_mut();
        assert!(e
            .freeze_checkpoint(
                &path,
                request(&e),
                Instant::now() + Duration::from_millis(15),
                &AtomicBool::new(false)
            )
            .is_err());
        drop(held);
        let f = e
            .freeze_checkpoint(&path, request(&e), deadline(), &AtomicBool::new(false))
            .unwrap();
        drop(f);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn checkpoint_fence_postpublish_cancel_and_deadline_keep_committed_ram_without_ack() {
        let (path, e) = fixture("postpublish");
        e.index_store_mut().entries[0].size = 456;
        let cancel = AtomicBool::new(false);
        let result = e.freeze_checkpoint_inner(&path, request(&e), deadline(), &cancel, || {
            cancel.store(true, Ordering::Release)
        });
        assert!(matches!(result,Err(ref x) if x.contains("after IO")));
        let layout = crate::V6Layout::for_index(&path);
        assert_eq!(crate::v6_load(&layout).unwrap().entries[0].size, 456);
        assert_eq!(e.index_store().entries[0].size, 456);
        let cancel = AtomicBool::new(false);
        let result = e.freeze_checkpoint_inner(
            &path,
            request(&e),
            Instant::now() + Duration::from_millis(100),
            &cancel,
            || std::thread::sleep(Duration::from_millis(110)),
        );
        assert!(matches!(result,Err(ref x) if x.contains("after IO")));
        assert_eq!(e.index_store().entries[0].size, 456);
        e.index_store_mut().entries[0].size = 789;
        let other = SearchEngine::new(crate::v6_load(&layout).unwrap());
        assert_ne!(
            e.checkpoint_instance().unwrap(),
            other.checkpoint_instance().unwrap()
        );
        assert!(other
            .freeze_checkpoint(&path, request(&e), deadline(), &cancel)
            .is_err());
        let f = e
            .freeze_checkpoint(&path, request(&e), deadline(), &cancel)
            .unwrap();
        assert_eq!(crate::v6_load(&layout).unwrap().entries[0].size, 789);
        drop(f);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn checkpoint_fence_drains_admitted_write_and_blocks_real_metadata_producer() {
        let (path, e) = fixture("metadata");
        let ticket = e.metadata_ticket_by_id(b'F', 1).unwrap();
        let held = e.index_store_mut();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (abort_tx, abort_rx) = mpsc::channel();
        let worker = e.clone();
        let wp = path.clone();
        let freezer = std::thread::spawn(move || {
            let f = worker
                .freeze_checkpoint(&wp, request(&worker), deadline(), &AtomicBool::new(false))
                .unwrap();
            ready_tx.send(()).unwrap();
            abort_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            drop(f);
        });
        {
            let mut state = e.store.gate.state.lock();
            let until = deadline();
            while !state.closed {
                assert!(Instant::now() < until);
                e.store
                    .gate
                    .changed
                    .wait_for(&mut state, Duration::from_millis(5));
            }
        }
        let (tx, rx) = mpsc::channel();
        let writer = e.clone();
        let metadata = std::thread::spawn(move || {
            assert!(writer.commit_metadata(ticket, 789, 0, 0).unwrap());
            tx.send(()).unwrap();
        });
        assert!(
            ready_rx.recv_timeout(Duration::from_millis(20)).is_err(),
            "must drain already-admitted raw writer first"
        );
        drop(held);
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(20)).is_err(),
            "late metadata cannot enter frozen snapshot"
        );
        abort_tx.send(()).unwrap();
        freezer.join().unwrap();
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        metadata.join().unwrap();
        assert_eq!(e.index_store().entries[0].size, 789);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn checkpoint_fence_pins_all_normal_v6_publishers_and_wait_timeout_is_not_success() {
        let (path, e) = fixture("publication");
        let cancel = AtomicBool::new(false);
        let lease = crate::persist_tx::PublicationLease::acquire(deadline(), &cancel).unwrap();
        assert!(e
            .freeze_checkpoint(
                &path,
                request(&e),
                Instant::now() + Duration::from_millis(15),
                &cancel
            )
            .is_err());
        drop(lease);
        let f = e
            .freeze_checkpoint(&path, request(&e), deadline(), &cancel)
            .unwrap();
        let layout = crate::V6Layout::for_index(&path);
        let loaded = crate::v6_load(&layout).unwrap();
        let (tx, rx) = mpsc::channel();
        let other = std::thread::spawn(move || {
            let gen = crate::v6_save_full(&layout, &loaded).unwrap();
            tx.send(gen).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
        drop(f);
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        other.join().unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
