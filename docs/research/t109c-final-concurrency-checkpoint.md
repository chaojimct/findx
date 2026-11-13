# t109c final concurrency and bounded-backfill checkpoint
2026-11-13 Asia/Shanghai. Local development only, NOT production rollout.

## Correctness fixes after cfc82fb
- Rebuild prepares its merged store and journal resume state offline, then atomically replaces slot identities, revokes the old compact runtime/tickets, clears old overlay bindings and bumps revision under the store write lock. No swap-then-invalidate gap. Removed the later redundant clear which could erase new-epoch metadata.
- A scoped, reference-counted metadata drain mirrors late ticket/directory/namespace metadata writes into the store. Ready is published only after bounded flush and a second atomic epoch/error/unknown-coverage check. Dirty coverage or epoch replacement cannot incorrectly publish completion. Failure drops the scoped guard. Readiness changes explicitly retain FullResync persistence work even with no overlay records.
- Four deterministic finite regressions cover slot reuse (including same FRN), old tickets/observations/finalizers, replacement between 8192-slot pages, late dirty coverage, late ticket+namespace writes and guard release. The page callback is an internal generic no-op in production, not a global injection flag.
- Final persistence distinguishes a concurrent-commit deferral from success and reports actual failure. An obsolete finalizer does not remove a new epoch's sidecar or claim current completion. Persisted snapshots may legitimately lag the newest in-memory namespace; normal FullResync tracking retains the replacement for the next commit.
- Streaming directory decoder now requires a complete aligned header, even/nonzero bounded UTF-16 name length and nonoverlapping forward links. Synthetic malformed/short pages are rejected without publishing a prefix. Real directory fixture remains restricted to a unique temporary 512-file public directory.

## Verified suites
- Previous cfc82fb chain (terminal 220004): core 74 passed/1 ignored, service 14 unit + 1 loading IPC passed, native 178 passed/1 ignored, IPC 2 passed; independent t109b debug build and finite actual-pipe/native-render-model integration passed (84ms delayed metadata, 4 model nodes). Those binaries are NOT the final t109c binaries.
- Post-fix locked serial chain (terminal 220007): core 62 passed/1 ignored without pinyin feature; service 14 unit + 1 loading IPC passed (117.11s); native 178 passed/1 ignored; IPC 2 passed. Its final package-name command was misspelled findx2-platform-windows, a command error rather than a test failure. Terminal 220008 passed the correct findx2-windows package (7 tests) plus all 16 pinyin feature tests separately: current core coverage totals 78 passed/1 ignored. The existing Windows suite also queried read-only storage seek-penalty properties on C:/D:/E: using zero-access device handles; this is not USN/MFT access, directory traversal or a volume scan.
- Final independent builds and actual-pipe fixture use target/native-usage-t109c only. Exact source commit and executable SHA256 belong in that directory's build-manifest.json and the final handoff; no old hash should be reused.

## Latest actual scheduler measurement, post-fix serial service tests
Same production ticket_pass scheduler with fake metadata producer, 1MiB active charge. Existing synthetic entries, compact tree and fixed overlay are in the baseline. This is not a total-engine or disk-throughput measurement.

| processed files | batches | peak active items | peak ticket capacity | ticket byte charge | retained ticket bytes | steady private delta B | steady WS delta B | sampled peak private delta B | sampled peak WS delta B |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
|65537|17|4096|7168|229376|0|311296|167936|655360|811008|
|1048577|257|4096|14336|458752|0|790528|344064|999424|856064|

Both attempted and committed counts equal processed counts. Scan coverage is not truncated. HashMap spare/tombstone capacities differ but remain window-sized across a 16x corpus increase. Peak samples are at 1ms, not an exact OS allocation high-water mark; allocator/process noise affects deltas. Debug elapsed 88ms / 1416ms is not storage speed. Raw append-only evidence: target/backfill-t109-memory.txt. Earlier run values are retained in t109b notes, not silently replaced.

Earlier independent compact-array measurement: N=6160274,D=719923, payload 96897333B, private delta 97091584B, WS delta 96927744B. Actual full enable_usage on quarter-scale representative-ratio subset N=1540069,D=179981: payload 24224450B, private delta 26288128B, WS delta 25882624B, sampled initialization peaks equal those deltas. Neither figure is the complete process footprint. Old 886.64MiB layout remains an estimate, not a measured legacy runtime comparison.

## Explicit remaining boundaries
- No real USN/MFT/full-volume/backfill benchmark, no official index/baseline read or write, no real-library-copy validation, no formal service/GUI operation, deployment or push. Full-volume superiority and production-readiness are not claimed.
- Primary directory enumeration is deliberately serial to bound memory and preserve observation ordering; it may regress SSD throughput versus the former parallel full-result collector. Search has a separate pool; real search latency under storage load still needs controlled copy validation.
- Failed/conflicted metadata remains unknown with visible retry-on-restart/rebuild coverage. There is no autonomous endless retry. Active work is bounded, not total coverage. Synchronous Windows IO is not forcibly interrupted; an epoch switch revokes commits immediately but an in-flight syscall/window can live until IO returns.
- Byte charging is not a hard OS memory cap; standard containers/bitmap internals are not made universally OOM-proof. The Unix unversioned producer is not upgraded by this Windows work.
- Prior native parallel run's 14 AllocFailed remains unresolved/resource-sensitive; serial 178 tests pass without bypassing the real pressure guard. Not a proven shared fault-injection race.
- The finite pipe integration exercises actual IpcClient and the exact production GUI render-model conversion, not visual screenshot acceptance or the user's running GUI. Existing UI animations and local measurement entry remain preserved.
- Terminal-persistence deferral is visible and left to regular persistence/restart; its informational backfill message may remain until a new pass. Cursor/manifest correctness uses the existing atomic snapshots and retained FullResync, not a claim that every recent metadata result is already durable.

Next step can be an explicitly authorized controlled real-library copy validation after checking headroom, with isolated pipe, strict no-watch/no-backfill mode first and snapshot/restart checks. It is NOT authorized or executed here. Real USN recovery, storage throughput and resource-pressure acceptance remain deployment gates.
