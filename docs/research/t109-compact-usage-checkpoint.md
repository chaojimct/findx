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

## 2026-11-13 11:13 Asia/Shanghai: correctness and isolated integration milestone

Additional fixes:
- Namespace metadata is now staged and published to MetaOverlay only after namespace success. It overrides earlier overlay values; metadata-only partial updates preserve other effective fields. Rejected prefixes cannot leak overlay writes. Any committed namespace event invalidates an outstanding metadata ticket, including same-size/time-only updates.
- Excluded identities have an explicit 4MiB capacity-charge resource guard and fallible allocation; resource exhaustion fails the transaction and retains the cursor, never silently drops events.
- Metadata-only batches no longer pre-reserve growing FRN/directory overlays. Identity undo remains O(batch), not a clone of the historical overlay.
- Production watch uses NamespaceGate, also exercised through a fake channel-driven watcher. Failed prefix + subsequent event + heartbeat stay fenced and unhealthy; explicit reconstructed gate can replay/recover.
- Unique temporary V6 crash fixture commits resolved data, then restores the exact previous manifest to simulate data written before manifest publication. Reload retains the old cursor, safely replays pending events, and recovers one correct file. This is an on-disk tail/manifest fault injection, NOT OS/power-loss testing.
- Native GUI indexed-page conversion was extracted unchanged into indexed_usage::scan_from_page and remains on the production UI path. Local actual-allocation scan and all prior UI behavior remain intact.
- Strict no-watch fixtures reject the formal pipe and no longer clear or overwrite formal shared startup diagnostics, including startup errors. Fixture event script can synthesize delayed metadata with the pending operation.

Completed locked verification:
- core full package (service feature union): 74 passed, 1 explicit memory experiment ignored by default (that experiment was run successfully separately).
- service: 10 unit + 1 integration passed; unit runtime 79.19s in the final run.
- native GUI: 178 passed, 1 ignored; IPC: 2 passed, serial test execution.
- latest service + native GUI dev builds succeeded.
- finite real named-pipe fixture: explicit temporary synthetic tree, --no-watch --no-backfill --no-everything-ipc, 32MiB usage budget, unique non-formal pipe; native production IpcClient + actual UI render-model mapper. Initial logical 33B/2 files, delayed create visible with unknown metadata, then 77B/3 files/unknown=0. Stale generation rejected; allocated stays 0. Final observed pending visibility 107ms and metadata visibility 107ms (fixture polls every 100ms, NOT production USN latency). Only four render-model nodes allocated. Fixture service exited and its temporary files were removed.
- This verifies native client/render-model integration, NOT an interactive pixel/screenshot visual acceptance test. No user's GUI process was launched or modified.

Final development artifacts under target/native-usage-t109:
- findx2-service.exe SHA256 7D1EBBA2D899F7AF6E29F6CC3C7912E54129775D65D247C864782EFE5DA8AE12
- findx2-gui-native.exe SHA256 9A921D49F417D921D3BA767A0F0C6A27AA0A202307F7F2DB869E29584AA445B5

Evidence logs on MZ: terminals/219997.txt (core/service), terminals/219999.txt (GUI/IPC/build/real-pipe integration). Prior 219990 also completed successfully but its artifact hashes are superseded.

### Important remaining rollout conditions / precise limitations
- No all-volume performance, real USN, MFT or production-index test has been performed. No rollout approval.
- Earlier suspicion of GUI shared fault-injection race was NOT substantiated: source has a real GlobalMemoryStatusEx pressure guard (97% load or <64MiB free), and allocation-failure tests attempt actual fallible reservations, not a shared injector. Keep the initial parallel 14 AllocFailed failures as an unresolved resource-sensitive test boundary; do not bypass production memory protection. Serial suite is green.
- Final audit found the existing Windows full-volume backfill still constructs whole-pending-set maps, and metadata ticket issuance can grow UsageRuntime.tickets for that whole pass; HashMap capacity can remain after removals. The initialization-only measurements above do NOT cover enabled backfill transient/retained maps. Before claiming bounded service memory with backfill enabled, bound/chunk the in-flight ticket lifetime and audit existing backfill temporaries. These were not exercised against real volumes and were not silently counted in the 92.41MiB payload.
- Unticketed compatibility overlay-write API cannot infer whether externally supplied metadata was observed before or after a mutation; Windows production backfill/stat paths use generation-scoped tickets. Unix/unversioned producer behavior remains outside this Windows verification.
- All container/OOM recovery is not guaranteed (event clones/bitmap internals and some temporaries still use infallible allocation). Explicit try_reserve/resource paths covered by tests do fail without acknowledging a namespace prefix.
- Missing-parent queues may require explicit recovery/restart/rebuild if the parent never becomes resolvable. The service remains visibly degraded with a retained restart cursor; it must not pretend such a queue is healthy or complete. Excluded identity cache itself is runtime-only.
- Legacy runtime same-condition comparison remains unmeasured. Do not replace the legacy layout estimate with a fabricated process measurement.

11:12 Asia/Shanghai read-only check: FindX2Search still Running, PID7428; another existing Program Files service process PID19600 was also observed, not touched or attributed. No isolated fixture processes remained. Formal index/baseline/deployment paths untouched.
