# Engine logical usage: phase-A checkpoint

This implements a production-connected Windows logical usage path, not an allocated-space replacement and not a cfg(test)-only experiment. Existing local measured-allocation views remain an explicit separate choice.

## Implemented

- Engine-owned compact directory aggregates and child adjacency; canonical indexed-path logical bytes/counts, with unverified metadata counted explicitly.
- Volume-qualified identity lookups within a process/store epoch; direct scoped mutation targets prevent global u64 FRN aliases from selecting another volume. Merge rebases root-parent indexes; runtime can derive authoritative parents from persisted directory entries.
- Initial O(N+D) topology aggregation; create/delete/metadata/file moves and directory-subtree moves update cached ancestor sums. No per-mutation tree rebuild or full-path copies.
- Whole namespace batches precede their journal cursor; failures do not acknowledge the failed end cursor. Missing-parent dependencies within a batch can be replayed. Rebuild changes epoch; guarded metadata tickets reject stale IO and dirty-sequence mismatches. Overlay flush does not add bytes twice.
- Watch transport and stat queue bounded by journal-frame/queue resources; pending stat no longer silently drops at a hard file count. Memory growth/preflight uses byte budgets and try_reserve on principal contiguous structures. Default logical-tree extra budget is one quarter of currently available physical RAM, not a node count; --usage-memory-mib is an explicit resource override. Additional tree/staging budget is not total process RSS.
- UsageCapabilities and generation/epoch-bound UsageChildren pages; no Search-as-tree workaround. Immediate children are read on demand with byte/work budgets and cursors. Windows request framing is bounded.
- GUI source “索引逻辑大小（非实际占用）”, actual logical Canvas weights without populating allocated bytes, bounded current-layer pages, drill, next-page, cancellation and coalesced refresh. Unsupported services are explicit; no implicit local scan fallback.
- Strict foreground --no-watch --no-backfill --no-everything-ipc fixture mode; explicit fixture root and synthetic-event backend; independent index/pipe/GUI data roots; stop-file and normal window-close lifecycle.

## Isolation and reproduction

Build independent executables:

    cargo build --locked -p findx2-service -p findx2-gui-native --target-dir target/native-t97-build

Start only with an explicitly owned new fixture root/index/pipe, for example:

    findx2-service --index <new-fixture-index> --pipe <unique-fixture-pipe> --no-watch --no-backfill --no-everything-ipc --usage-memory-mib 32 --fixture-root <owned-public-fixture-directory> --fixture-events <owned-events-jsonl> --shutdown-file <owned-stop-file>

`--fixture-root` refuses to overwrite an existing index. The fixture backend uses only that directory and synthetic identities; no real journal, MFT, disk enumeration or production IPC. GUI connects to the same unique pipe with a separate LOCALAPPDATA and fixture-only startup variables. The integration harness samples the two owned processes, uses real desktop pixels after checking fixture-window visibility, never activates another window or injects input, and closes only the processes it launched through normal lifecycle signals.

## Important boundaries / next work

- Phase B is not implemented: no engine allocated sidecar, full128 object binding, complete hardlink graph, allocation coverage or allocated dirty refresh. Existing local measurement still operates independently. File totals do not equal whole-volume used/reclaimable bytes.
- 128-bit identities not safely bindable to existing u64 entries are rejected, not joined by low64. Logical scope is the indexed canonical path set, not a complete filesystem/hardlink namespace. Physical volume GUID/full-ID binding remains B work; A's qualification is the configured volume inside a journal/store epoch.
- Initial aggregation runs before EngineSlot is published (loading state); it is not instant, cancellable online bootstrap/COW initialization. Memory refusal is explicit; no hidden node/time truncation. No formal-service rollout has been performed.
- Metadata IO failure remains unverified/unknown and may await a later event/backfill; durable dirty retry and richer per-file error/source persistence are follow-up work. metadata_ready is not used as proof of freshness. Legacy unbound overlay rows are not imported into the guarded Windows path.
- Usage snapshots are runtime only for A; reopening while the service lives reuses the tree. Restart builds from the persisted index with a new process epoch and unverified flags. There is no automatic historical usage-snapshot migration or full-disk scan on open.
- Generation mismatch requires page restart; no unlimited pinned snapshot/stream subscription. GUI loads a bounded current layer, not all descendants. Large-directory exact sorting and stable multi-page views under continuous churn need later polish.
- Tests validate synthetic and owned-fixture behavior only, not full-volume throughput, real USN kernel delivery, real-media allocation or deployment compatibility. The pre-existing Windows SSD-detection unit queries drive type; subsequent focused watcher tests avoid that probe. No real USN/MFT enumeration was run.

Results/artifacts, screenshots and exact timings/RSS are recorded separately in target/native-usage-t97. No production service/index/cache/settings were changed, and nothing was pushed or published.

## Verified checkpoint results

- Locked regression: core61 (including16 pinyin cases), IPC2, service8, native176+4 worker integrations (1 ignored), plus4 focused USN parser/publication tests:255 passed distinct tests and1 ignored. CLI locked check and independent service/GUI locked builds passed.
- Owned real-process fixture:7 public files/four child directories, separate index/pipe/cache root; create/grow/shrink/rename/move-directory/delete events are fake-watcher inputs through the actual engine mutation API, not real kernel USN delivery. IPC generations advance1→8 and root logical bytes33,947,648→30,801,920. Search/Status compatibility, out-of-scope rejection, drill and stale-generation rejection pass.
- Two final real-screen PNGs at1024×670,120DPI were opened and inspected: nonblank weighted Canvas, right Inspector, fixed source/local-measure button, collapsed path, scrollable children and explicit unavailable-volume section all fit. No PrintWindow assumption, focus activation or injected mouse/keyboard. Invisible Windows resize margins are excluded from the capture.
- The initially blank fixture GUI had a definite application cause: switching modes issued an old-scope request, then the new scope was suppressed by its busy flag; the stale reply cleared busy before capability-checked polling could begin. Navigation now invalidates old requests, suppresses intermediate mode-switch fetches, and retries loading after capability negotiation. The logical scene adapter also now selects logical weights instead of still using allocated=0. Tests and real pixels validate both fixes. The --diag overlay is not used in acceptance captures.
- Watch/backfill health is separately transmitted and refreshed even when the tree generation is unchanged. Switching sources clears old-metric scenes and cancels the old local scanner rather than relabeling its data. Old/unbound overlay freshness is never inferred from mtime.
- Final fixture startup to ready IPC page: 125ms; fake-event commit-to-page latencies 77–125ms (fixture polling contributes; not real USN latency). Service peak working set 17,924,096 bytes; GUI peak working set 102,084,608 bytes; sampled GUI peak PrivateBytes 131,158,016 bytes. Measurements are process counters, not merely modeled tree bytes. Both owned processes closed normally with exit0; pre-existing production services were left untouched.
- Pure in-memory synthetic2,000,018-file test: no real file creation, tree resident capacities346,802,776 bytes; tree/page build1775ms, observed process wall1921ms, peak working set452,255,744 bytes and sampled peak PrivateBytes445,014,016 bytes. Working set includes baseline index/test/allocator overhead and is not equal to the additional-tree byte budget. This is not full-volume performance validation or proof against every possible OOM site.

Run the owned process acceptance from the repository root after building with Python/Pillow available:

    python scripts/test_engine_usage_fixture.py

This creates a new public fixture per run and leaves its report/logs/screenshots in target/native-usage-t97. It does not register a Windows service. Existing formal services are not endpoints for these tests. No production cache content was read or migrated in this implementation run.
