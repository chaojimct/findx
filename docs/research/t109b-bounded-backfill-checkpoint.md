# t109b bounded Windows backfill checkpoint
2026-11-13 11:25 Asia/Shanghai. Local implementation milestone, NOT rollout permission.

## Production path changed
- Removed whole-volume VolPending vectors, needed/hit/FRN/ticket HashMaps and full-volume directory result collection from the Windows backfill path.
- Reusable 64KiB directory syscall buffer emits one bounded page; caller consumes each page before another IO. Uses existing scoped index identities, not a new per-file identity map. Every indexed directory is considered once, then one forward linear index fallback pass; no re-scan per work batch.
- Directory reads are deliberately serial for ordered page-observation stamps; any intervening engine write rejects the observation conservatively, leaving unknown entries to the fallback pass. This is a stated design tradeoff versus the former parallel full-result directory query. SSD/full-volume throughput is NOT measured or promised superior. Dedicated file-ID fallback pool is bounded to 1..4 workers, separate from search Rayon workers.
- Fallback active window is byte-charged (2MiB production default, 256B conservative per admitted item). A 32768-slot scan slice releases locks even if all slots are already known. Total files are never truncated by the window size.
- Metadata tickets use fallible allocation and byte-budget checks; a batch lease releases tickets on success, missing result, error, cancellation or epoch change. Capacity is reclaimed once at pass end/cancellation, not shrunk for each file.
- Backfill commits use the already-existing compact metadata overlay and do not retain N individual mutation log patches. Post-pass overlay drain is bounded to 8192 slots and releases store write locks between pages; Windows no longer takes a full overlay sidecar snapshot.
- metadata_ready is only finalized after compact root aggregates prove zero unknown coverage under the correct epoch. Missing/failed/conflicted entries leave readiness false with an explicit retry-on-restart/rebuild message. No unsuccessful attempts count as committed coverage.
- A failed volume-open is remembered for the pass, not retried for every directory. No real volume calls were executed in validation.

## Focused verification already real
- Existing 16 core usage regressions passed after the first ticket/overlay changes.
- cargo check --locked -p findx2-service passed with the production streaming/scheduler wiring.
- Shared production scheduler tests: 4 passed, covering fake producer scale, IO error, cancellation, epoch switch, active budget failure, incomplete readiness and stale directory observation vs namespace metadata.
- 512-file unique temporary public directory through actual GetFileInformationByHandleEx streaming reader: passed, multiple fixed-size pages; no volume, journal or MFT handle access. Temporary files removed.

## Actual process measurements (fake producer, existing core/index/overlay excluded via baseline)
The same active 1MiB charge budget was used for both sizes. This charge is NOT an OS Private/WS cap.

| Files fully processed | Batches | Peak active items | Peak ticket capacity | Ticket capacity charge | Retained ticket bytes | Private delta | WS delta | sampled peak Private delta | sampled peak WS delta |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 65537 | 17 | 4096 | 7168 | 229376 | 0 | 323584 | 167936 | 663552 | 811008 |
| 1048577 | 257 | 4096 | 14336 | 458752 | 0 | 794624 | 495616 | 1015808 | 1073152 |

All file results committed; no N-sized mutation log remained. Observed HashMap spare/tombstone capacity differs between sizes, but stays work-window-sized, not proportional to the 16x corpus increase. Sample interval 1ms. Debug elapsed 82ms / 1571ms, NOT real disk throughput. Baseline included the existing synthetic index, compact tree and fixed metadata overlay. Raw evidence target/backfill-t109-memory.txt. Before focused tests physical free memory was about 6.39GiB; no memory-pressure stress.

## Pending at this checkpoint
- Full final locked core/service/IPC/native regressions, independent t109b build and finite actual-pipe integration must finish before giving final artifact hashes.
- Review final readiness/persistence concurrency and platform reader error paths. Existing Unix unversioned producer is unchanged; no cross-platform all-OOM claim.
- No real USN/MFT/full-volume test, no official index/baseline access, no service switch or deployment. Formal FindX2Search and other existing service processes remain outside this task.
