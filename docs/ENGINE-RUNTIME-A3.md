# Runtime lifecycle A3 — explicitly not a final upgrade receipt

Base: 22ddc451. The existing Windows CI44434459599 completed SUCCESS on 2026-11-19 09:14 Asia/Shanghai; it was received, not rerun.

## Actual integration
- The Windows service attaches a per-engine runtime owner before installing EngineSlot and launching producers. A dedicated actor holds the real borrowed FrozenCheckpoint; non-Send core guards never cross an IPC thread.
- Lifetimes now cover actual watch-loop batches, the real capture/write/pending-restore persister, metadata pipeline, before-spawn backfill/finalizer/trigram reservations, and queued-stat messages through actual metadata commit. Existing admitted descendants can finish while root ingress is sealed; admitting a new root cannot bypass the seal.
- Legacy watch batches flush before relinquishing ownership. A managed persister captured before prepare must finish before a frozen observation. Full snapshots mark FullResync without clearing RAM/dirty state: abort/partial publication cannot replay old structural mutations onto a full snapshot.
- STOP invalidates held frames and wakes rejected producer waiters, then joins watches and drains managed work before the one final global save. Save failure keeps the engine/RAM/IPC/SCM worker alive and retries; it does not force-exit or turn an error into success. A detected producer panic/failure deliberately retains a safe closed state, potentially StopPending, rather than claiming completion.
- Internal observations are explicitly upgrade_ack=false / process_stopped=false. Instance, request nonce, canonical index identity, RAM write sequence and actual durable published generation are evidence fields. Abort additionally matches the generation. Nonce ledger is bounded (4096 requests per owner; no eviction/replay acceptance). Cancellation after a prepared observation releases the actual fence; late callbacks cannot reuse an expired nonce. Deadline/cancellation cannot interrupt blocking OS IO and suppress success after late IO.

## Evidence boundary — do not mark release gates green
Real writer/file-fixture tests are NOT actual SCM/VHD/native USN acceptance. Metadata fetch in the pipeline test is deterministic; actual ticket admission/commit and actual v6 IO/readback are exercised. There is no native volume probing from these MZ tests. Positive Prepare/Commit upgrade IPC is still disabled and the actor's Prepare/Abort adapter remains internal/test-only. Old2.5.2 continues to be safely rejected, never upgraded to a protocol it does not implement.

Next required: make each source's normal Result failure atomic with its last permit release (not just panic/backfill error); prove actual native backfill/stat/rebuild cancellation and SCM StopPending with isolated Windows service/VHD/index copies; implement authenticated/versioned IPC state machine and final receipt only after those results, then installer consumes it. Complete-volume identity/cursor/USN historical gates remain independent. New whole payload and new installer matrix are required after engine changes; archived old12/12 is not evidence for this source. No tag/release is authorized by these intermediate tests alone.

## Safety
No format migration. No original checkout edits, GUI changes, official service/index/ProgramData access, UAC, installation, force push/history rewrite, or replacement of archived release assets. Original29 tracked +17 untracked protected files and GUI42816/snapshot65168 instance timestamps are verified separately before/after.
