# Native usage resource and rendering validation

2026-11-12, Asia/Shanghai. Local MZ checkout, no production index/service/IPC changes.

## Determined causes

* The map wrapper was a normal block div. Its flex_1 child had no flex parent to
  allocate height; the absolutely positioned GPU canvas inherited zero height.
  The main-page block wrapper also broke vertical flex propagation. Both are
  now vertical flex containers; the camera is explicitly a column flex box.
* Cache lookup enumerated recent snapshot files and expanded unrelated trees,
  then remembered them before checking coverage. A 5/12/47 MiB compressed file
  size therefore did not bound resident reconstructed full paths and child lists.
  The old read path also touched snapshot mtimes. Earlier changed timestamps
  alone do not prove all three files were reserialized by the fixture process;
  no historical snapshot contents were reopened to speculate about that.
  New lookup opens only the requested identity and identity-named ancestors,
  retains only a covering result, and never touches historical snapshot mtimes.
* DPI-unaware PrintWindow cropped a 125% window. Diagnosis now uses measured
  logical bounds, actual paint counts and an occlusion-checked real-screen capture,
  not a blank or clipped PrintWindow as product acceptance.

## Resources (not node tiers)

No scan entry count or wall-clock cap. Default allowance is now **1 GiB managed
scan/derived memory**, not the previous 512 MiB length-only tree estimate.
Charges include reserved Node/children capacities, PathBuf capacity, conservative
normalized lookup/adapter string allowances, identity/hash/queue/sort overhead.
This is NOT a promise that total process RSS including GPUI/GPU/allocator baseline
cannot exceed 1 GiB. The independent LRU decoded-cache allowance is 256 MiB.
Cache reads reject >256 MiB uncompressed entries and projected/actual expanded
resource excess before unbounded expansion. Cache writes refuse trees beyond the
read-resident allowance without truncating or throwing away the measured scan.
Cache resource refusal is surfaced in the UI; original snapshots stay unchanged.
Major node/child/hash/queue/lookup reserves are fallible. Severe Windows physical
memory pressure (load >=97% or available <64 MiB) refuses further admission.
No fixed entry constant stands in for that measurement.

Fixture launch uses a process-private temporary cache directory, never the normal
historical cache. Explicit fixture lock prevents navigation outside the selected
public fixture, and fixture scans refuse paths outside that root. Test launches
request no activation. Diagnostics are debug/fixture-only, count/size-only and
bounded; generated temp logs are cleared after controlled close.

## Evidence

* Locked library suite: 175 passed, 1 intentionally ignored, 0 failed.
* Regression: only requested snapshot decoded; unrelated snapshot bytes/mtime
  unchanged; miss does not decode unrelated roots. Inflated snapshot header
  rejected before tree allocation. Existing cache/cancel/partial/legacy tests pass.
* Synthetic admission exceeds old 500k/2M defaults; no real million-file fixture.
* Actual-allocation synthetic test (250,000 nodes, zero filesystem files): Node
  capacity 250,880; managed allowance 133,708,910 bytes; private growth 62,951,424
  bytes; working-set growth 62,935,040 bytes. At 100k: allowance 53,232,878 bytes,
  private growth 25,477,120 bytes. These are a measured correlation, not a proof
  for every path distribution or allocator/OS state.
* Final EXE public fixture: 122 nodes, Node capacity 1024; managed tree/derived
  allowance 218,156 bytes; lookup capacity 224; volume API available. Only one
  2187-byte snapshot in that process-private cache.
* Sustained final narrow-window run through 54 seconds: working set
  92,758,016..92,811,264 bytes; private 75,800,576..75,878,400 bytes, no 1.2 GiB rise.
* 1024-logical window: canvas 728 x 524.8, 54 painted tiles; real-screen capture
  1280 x 838 device pixels at 125% DPI.
* Stricter narrow window: 819.2 x 536 logical; real-screen capture exactly
  **1024 x 670** device pixels. Canvas 523.2 x 390.4, 52 painted tiles. Treemap,
  folded address, rescan button, selected item, scrollable Inspector middle,
  docked volume and bottom status all visible. Missing volume has explicit text.
* Final PNG inspected with Read: target/native-usage-t95/usage-final-1024x670-screen.png.
  The screen capture verifies public window visibility before reading pixels,
  uses consistent DPI, and injects no input/foreground activation.

Final EXE: target/native-usage-t95/findx2-gui-native.exe (locked dev build).
No full volume rescan performed. Large/long-path whole-volume peak RSS is not
claimed validated by the small fixture. All owned isolated GUI instances were
closed through normal lifecycle. No live GUI was running at final inspection;
its earlier disappearance remains unexplained and was not auto-restored.
Three snapshots moved during the previous t94 run were not touched again.
Their original/destination paths and previous motive are recorded in the local
artifact historical-snapshot-move-record.json, not inferred from private contents.
