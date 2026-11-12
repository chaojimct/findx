//! Shared, logical-only usage tree. No filesystem IO, paths, allocation-size guesses or scan limits.
//! Store lock precedes runtime lock. All engine mutation/metadata commits use that order.
use crate::{ChangeEvent, IndexStore, SearchEngine};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
const NIL: u32 = u32::MAX;
static EPOCH: AtomicU64 = AtomicU64::new(1);
fn next_epoch() -> u64 {
    static START: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let seed = *START.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        (nanos ^ ((std::process::id() as u64) << 32)).max(1)
    });
    seed.wrapping_add(EPOCH.fetch_add(1, Ordering::Relaxed))
        .max(1)
}
#[derive(Clone, Copy, Debug, Default)]
struct Totals {
    bytes: u64,
    files: u64,
    dirs: u64,
    unknown: u64,
}
impl Totals {
    fn add(&mut self, b: Self) {
        self.bytes = self.bytes.saturating_add(b.bytes);
        self.files += b.files;
        self.dirs += b.dirs;
        self.unknown += b.unknown;
    }
    fn sub(&mut self, b: Self) {
        self.bytes = self.bytes.saturating_sub(b.bytes);
        self.files = self.files.saturating_sub(b.files);
        self.dirs = self.dirs.saturating_sub(b.dirs);
        self.unknown = self.unknown.saturating_sub(b.unknown);
    }
}
#[derive(Clone, Debug)]
struct Node {
    entry: u32,
    parent: u32,
    first: u32,
    next: u32,
    prev: u32,
    directory: bool,
    active: bool,
    own: Totals,
    sum: Totals,
    seq: u64,
    volume: u8,
}
#[derive(Clone, Copy, Debug)]
pub struct MetadataTicket {
    pub entry: usize,
    epoch: u64,
    seq: u64,
    frn: u64,
}
#[derive(Clone, Debug)]
pub struct UsageItem {
    pub id: u32,
    pub name: String,
    pub directory: bool,
    pub logical: u64,
    pub files: u64,
    pub directories: u64,
    pub unknown: u64,
}
#[derive(Clone, Debug)]
pub struct UsagePage {
    pub epoch: u64,
    pub generation: u64,
    pub root: UsageItem,
    pub children: Vec<UsageItem>,
    pub next: Option<u32>,
    pub resident_bytes: usize,
    pub state: String,
}
#[derive(Default)]
pub(crate) struct UsageRuntime {
    nodes: Vec<Node>,
    entries: Vec<u32>,
    dirs: Vec<u32>,
    objects: HashMap<(u8, u64), u32>,
    directory_ids: HashMap<(u8, u64), u32>,
    roots: HashMap<u8, u32>,
    pub epoch: u64,
    pub generation: u64,
    pub budget: usize,
    pub error: Option<String>,
}
impl UsageRuntime {
    fn bytes(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<Node>()
            + (self.entries.capacity() + self.dirs.capacity()) * 4
            + (self.objects.capacity() + self.directory_ids.capacity())
                * (std::mem::size_of::<((u8, u64), u32)>() + 16)
    }
    fn reserve(&mut self, n: usize, d: usize) -> Result<(), String> {
        let grow = |len: usize, cap: usize, extra: usize| {
            if len.saturating_add(extra) <= cap {
                cap
            } else {
                len.saturating_add(extra)
                    .max(cap.saturating_mul(2))
                    .checked_next_power_of_two()
                    .unwrap_or(usize::MAX)
            }
        };
        let hash_grow = |len: usize, cap: usize, extra: usize| {
            if len.saturating_add(extra) <= cap {
                cap
            } else {
                len.saturating_add(extra)
                    .max(cap.saturating_mul(2))
                    .saturating_mul(8)
                    .saturating_div(7)
                    .saturating_add(1)
                    .checked_next_power_of_two()
                    .unwrap_or(usize::MAX)
            }
        };
        let predicted = grow(self.nodes.len(), self.nodes.capacity(), n)
            .saturating_mul(std::mem::size_of::<Node>())
            .saturating_add(grow(self.entries.len(), self.entries.capacity(), n).saturating_mul(4))
            .saturating_add(grow(self.dirs.len(), self.dirs.capacity(), d).saturating_mul(4))
            .saturating_add(
                hash_grow(self.objects.len(), self.objects.capacity(), n)
                    .saturating_mul(std::mem::size_of::<((u8, u64), u32)>() + 16),
            )
            .saturating_add(
                hash_grow(self.directory_ids.len(), self.directory_ids.capacity(), d)
                    .saturating_mul(std::mem::size_of::<((u8, u64), u32)>() + 16),
            );
        if predicted > self.budget {
            return Err(
                "usage resource budget: expanded arrays/identity maps would exceed byte budget"
                    .into(),
            );
        }
        self.nodes
            .try_reserve(n)
            .map_err(|_| "usage node allocation failed")?;
        self.entries
            .try_reserve(n)
            .map_err(|_| "usage entry map allocation failed")?;
        self.dirs
            .try_reserve(d)
            .map_err(|_| "usage directory map allocation failed")?;
        self.objects
            .try_reserve(n)
            .map_err(|_| "usage identity map allocation failed")?;
        self.directory_ids
            .try_reserve(d)
            .map_err(|_| "usage directory identity allocation failed")?;
        if self.bytes() > self.budget {
            return Err("usage actual capacity exceeds byte budget".into());
        }
        Ok(())
    }
    fn build(g: &IndexStore, budget: usize) -> Result<Self, String> {
        let mut t = Self {
            epoch: next_epoch(),
            generation: 1,
            budget,
            ..Self::default()
        };
        t.reserve(g.entries.len() + g.dirs.len(), g.dirs.len())?;
        let staging = (g.entries.len() + g.dirs.len())
            .saturating_mul(std::mem::size_of::<usize>() + 4)
            + g.dirs.len().saturating_mul(128);
        if t.bytes().saturating_add(staging) > budget {
            return Err("usage initialization peak (resident arrays plus topology staging) exceeds byte budget".into());
        }
        // Directory entries retain authoritative packed parent indexes, including old
        // multi-volume files whose directory-table sentinel 0 was not rebased.
        let dir_entries: HashMap<_, _> = g
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_dir_entry())
            .map(|(i, e)| ((g.frns[i], e.name_offset, e.n_len), e.dir_idx))
            .collect();
        let parents: Vec<u32> = g
            .dirs
            .iter()
            .map(|d| {
                dir_entries
                    .get(&(d.frn, d.name_offset, d.name_len))
                    .copied()
                    .unwrap_or(d.parent_idx)
            })
            .collect();
        let roots: Vec<usize> = parents
            .iter()
            .enumerate()
            .filter(|(i, p)| **p as usize == *i)
            .map(|(i, _)| i)
            .collect();
        if roots.len() != g.volumes.len() {
            return Err("usage identity unresolved: directory roots do not match volumes".into());
        }
        let rootvol: HashMap<usize, u8> = roots
            .into_iter()
            .zip(g.volumes.iter().map(|v| v.volume_letter))
            .collect();
        let mut vols = Vec::new();
        vols.try_reserve(g.dirs.len())
            .map_err(|_| "volume map allocation failed")?;
        vols.resize(g.dirs.len(), None);
        for i in 0..g.dirs.len() {
            let mut path = Vec::new();
            let mut at = i;
            let v = loop {
                if let Some(v) = vols[at] {
                    break v;
                }
                if let Some(&v) = rootvol.get(&at) {
                    break v;
                }
                if path.len() > g.dirs.len() {
                    return Err("usage directory cycle".into());
                }
                path.push(at);
                at = *parents.get(at).ok_or("usage invalid parent")? as usize;
            };
            vols[i] = Some(v);
            for d in path {
                vols[d] = Some(v);
            }
        }
        for (&di, &volume) in &rootvol {
            t.roots.insert(volume, di as u32);
        }
        for (di, d) in g.dirs.iter().enumerate() {
            let id = t.nodes.len() as u32;
            t.dirs.push(id);
            let v = vols[di].unwrap();
            t.directory_ids.insert((v, d.frn), di as u32);
            t.nodes.push(Node {
                entry: NIL,
                parent: NIL,
                first: NIL,
                next: NIL,
                prev: NIL,
                directory: true,
                active: true,
                own: Totals {
                    dirs: 1,
                    ..Totals::default()
                },
                sum: Totals::default(),
                seq: 0,
                volume: v,
            });
        }
        for (idx, e) in g.entries.iter().enumerate() {
            let v = *vols
                .get(e.dir_idx as usize)
                .and_then(|v| v.as_ref())
                .ok_or("usage entry volume unresolved")?;
            let frn = g.frns.get(idx).copied().ok_or("usage missing identity")?;
            if t.objects.insert((v, frn), idx as u32).is_some() {
                return Err("usage duplicate object identity in volume".into());
            }
            let id = if e.is_dir_entry() {
                *t.dirs
                    .get(
                        *t.directory_ids
                            .get(&(v, frn))
                            .ok_or("usage missing directory identity")?
                            as usize,
                    )
                    .unwrap()
            } else {
                let id = t.nodes.len() as u32;
                t.nodes.push(Node {
                    entry: idx as u32,
                    parent: NIL,
                    first: NIL,
                    next: NIL,
                    prev: NIL,
                    directory: false,
                    active: !e.is_deleted(),
                    own: Totals {
                        bytes: e.size,
                        files: 1,
                        unknown: 1,
                        dirs: 0,
                    },
                    sum: Totals::default(),
                    seq: 0,
                    volume: v,
                });
                id
            };
            t.entries.push(id);
            t.nodes[id as usize].entry = idx as u32;
            t.nodes[id as usize].active = !e.is_deleted();
        }
        for di in 0..g.dirs.len() {
            let p = parents[di] as usize;
            if p != di {
                t.link(t.dirs[di], t.dirs[p]);
            }
        }
        for idx in 0..g.entries.len() {
            if !g.entries[idx].is_dir_entry() {
                t.link(t.entries[idx], t.dirs[g.entries[idx].dir_idx as usize]);
            }
        }
        // Iterative leaf-to-root aggregation, including suppressed subtree semantics.
        let mut remaining = Vec::new();
        remaining
            .try_reserve(t.nodes.len())
            .map_err(|_| "usage topology allocation failed")?;
        remaining.resize(t.nodes.len(), 0usize);
        let mut leaves = Vec::new();
        leaves
            .try_reserve(t.nodes.len())
            .map_err(|_| "usage topology queue allocation failed")?;
        for i in 0..t.nodes.len() {
            let n = &t.nodes[i];
            if n.parent != NIL {
                remaining[n.parent as usize] += 1;
            }
        }
        for (i, &n) in remaining.iter().enumerate() {
            if n == 0 {
                leaves.push(i as u32)
            }
        }
        let mut seen = 0;
        while let Some(id) = leaves.pop() {
            seen += 1;
            let n = &mut t.nodes[id as usize];
            n.sum.add(n.own);
            let sum = if n.active { n.sum } else { Totals::default() };
            let p = n.parent;
            if p != NIL {
                t.nodes[p as usize].sum.add(sum);
                remaining[p as usize] -= 1;
                if remaining[p as usize] == 0 {
                    leaves.push(p)
                }
            }
        }
        if seen != t.nodes.len() {
            return Err("usage graph cycle".into());
        }
        Ok(t)
    }
    fn link(&mut self, id: u32, p: u32) {
        let first = self.nodes[p as usize].first;
        self.nodes[id as usize].parent = p;
        self.nodes[id as usize].next = first;
        self.nodes[id as usize].prev = NIL;
        if first != NIL {
            self.nodes[first as usize].prev = id;
        }
        self.nodes[p as usize].first = id;
    }
    fn unlink(&mut self, id: u32) {
        let n = self.nodes[id as usize].clone();
        if n.parent == NIL {
            return;
        }
        if n.prev == NIL {
            self.nodes[n.parent as usize].first = n.next
        } else {
            self.nodes[n.prev as usize].next = n.next
        }
        if n.next != NIL {
            self.nodes[n.next as usize].prev = n.prev
        }
        self.nodes[id as usize].parent = NIL;
        self.nodes[id as usize].next = NIL;
        self.nodes[id as usize].prev = NIL;
    }
    fn ancestors(&mut self, mut p: u32, w: Totals, add: bool) {
        while p != NIL {
            let n = &mut self.nodes[p as usize];
            if add {
                n.sum.add(w)
            } else {
                n.sum.sub(w)
            }
            if !n.active {
                break;
            }
            p = n.parent;
        }
    }
    fn visible(&self, mut id: u32) -> bool {
        while id != NIL {
            let n = &self.nodes[id as usize];
            if !n.active {
                return false;
            }
            id = n.parent;
        }
        true
    }
    fn sync(&mut self, g: &IndexStore, idx: usize, known: Option<bool>) -> Result<(), String> {
        let e = &g.entries[idx];
        let id = self.entries[idx];
        let old = self.nodes[id as usize].clone();
        let parent = if old.directory {
            let _di = *self
                .directory_ids
                .get(&(old.volume, g.frns[idx]))
                .ok_or("missing dir")?;
            let p = e.dir_idx;
            if self.dirs[p as usize] == id {
                NIL
            } else {
                self.dirs[p as usize]
            }
        } else {
            self.dirs[e.dir_idx as usize]
        };
        let mut at = parent;
        while at != NIL {
            if at == id {
                return Err("usage directory move cycle".into());
            }
            at = self.nodes[at as usize].parent;
        }
        if old.active {
            self.ancestors(old.parent, old.sum, false)
        }
        if parent != old.parent {
            self.unlink(id);
            if parent != NIL {
                self.link(id, parent)
            }
        }
        let n = &mut self.nodes[id as usize];
        n.active = !e.is_deleted();
        if !n.directory {
            n.sum.sub(n.own);
            n.own.bytes = e.size;
            if let Some(k) = known {
                n.own.unknown = u64::from(!k)
            }
            n.sum.add(n.own);
        }
        n.seq += 1;
        let w = n.sum;
        let active = n.active;
        if active {
            self.ancestors(parent, w, true)
        }
        self.generation += 1;
        Ok(())
    }
    fn item(&self, g: &IndexStore, id: u32) -> Result<UsageItem, String> {
        let n = self.nodes.get(id as usize).ok_or("unknown usage node")?;
        let e = g
            .entries
            .get(n.entry as usize)
            .ok_or("usage entry unavailable")?;
        Ok(UsageItem {
            id,
            name: String::from_utf8_lossy(g.name_bytes(e)).into_owned(),
            directory: n.directory,
            logical: n.sum.bytes,
            files: n.sum.files,
            directories: n.sum.dirs,
            unknown: n.sum.unknown,
        })
    }
    fn resolve(&self, g: &IndexStore, path: &str) -> Result<u32, String> {
        let path = path.replace('/', "\\");
        let path = path.trim_end_matches('\\');
        let (v, tail) = g
            .volumes
            .iter()
            .find_map(|v| {
                let root = v.root_prefix.trim_end_matches('\\');
                if path.eq_ignore_ascii_case(root) {
                    Some((v, ""))
                } else if path.len() > root.len()
                    && path
                        .get(..root.len())
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(root))
                    && path.as_bytes()[root.len()] == b'\\'
                {
                    Some((v, &path[root.len() + 1..]))
                } else {
                    None
                }
            })
            .ok_or("path is outside indexed volumes")?;
        let mut id = *self
            .roots
            .get(&v.volume_letter)
            .ok_or("volume root unavailable")?;
        for part in tail.split('\\').filter(|s| !s.is_empty()) {
            if part == "." || part == ".." {
                return Err("relative path components not allowed".into());
            }
            let mut child = self.nodes[id as usize].first;
            let mut found = None;
            while child != NIL {
                if self.nodes[child as usize].active
                    && self.item(g, child)?.name.eq_ignore_ascii_case(part)
                {
                    found = Some(child);
                    break;
                }
                child = self.nodes[child as usize].next;
            }
            id = found.ok_or("path not present in indexed scope")?;
        }
        if !self.visible(id) {
            return Err("path excluded/deleted".into());
        }
        Ok(id)
    }
}
impl SearchEngine {
    /// Enable before publishing EngineSlot/watchers: initialization is O(N+D), without live lock contention.
    pub fn usage_budget(&self) -> usize {
        self.usage.lock().budget
    }
    pub fn enable_usage(&self, budget: usize) -> Result<(), String> {
        let g = self.store.read();
        let mut r = self.usage.lock();
        *r = UsageRuntime::build(&g, budget)?;
        Ok(())
    }
    pub fn usage_page(
        &self,
        path: &str,
        expected: Option<(u64, u64)>,
        after: Option<u32>,
        max_bytes: usize,
    ) -> Result<UsagePage, String> {
        let g = self.store.read();
        let r = self.usage.lock();
        if let Some(e) = &r.error {
            return Err(e.clone());
        }
        if r.epoch == 0 {
            return Err("indexed usage unavailable / initialization required".into());
        }
        if expected.is_some_and(|v| v != (r.epoch, r.generation)) {
            return Err("usage generation changed; restart page".into());
        }
        let root = r.resolve(&g, path)?;
        let mut at = if let Some(a) = after {
            let n = r.nodes.get(a as usize).ok_or("invalid page cursor")?;
            if n.parent != root {
                return Err("page cursor outside scope".into());
            }
            n.next
        } else {
            r.nodes[root as usize].first
        };
        let mut children = Vec::new();
        let mut bytes = 0;
        let mut examined_bytes = 0;
        let budget = max_bytes.clamp(1024, 256 * 1024);
        let mut last = None;
        while at != NIL {
            if examined_bytes >= budget && last.is_some() {
                break;
            }
            examined_bytes += std::mem::size_of::<Node>();
            let n = &r.nodes[at as usize];
            if n.active {
                let item = r.item(&g, at)?;
                let cost = item.name.len() + 128;
                if bytes + cost > budget && !children.is_empty() {
                    break;
                }
                children
                    .try_reserve(1)
                    .map_err(|_| "usage page allocation failed")?;
                bytes += cost;
                children.push(item);
            }
            last = Some(at);
            at = n.next;
        }
        Ok(UsagePage{epoch:r.epoch,generation:r.generation,root:r.item(&g,root)?,children,next:if at==NIL{None}else{last},resident_bytes:r.bytes(),state:"indexed logical bytes; canonical indexed paths; allocation not measured; journal/metadata freshness tracked separately".into()})
    }
    pub fn metadata_ticket(&self, idx: usize) -> Option<MetadataTicket> {
        let g = self.store.read();
        let r = self.usage.lock();
        let n = r.nodes.get(*r.entries.get(idx)? as usize)?;
        if !n.active {
            return None;
        }
        Some(MetadataTicket {
            entry: idx,
            epoch: r.epoch,
            seq: n.seq,
            frn: *g.frns.get(idx)?,
        })
    }
    pub fn metadata_ticket_by_id(&self, volume: u8, frn: u64) -> Option<MetadataTicket> {
        let g = self.store.read();
        let r = self.usage.lock();
        if r.epoch == 0 || r.error.is_some() {
            return None;
        }
        let idx = *r.objects.get(&(volume, frn))? as usize;
        let n = r.nodes.get(*r.entries.get(idx)? as usize)?;
        if !n.active || g.frns.get(idx) != Some(&frn) {
            return None;
        }
        Some(MetadataTicket {
            entry: idx,
            epoch: r.epoch,
            seq: n.seq,
            frn,
        })
    }

    pub fn commit_metadata(
        &self,
        t: MetadataTicket,
        size: u64,
        mtime: u64,
        ctime: u64,
    ) -> Result<bool, String> {
        let mut g = self.store.write();
        let mut r = self.usage.lock();
        if r.epoch == 0
            || r.error.is_some()
            || r.epoch != t.epoch
            || g.frns.get(t.entry) != Some(&t.frn)
        {
            return Ok(false);
        }
        let Some(&id) = r.entries.get(t.entry) else {
            return Ok(false);
        };
        let n = &r.nodes[id as usize];
        if n.seq != t.seq || !n.active {
            return Ok(false);
        }
        g.patch_entry_metadata(t.entry, size, mtime, ctime)
            .map_err(|e| e.to_string())?;
        self.metadata_overlay.put(
            t.entry,
            size,
            crate::index::filetime_to_unix_secs(mtime),
            crate::index::filetime_to_unix_secs(ctime),
        );
        r.sync(&g, t.entry, Some(true))?;
        self.bump_revision();
        Ok(true)
    }
}
impl UsageRuntime {
    pub(crate) fn metadata_sync(&mut self, g: &IndexStore, idx: usize) {
        if self.epoch != 0 {
            if let Err(e) = self.sync(g, idx, Some(true)) {
                self.error = Some(e)
            }
        }
    }
    pub(crate) fn overlay_value(&mut self, idx: usize, size: u64) {
        if self.epoch == 0 {
            return;
        }
        let Some(&id) = self.entries.get(idx) else {
            return;
        };
        let old = self.nodes[id as usize].clone();
        if old.directory || !old.active {
            return;
        }
        self.ancestors(old.parent, old.sum, false);
        let n = &mut self.nodes[id as usize];
        n.sum.sub(n.own);
        n.own.bytes = size;
        n.own.unknown = 0;
        n.sum.add(n.own);
        n.seq += 1;
        let w = n.sum;
        self.ancestors(old.parent, w, true);
        self.generation += 1;
    }
    fn register(&mut self, g: &IndexStore, idx: usize, volume: u8) -> Result<(), String> {
        let e = &g.entries[idx];
        let id = self.nodes.len() as u32;
        let parent = self.dirs[e.dir_idx as usize];
        self.objects.insert((volume, g.frns[idx]), idx as u32);
        self.entries.push(id);
        if e.is_dir_entry() {
            self.directory_ids
                .insert((volume, g.frns[idx]), self.dirs.len() as u32);
            self.dirs.push(id);
        }
        let own = if e.is_dir_entry() {
            Totals {
                dirs: 1,
                ..Totals::default()
            }
        } else {
            Totals {
                bytes: e.size,
                files: 1,
                unknown: 1,
                dirs: 0,
            }
        };
        self.nodes.push(Node {
            entry: idx as u32,
            parent: NIL,
            first: NIL,
            next: NIL,
            prev: NIL,
            directory: e.is_dir_entry(),
            active: !e.is_deleted(),
            own,
            sum: own,
            seq: 0,
            volume,
        });
        self.link(id, parent);
        if !e.is_deleted() {
            self.ancestors(parent, own, true)
        }
        Ok(())
    }
    fn full_path(&self, g: &IndexStore, mut id: u32) -> Result<String, String> {
        let mut parts = Vec::new();
        while self.nodes[id as usize].parent != NIL {
            parts.push(self.item(g, id)?.name);
            id = self.nodes[id as usize].parent;
        }
        let v = g
            .volumes
            .iter()
            .find(|v| v.volume_letter == self.nodes[id as usize].volume)
            .ok_or("volume unavailable")?;
        let mut s = v.root_prefix.trim_end_matches('\\').to_owned();
        for n in parts.iter().rev() {
            s.push('\\');
            s.push_str(n)
        }
        Ok(s)
    }
}
impl SearchEngine {
    /// Atomic namespace mutation with volume-qualified targets and its logical aggregate update.
    pub fn apply_volume_change(&self, volume: u8, ev: &ChangeEvent) -> Result<(), String> {
        self.apply_volume_change_at(volume, ev, None)
    }
    fn apply_volume_change_at(
        &self,
        volume: u8,
        ev: &ChangeEvent,
        expected_epoch: Option<u64>,
    ) -> Result<(), String> {
        let mut g = self.store.write();
        let mut r = self.usage.lock();
        if expected_epoch.is_some_and(|e| e != r.epoch) {
            return Err("batch index epoch changed".into());
        }
        if r.epoch == 0 || r.error.is_some() {
            return Err("usage/identity epoch unavailable".into());
        }
        let extended = match ev {
            ChangeEvent::Create { entry } => entry.file_id_128,
            ChangeEvent::CreatePending { file_id_128, .. } => *file_id_128,
            _ => None,
        };
        if extended.is_some_and(|id| id[8..].iter().any(|&b| b != 0)) {
            return Err(
                "full 128-bit identity binding unavailable; refusing truncated-ID mutation".into(),
            );
        }
        let (frn, parent, name) = match ev {
            ChangeEvent::Create { entry } => (
                entry.file_id,
                Some(entry.parent_id),
                Some(entry.name.as_str()),
            ),
            ChangeEvent::CreatePending {
                file_id,
                parent_id,
                name,
                ..
            } => (*file_id, Some(*parent_id), Some(name.as_str())),
            ChangeEvent::Rename {
                file_id,
                new_parent_id,
                new_name,
            } => (*file_id, Some(*new_parent_id), Some(new_name.as_str())),
            ChangeEvent::Delete { file_id } => (*file_id, None, None),
            ChangeEvent::DataOrMeta { file_id, .. } => (*file_id, None, None),
        };
        let entry = r.objects.get(&(volume, frn)).copied();
        let parent = if let Some(p) = parent {
            Some(
                *r.directory_ids
                    .get(&(volume, p))
                    .ok_or("parent identity missing; batch not committed")?,
            )
        } else {
            None
        };
        let directory = r.directory_ids.get(&(volume, frn)).copied();
        if let (Some(di), Some(p)) = (directory, parent) {
            let id = r.dirs[di as usize];
            let mut at = r.dirs[p as usize];
            while at != NIL {
                if at == id {
                    return Err("directory move cycle".into());
                }
                at = r.nodes[at as usize].parent;
            }
        }
        let excluded = if let (Some(p), Some(name)) = (parent, name) {
            let mut path = r.full_path(&g, r.dirs[p as usize])?;
            path.push('\\');
            path.push_str(name);
            let path = path.to_lowercase();
            g.excluded_dirs.iter().any(|ex| {
                let ex = ex.replace('/', "\\").trim_end_matches('\\').to_lowercase();
                path == ex
                    || path
                        .strip_prefix(&ex)
                        .is_some_and(|tail| tail.starts_with('\\'))
            })
        } else {
            false
        };
        g.try_reserve_namespace_change(
            name.map(str::len).unwrap_or(0),
            entry.is_none(),
            directory.is_none()
                && matches!(
                    ev,
                    ChangeEvent::CreatePending { is_dir: true, .. }
                        | ChangeEvent::Create {
                            entry: crate::RawEntry { is_dir: true, .. }
                        }
                ),
        )?;
        let before = g.entries.len();
        if entry.is_none() && !excluded {
            r.reserve(
                1,
                usize::from(matches!(
                    ev,
                    ChangeEvent::CreatePending { is_dir: true, .. }
                        | ChangeEvent::Create {
                            entry: crate::RawEntry { is_dir: true, .. }
                        }
                )),
            )?;
        }
        if excluded {
            if let Some(idx) = entry {
                g.delete_entry(idx)
            }
        } else {
            let scope = crate::index::ResolvedMutation {
                entry,
                parent: parent.unwrap_or(0),
                directory,
            };
            // Preserve effective metadata on rename while overlay is still populated.
            if let Some(idx) = entry {
                if let Some((size, mt, ct)) = self.metadata_overlay.get(idx as usize) {
                    g.patch_entry_metadata(
                        idx as usize,
                        size,
                        crate::index::unix_secs_to_filetime(mt),
                        crate::index::unix_secs_to_filetime(ct),
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            g.apply_change_event_resolved(ev, Some(&scope))
                .map_err(|e| e.to_string())?;
        }
        let idx = if g.entries.len() > before {
            r.register(&g, before, volume)?;
            Some(before)
        } else {
            entry.map(|i| i as usize)
        };
        if let Some(idx) = idx {
            let known = match ev {
                ChangeEvent::CreatePending { .. } => Some(false),
                ChangeEvent::Create { .. } => Some(true),
                ChangeEvent::DataOrMeta { size: Some(_), .. } => Some(true),
                _ => None,
            };
            r.sync(&g, idx, known)?;
            let e = &g.entries[idx];
            self.metadata_overlay.put(idx, e.size, e.mtime, e.ctime);
        }
        self.bump_revision();
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn raw(id: u64, parent: u64, name: &str, size: u64, dir: bool) -> crate::RawEntry {
        crate::RawEntry {
            file_id: id,
            file_id_128: None,
            parent_id: parent,
            name: name.into(),
            size,
            mtime: 0,
            ctime: 0,
            attrs: if dir { 0x10 } else { 0 },
            is_dir: dir,
        }
    }
    fn store(v: u8) -> IndexStore {
        crate::IndexBuilder::new(v, 1, 7, 10)
            .build_from_raw(
                vec![raw(10, 2, "a.bin", 100, false)],
                vec![
                    raw(1, 0, "root", 0, true),
                    raw(2, 1, "left", 0, true),
                    raw(3, 1, "right", 0, true),
                ],
                true,
            )
            .unwrap()
    }
    fn engine() -> SearchEngine {
        let e = SearchEngine::new(store(b'C'));
        e.enable_usage(16 * 1024 * 1024).unwrap();
        e
    }
    fn size(e: &SearchEngine) -> u64 {
        e.usage_page("C:\\", None, None, 1024).unwrap().root.logical
    }
    #[test]
    fn incremental_namespace_and_metadata() {
        let e = engine();
        assert_eq!(size(&e), 100);
        e.apply_volume_change(
            b'C',
            &ChangeEvent::Create {
                entry: raw(11, 2, "new", 50, false),
            },
        )
        .unwrap();
        assert_eq!(size(&e), 150);
        for s in [200, 20] {
            e.apply_volume_change(
                b'C',
                &ChangeEvent::DataOrMeta {
                    file_id: 11,
                    size: Some(s),
                    mtime: None,
                    ctime: None,
                    create_if_missing: None,
                },
            )
            .unwrap();
            assert_eq!(size(&e), 100 + s);
        }
        e.apply_volume_change(
            b'C',
            &ChangeEvent::Rename {
                file_id: 11,
                new_parent_id: 3,
                new_name: "moved".into(),
            },
        )
        .unwrap();
        assert_eq!(
            e.usage_page("C:\\right", None, None, 1024)
                .unwrap()
                .root
                .logical,
            20
        );
        e.apply_volume_change(
            b'C',
            &ChangeEvent::Rename {
                file_id: 2,
                new_parent_id: 3,
                new_name: "tree".into(),
            },
        )
        .unwrap();
        assert_eq!(
            e.usage_page("C:\\right", None, None, 1024)
                .unwrap()
                .root
                .logical,
            120
        );
        e.apply_volume_change(b'C', &ChangeEvent::Delete { file_id: 2 })
            .unwrap();
        assert_eq!(size(&e), 20);
        e.apply_volume_change(b'C', &ChangeEvent::Delete { file_id: 10 })
            .unwrap();
        assert_eq!(size(&e), 20);
        e.apply_volume_change(b'C', &ChangeEvent::Delete { file_id: 11 })
            .unwrap();
        assert_eq!(size(&e), 0);
    }
    #[test]
    fn metadata_order_overlay_flush_and_epoch() {
        let e = engine();
        let old = e.metadata_ticket_by_id(b'C', 10).unwrap();
        e.apply_volume_change(
            b'C',
            &ChangeEvent::DataOrMeta {
                file_id: 10,
                size: Some(200),
                mtime: None,
                ctime: None,
                create_if_missing: None,
            },
        )
        .unwrap();
        assert!(!e.commit_metadata(old, 1, 0, 0).unwrap());
        assert_eq!(size(&e), 200);
        let t = e.metadata_ticket_by_id(b'C', 10).unwrap();
        assert!(e.commit_metadata(t, 300, 0, 0).unwrap());
        e.flush_metadata_overlay_into_store().unwrap();
        assert_eq!(size(&e), 300);
        e.extend_metadata_overlay_batch(&[(0, 400, 0, 0)]);
        assert_eq!(size(&e), 400);
        e.flush_metadata_overlay_into_store().unwrap();
        assert_eq!(size(&e), 400);
        let t = e.metadata_ticket_by_id(b'C', 10).unwrap();
        e.note_external_mutation();
        e.enable_usage(16 * 1024 * 1024).unwrap();
        assert!(!e.commit_metadata(t, 1, 0, 0).unwrap());
    }
    #[test]
    fn volume_identity_and_generation_scope() {
        let e =
            SearchEngine::new(crate::merge_index_stores(vec![store(b'C'), store(b'D')]).unwrap());
        e.enable_usage(16 * 1024 * 1024).unwrap();
        let p = e.usage_page("C:\\", None, None, 1024).unwrap();
        e.apply_volume_change(
            b'D',
            &ChangeEvent::DataOrMeta {
                file_id: 10,
                size: Some(999),
                mtime: None,
                ctime: None,
                create_if_missing: None,
            },
        )
        .unwrap();
        assert_eq!(size(&e), 100);
        assert_eq!(
            e.usage_page("D:\\", None, None, 1024).unwrap().root.logical,
            999
        );
        assert!(e
            .usage_page("C:\\", Some((p.epoch, p.generation)), None, 1024)
            .is_err());
        assert!(e.usage_page("C:\\leftover", None, None, 1024).is_err());
    }
    #[test]
    fn allocation_failure_and_missing_parent_do_not_advance() {
        let e = SearchEngine::new(store(b'C'));
        assert!(e.enable_usage(1).is_err());
        let e = engine();
        let p = e.usage_page("C:\\", None, None, 1024).unwrap();
        assert!(e
            .apply_volume_change(
                b'C',
                &ChangeEvent::Create {
                    entry: raw(77, 999, "orphan", 1, false)
                }
            )
            .is_err());
        assert_eq!(
            e.usage_page("C:\\", None, None, 1024).unwrap().generation,
            p.generation
        );
    }
}

impl SearchEngine {
    pub fn mark_metadata_dirty(&self, volume: u8, frn: u64) {
        let _g = self.store.read();
        let mut r = self.usage.lock();
        let Some(&idx) = r.objects.get(&(volume, frn)) else {
            return;
        };
        let id = r.entries[idx as usize];
        let n = r.nodes[id as usize].clone();
        if n.directory || !n.active {
            return;
        }
        r.ancestors(n.parent, n.sum, false);
        let node = &mut r.nodes[id as usize];
        node.sum.sub(node.own);
        node.own.unknown = 1;
        node.sum.add(node.own);
        node.seq += 1;
        let w = node.sum;
        r.ancestors(n.parent, w, true);
        r.generation += 1;
    }
    /// Failed dependency/application never acknowledges the end cursor. Replaying
    /// an already-applied prefix is idempotent, including scoped create and rename.
    pub fn apply_volume_batch(
        &self,
        volume: u8,
        events: &[ChangeEvent],
        journal_id: u64,
        next_usn: u64,
    ) -> Result<(), String> {
        let epoch = {
            let g = self.store.read();
            let r = self.usage.lock();
            let v = g
                .volumes
                .iter()
                .find(|v| v.volume_letter == volume)
                .ok_or("batch volume unavailable")?;
            if v.usn_journal_id != 0 && v.usn_journal_id != journal_id {
                return Err("journal epoch changed; rebuild required".into());
            }
            if next_usn < v.last_usn {
                return Err("cursor regression rejected".into());
            }
            r.epoch
        };
        let mut pending = Vec::new();
        pending
            .try_reserve(events.len())
            .map_err(|_| "batch replay buffer allocation failed")?;
        pending.extend(events.iter());
        while !pending.is_empty() {
            let before = pending.len();
            let mut last = String::new();
            pending.retain(
                |ev| match self.apply_volume_change_at(volume, ev, Some(epoch)) {
                    Ok(()) => false,
                    Err(e) => {
                        last = e;
                        true
                    }
                },
            );
            if pending.len() == before {
                return Err(format!("batch not committed: {last}"));
            }
        }
        let mut g = self.store.write();
        let r = self.usage.lock();
        if r.epoch != epoch {
            return Err("batch epoch changed before cursor commit".into());
        }
        let v = g
            .volumes
            .iter_mut()
            .find(|v| v.volume_letter == volume)
            .ok_or("batch volume unavailable")?;
        if v.usn_journal_id != 0 && v.usn_journal_id != journal_id {
            return Err("journal epoch changed; rebuild required".into());
        }
        if next_usn < v.last_usn {
            return Err("cursor regression rejected".into());
        }
        v.usn_journal_id = journal_id;
        v.last_usn = next_usn;
        Ok(())
    }
}
#[cfg(test)]
mod scale_tests {
    use super::*;
    #[test]
    fn over_two_million_synthetic_entries_have_no_count_cutoff() {
        let raw = |id, parent, dir| crate::RawEntry {
            file_id: id,
            file_id_128: None,
            parent_id: parent,
            name: if dir { "root".into() } else { "x".into() },
            size: 1,
            mtime: 0,
            ctime: 0,
            attrs: if dir { 0x10 } else { 0 },
            is_dir: dir,
        };
        let mut g = crate::IndexBuilder::new(b'C', 1, 1, 1)
            .build_from_raw(vec![raw(2, 1, false)], vec![raw(1, 0, true)], true)
            .unwrap();
        let n = 2_000_017usize;
        let file = g.entries[0];
        g.entries.try_reserve(n).unwrap();
        g.frns.try_reserve(n).unwrap();
        for i in 0..n {
            g.entries.push(file);
            g.frns.push(100 + i as u64);
        }
        let e = SearchEngine::new(g);
        let start = std::time::Instant::now();
        e.enable_usage(1024 * 1024 * 1024).unwrap();
        let p = e.usage_page("C:\\", None, None, 4096).unwrap();
        assert_eq!(p.root.files, (n + 1) as u64);
        assert_eq!(p.root.logical, (n + 1) as u64);
        assert!(p.next.is_some());
        eprintln!(
            "synthetic logical tree entries={} resident_capacity_bytes={} build_ms={}",
            n + 1,
            p.resident_bytes,
            start.elapsed().as_millis()
        );
    }
}
#[cfg(test)]
mod batch_tests {
    use super::*;
    #[test]
    fn failed_batch_retains_cursor_and_can_replay_after_missing_parent() {
        let raw = |id, parent, name: &str, dir| crate::RawEntry {
            file_id: id,
            file_id_128: None,
            parent_id: parent,
            name: name.into(),
            size: 10,
            mtime: 0,
            ctime: 0,
            attrs: if dir { 0x10 } else { 0 },
            is_dir: dir,
        };
        let g = crate::IndexBuilder::new(b'F', 1, 9, 10)
            .build_from_raw(vec![], vec![raw(1, 0, "root", true)], true)
            .unwrap();
        let e = SearchEngine::new(g);
        e.enable_usage(1024 * 1024).unwrap();
        let child = ChangeEvent::Create {
            entry: raw(3, 2, "file", false),
        };
        assert!(e.apply_volume_batch(b'F', &[child.clone()], 9, 20).is_err());
        assert_eq!(e.index_store().volumes[0].last_usn, 10);
        e.apply_volume_batch(
            b'F',
            &[
                child.clone(),
                ChangeEvent::Create {
                    entry: raw(2, 1, "parent", true),
                },
            ],
            9,
            20,
        )
        .unwrap();
        assert_eq!(e.index_store().volumes[0].last_usn, 20);
        assert_eq!(
            e.usage_page("F:\\", None, None, 4096).unwrap().root.logical,
            10
        );
        assert!(e
            .apply_volume_batch(b'F', &[ChangeEvent::Delete { file_id: 3 }], 99, 21)
            .is_err());
        assert_eq!(
            e.usage_page("F:\\", None, None, 4096).unwrap().root.logical,
            10
        );
        let t = e.metadata_ticket_by_id(b'F', 3).unwrap();
        e.mark_metadata_dirty(b'F', 3);
        assert!(!e.commit_metadata(t, 99, 0, 0).unwrap());
        let t = e.metadata_ticket_by_id(b'F', 3).unwrap();
        e.note_external_mutation();
        e.enable_usage(1024 * 1024).unwrap();
        assert!(!e.commit_metadata(t, 99, 0, 0).unwrap());
    }
}
#[cfg(test)]
mod allocation_tests {
    use super::*;
    #[test]
    fn capacity_overflow_is_a_fallible_allocation_error_not_a_panic() {
        let mut runtime = UsageRuntime {
            budget: usize::MAX,
            ..Default::default()
        };
        let e = runtime.reserve(usize::MAX, 0).unwrap_err();
        assert!(e.contains("allocation failed"), "{e}");
    }
}
