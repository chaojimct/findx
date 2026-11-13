# t153 checkpoint repair: Stage A, not release approval

Baseline `3ea1c1b`, candidate 2.5.0. No running installation or production index changed.

## Implemented

Ticket, bounded-batch and FRN-observation producers mirror accepted metadata into the authoritative store under the namespace write lock. Coverage remains false until normal checks succeed; metadata errors are not cleared. A Roaring dirty-slot bitmap coalesces updates rather than accumulating per-file mutation records. Checkpoint capture snapshots packed metadata, structural mutations and volume watermarks under one namespace lock; the final metadata projection includes newly appended entries.

Periodic saves no longer wait for complete metadata coverage. Dirty checks include metadata-only changes; partial saves do not launch trigram rebuilds. Drained batches are acknowledged only after the whole writer succeeds. On error/unwind, records return ahead of concurrent mutations plus FullResync to repair from authoritative memory. Poisoned writer mutexes are recovered. Disk errors no longer terminate the USN consumer: an independent five-second retry backoff preserves the last-success clock and dirty state. Busy writers do not drain them.

Structured logs report IO success/failure timestamps, dirty/waiting/retry, readiness and explicitly `durable=false, transactional=false`. No new IPC/GUI health guarantee is claimed.

## Evidence and costs

Evidence: `target/t153-checkpoint-stage-a/`.

* `before-exact-3ea1c1b-partial-metadata.log`: exact baseline core, new public synthetic producer tests: three failures.
* `before-exact-legacy-writer-drain.log`: exact baseline persist_index body, temporary test-only harness and current core representation: one failure after real missing-segment IO loses a drained batch. Temporary edits were byte-restored, including unrelated work.
* `core-final.log`, `core-pinyin-final.log`, `service-final.log`: after-fix regressions, including periodic partial save, namespace/cursor consistency, new-entry metadata, IO failure/repair/retry, concurrent writes during failure AND success, busy deferral, unwind/poison recovery, restored search and preserved metadata errors.
* `stage-a-summary.json`: counts, resource measurements, artifact hash and boundaries.

The 1,048,577-file synthetic pass retains a 131,220-byte SERIALIZED dirty bitmap (not resident capacity), <=4096 active items and no per-update enum log. Checkpoint transient payload is exactly 20 bytes per distinct dirty slot: 20,971,540 bytes at that count, plus other batch overhead. Million-slot checkpoint IO peak was NOT measured. Delta payload writes 16 bytes per dirty slot plus manifest and filesystem amplification. Full repair still writes the full index under a read lock. No disk generations/backup growth were introduced.

## P0 handoff: transaction repair still mandatory

This is NOT power-failure recovery. Legacy v6 still overwrites committed prefixes, ignores some atomic-file sync failures, has a Windows remove-before-rename gap and separately publishes trigram pending state. The negative old-manifest/prefix test remains a reproduction of UNSAFETY, not a durability pass. All release JSON gates remain false.

Stage B must protect the old committed generation and atomically publish namespace, metadata, ALL sidecars and watermarks; propagate errors and prove every pre/post-publication boundary. Prefer immutable generation data with bounded redo/compaction over unmeasured multi-GB full clones every 30 seconds; select and prove the actual design next. Preserve old v5/v6 data read-only; migrate from a consistent isolated copy. Verify old readers/mappings and bounded app-owned generation reclamation, never real backups.

NOT done: actual short writes/ENOSPC/access/rename faults, CRC/missing-sidecar corruption recovery, process-kill transaction matrix, physical power loss, complete-volume recovery, consistent-copy transaction migration, or Stage C exe+all-sidecars+config+simulated SCM/Run rollback. Unix/macOS service paths were not validated. Windows MZ tests only; no process force-stopped, SCM/UAC/setup/deployment/push/tag/release used. The offline service executable is a developer candidate, not a release artifact or permission to replace engine39932.
