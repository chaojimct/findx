# t154 Stage B: generation publish, not a production switch

Baseline `f7431c9` (Stage A). Candidate version remains 2.5.0. No official install, production index, SCM service, UAC, push, tag, or running trial process was changed.

## Transaction design

Already-published generation files are never opened for write. A commit materializes `index.bin.v6/gen/<n>/` with a complete set of segments plus optional `tri.pending` and `trigram.dat`. Unchanged files are hard-linked (copy fallback). Changed files are copied from the previous committed prefix, then patched. Every new file is `sync_all`'d, the generation directory is flushed (Windows `FlushFileBuffers` on a backup-semantics directory handle with write access), and only then is `published.json` replaced with `MoveFileEx(REPLACE_EXISTING | WRITE_THROUGH)` — no delete-before-rename, and sync errors are not ignored.

Failure or an isolated-process kill before that replace leaves the previous published pointer as the only loadable state. After replace, load verifies size+CRC32 of every listed sidecar and returns the complete new generation. Metadata, namespace, entries, cursors and trigram sidecars are published together. Missing or corrupt listed files fail closed; leftover flat `entries.dat` or a future unpublished `gen/` directory cannot be mixed in.

Legacy flat v6 and v5 stay readable. The first transactional write copies a consistent snapshot into `gen/n` and does not patch the old files. Application-owned `gen/` dirs keep the current and previous generation only. Pre-migration leftover files and any path outside `gen/` are not reclaimed. The previous in-place prefix format remains unsafe if someone still writes that layout; that is documented by a dedicated reproduction, not treated as a durability pass.

Trigram rebuild writes a staging file and, when a published pointer exists, publishes a new generation that hard-links the current data files and includes the new snapshot plus remaining pending bits, then optionally replaces the classic `.tri` path.

## Fault matrix (isolated fixtures)

Before publish, complete old state was restored for: injected before-publish, after-data-sync, rename failure, short write, disk-full quota, access failure, and an isolated child process killed at a pre-publish barrier. After a successful publish, load returns the complete new state and an empty restart commit is idempotent. Post-publish missing `names.dat` or a flipped CRC fails load rather than mixing leftover bytes. A shared handle held on the previous generation still reads the old bytes after the next publish. Generation GC kept at most two `gen/` dirs on a 5-commit fixture (942 observed directory bytes including links) and left an explicit leftover `legacy-keep.dat` untouched.

These are protocol and injected-IO proofs, plus one isolated child kill. They are not a certified physical power loss, controller cache, full-volume USN, or official-install result.

## Gates and deployment

`publish_ready` remains false. Required 2.5.0 gates are unchanged. No official version switch was performed. Engine 39932 and the stopped `FindX2Search` service were left alone. Stage C (isolated install rollback + deliverable release candidate) is not part of this commit.
