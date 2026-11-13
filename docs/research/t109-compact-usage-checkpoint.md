# t109 compact usage implementation checkpoint (2026-11-13 11:01 Asia/Shanghai)

Local-only checkpoint, NOT formal rollout approval. No formal index or deployment accessed.

## Implementation
- 64-byte directory aggregates; 8-byte entry-aligned links; 2-bit freshness.
- Namespace state reuses FileEntry/name/FRN/parent indexes. Scoped FRN maps separate volumes.
- Zero-name metadata changes no longer grow the name arena.
- Namespace rollback retains per-batch identity undo (no full overlay clone) and touched slot before-images; removes appended bitmap members.
- Deferred usage folding uses before/after namespace snapshots in journal order, including repeated same-identity mutations.
- Missing parents use a bounded ordered 4MiB queue. While it is nonempty the durable cursor stays pinned, including heartbeats, so restart can replay the lost in-memory queue. Idle heartbeats do not retry pending events.
- Failed watch namespace batches fence later events/cursors until restart/rebuild, rather than silently acknowledging a later prefix. Search remains available with visible watch health failure.
- Usage initialization failure is visible and does not abort search; reinitialization releases the previous sidecar before building the next one.
- Directory initialization topology staging uses fallible allocations. Native GUI source unchanged.

## Verification recorded so far
- Earlier core full locked package: 53 passed. After audit changes: 14 namespace/usage tests passed; full package needs rerun on final source.
- Service full locked package: 9 unit + 1 integration passed (143s unit tests); after watcher audit, IPC 2 tests passed; final full package rerun pending.
- Native GUI locked lib tests with --test-threads=1: 178 passed, 1 ignored. IPC: 2 passed.
- Initial GUI parallel run: 164 passed, 14 AllocFailed failures. Serial isolation passed; likely shared fault-injection state. Do not claim parallel suite clean.
- Isolated EngineSlot only; no real watcher or volume scan launched.

## Windows process memory measurements
All numbers are bytes. Additional payload capacity is not process memory.

| Synthetic experiment | Payload capacity | Private delta | WS delta |
|---|---:|---:|---:|
| Full-scale arrays N=6160274 D=719923 | 96897333 | 97091584 | 96927744 |
| 2000018 files, actual enable_usage, 1 directory | 16500509 | 16556032 | 16592896 |
| Representative quarter N=1540069 D=179981, actual enable_usage | 24224450 | 26288128 | 25882624 |

Quarter-scale initialization: 307ms debug, not production/full-volume latency. A 1ms process sampler observed peak Private delta=26288128 and peak WS delta=25882624 (same as retained steady measurements). Allocator can retain freed staging pages. Directory/link len==capacity in this run. Quarter-scale is an explained subset with approximately the actual directory ratio, not a measured full-scale engine.

Before experiment: physical total 32829940 KiB, free 3969160 KiB. The selected subset stays well below free RAM. No old heavy implementation was run. Prior ~886.64MiB legacy layout remains a model, not process measurement.

Raw evidence: target/usage-t109-memory.txt (synthetic counts only).

## Remaining audit / validation
- Complete all final-source locked tests/build; rebuild service after final checkpoint source changes.
- Bounded queue byte budget still uses event-cost estimates. More fallible coverage needed for event cloning, bitmap internals and all transaction temporaries; Rust infallible allocation/OOM is NOT claimed recoverable.
- Excluded identity set needs explicit resource policy; excluded rollback currently snapshots that set once per batch.
- Pending queue has no separate persistence: safety depends on pinned watermark and idempotent replay; test simulates restart from namespace clone, not crash-injection of on-disk journal/manifest commits.
- Review overlay metadata precedence when namespace metadata changes an already-overlaid entry.
- Add fake watcher fence regression (current EngineSlot health test is not full watcher simulation).
- Old implementation same-condition runtime comparison is not yet measured.
- Do not declare whole-volume performance or readiness to switch the formal service.

Build output directory: target/native-usage-t109; existing t97/t100 and Program Files outputs untouched. Current artifacts are development builds; never launched. Service must be refreshed after the latest checkpoint changes.
