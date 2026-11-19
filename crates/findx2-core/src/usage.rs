//! Compact logical usage tree: directory aggregates plus per-file links.
//! Reuses FileEntry name/size/FRN/parent indexes. No per-file Node and no full identity HashMap.
//! Store lock precedes runtime lock. Namespace mutations do not require a healthy usage tree.
use crate::{ChangeEvent, IndexStore, SearchEngine};
use crate::index::ResolvedMutation;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
const NIL: u32 = u32::MAX;
const ACTIVE: u8 = 1;
const ROOT_ALIAS: u8 = 2;
// Capability bits live only on existing volume roots. Keep the runtime/engine
// layout and directory stride unchanged; no auxiliary parent index is needed.
const PARENT_SEARCH_VALID: u8 = 4;
const PARENT_SEARCH_DEEP_VALID: u8 = 8;
const CURSOR_SLOTS: usize = 1024;
const SCAN_WORK_BYTES: usize = 64 * 1024;
static CURSOR_SEQUENCE: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy, Debug)]
struct PageContinuation { token:u32, epoch:u64, generation:u64, root:u32, dir:Option<u32>, file:Option<u32> }

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
    seed.wrapping_add(EPOCH.fetch_add(1, Ordering::Relaxed)).max(1)
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Totals {
    bytes: u64,
    files: u64,
    dirs: u64,
    unknown: u64,
}
impl Totals {
    fn file(bytes: u64, unknown: bool) -> Self {
        Self { bytes, files: 1, dirs: 0, unknown: u64::from(unknown) }
    }
    fn dir_self() -> Self {
        Self { bytes: 0, files: 0, dirs: 1, unknown: 0 }
    }
    fn add(&mut self, b: Self) {
        self.bytes = self.bytes.saturating_add(b.bytes);
        self.files = self.files.saturating_add(b.files);
        self.dirs = self.dirs.saturating_add(b.dirs);
        self.unknown = self.unknown.saturating_add(b.unknown);
    }
    fn sub(&mut self, b: Self) {
        self.bytes = self.bytes.saturating_sub(b.bytes);
        self.files = self.files.saturating_sub(b.files);
        self.dirs = self.dirs.saturating_sub(b.dirs);
        self.unknown = self.unknown.saturating_sub(b.unknown);
    }
}
/// 48-byte directory aggregate. Counts are bounded by the u32 namespace IDs.
/// Public totals remain u64; checked writes fail closed instead of truncating.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct DirAgg {
    logical: u64,
    files: u32,
    dirs: u32,
    unknown: u32,
    parent: u32,
    first_dir: u32,
    next_dir: u32,
    prev_dir: u32,
    owner_entry: u32,
    first_file: u32,
    flags: u8,
    volume_slot: u8,
    pad: [u8; 2],
}
impl DirAgg {
    fn empty(parent: u32, volume_slot: u8) -> Self {
        Self {
            logical: 0, files: 0, dirs: 0, unknown: 0,
            parent, first_dir: NIL, next_dir: NIL, prev_dir: NIL,
            owner_entry: NIL, first_file: NIL, flags: ACTIVE, volume_slot, pad: [0; 2],
        }
    }
    fn totals(&self) -> Totals {
        Totals { bytes: self.logical, files: self.files as u64, dirs: self.dirs as u64, unknown: self.unknown as u64 }
    }
    fn set_totals(&mut self, t: Totals) -> Result<(), String> {
        let files=u32::try_from(t.files).map_err(|_|"usage file count exceeds namespace index space")?;
        let dirs=u32::try_from(t.dirs).map_err(|_|"usage directory count exceeds namespace index space")?;
        let unknown=u32::try_from(t.unknown).map_err(|_|"usage unknown count exceeds namespace index space")?;
        self.logical=t.bytes; self.files=files; self.dirs=dirs; self.unknown=unknown;
        Ok(())
    }
    fn active(self) -> bool { self.flags & ACTIVE != 0 }
}
const _: () = assert!(std::mem::size_of::<DirAgg>() == 48);
/// 8-byte intrusive file sibling links; parent is FileEntry.dir_idx.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct FileLinks { next: u32, prev: u32 }
impl Default for FileLinks { fn default() -> Self { Self { next: NIL, prev: NIL } } }
const _: () = assert!(std::mem::size_of::<FileLinks>() == 8);
/// Packed 2-bit freshness per entry: 0 unknown, 1 known, 2 dirty, 3 error.
struct Freshness { bytes: Vec<u8> }
impl Freshness {
    fn with_len(n: usize) -> Result<Self, String> {
        let mut s = Self { bytes: Vec::new() };
        s.resize(n)?;
        Ok(s)
    }
    fn resize(&mut self, n: usize) -> Result<(), String> {
        let need = n.div_ceil(4);
        if need > self.bytes.capacity() {
            self.bytes.try_reserve(need - self.bytes.len()).map_err(|_| "usage freshness allocation failed")?;
        }
        self.bytes.resize(need, 0);
        Ok(())
    }
    fn get(&self, idx: usize) -> u8 {
        let b = *self.bytes.get(idx / 4).unwrap_or(&0);
        (b >> ((idx % 4) * 2)) & 3
    }
    fn set(&mut self, idx: usize, v: u8) {
        if idx / 4 >= self.bytes.len() { return; }
        let shift = (idx % 4) * 2;
        let mask = 3u8 << shift;
        self.bytes[idx / 4] = (self.bytes[idx / 4] & !mask) | ((v & 3) << shift);
    }
    fn capacity_bytes(&self) -> usize { self.bytes.capacity() }
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
pub(crate) struct OrphanQueue {
    ordered: Vec<(u8, u64, ChangeEvent)>,
    bytes: usize,
    budget: usize,
}
impl Default for OrphanQueue {
    fn default() -> Self { Self { ordered: Vec::new(), bytes: 0, budget: 4 * 1024 * 1024 } }
}
impl OrphanQueue {
    fn identity(ev: &ChangeEvent) -> u64 {
        match ev {
            ChangeEvent::Create { entry } => entry.file_id,
            ChangeEvent::CreatePending { file_id, .. } | ChangeEvent::Rename { file_id, .. }
            | ChangeEvent::Delete { file_id } | ChangeEvent::DataOrMeta { file_id, .. } => *file_id,
        }
    }
    fn event_cost(ev: &ChangeEvent) -> usize {
        128 + match ev {
            ChangeEvent::Create { entry } => entry.name.len(),
            ChangeEvent::CreatePending { name, .. } => name.len(),
            ChangeEvent::Rename { new_name, .. } => new_name.len(),
            _ => 0,
        }
    }
    fn take_volume(&mut self, volume: u8) -> Vec<ChangeEvent> {
        // Preserve journal order across different missing parents/identities.
        let all = std::mem::take(&mut self.ordered);
        let mut out = Vec::new();
        for (v, p, ev) in all {
            if v == volume {
                self.bytes = self.bytes.saturating_sub(Self::event_cost(&ev));
                out.push(ev);
            } else { self.ordered.push((v, p, ev)); }
        }
        out
    }
    fn park(&mut self, volume: u8, parent: u64, ev: ChangeEvent) -> Result<(), String> {
        let cost = Self::event_cost(&ev);
        if self.bytes.saturating_add(cost) > self.budget {
            return Err("pending parent-resolution budget exceeded; cursor not acknowledged".into());
        }
        self.ordered.try_reserve(1).map_err(|_| "pending parent-resolution allocation failed")?;
        self.ordered.push((volume, parent, ev));
        self.bytes = self.bytes.saturating_add(cost);
        Ok(())
    }
    pub(crate) fn clear(&mut self) { self.ordered.clear(); self.bytes = 0; }
}
#[derive(Default)]
pub(crate) struct UsageRuntime {
    dirs: Vec<DirAgg>,
    file_links: Vec<FileLinks>,
    freshness: Freshness,
    roots: HashMap<u8, u32>,
    tickets: HashMap<usize, u64>,
    ticket_seq: u64,
    excluded_ids: HashSet<(u8, u64)>,
    pub epoch: u64,
    pub generation: u64,
    pub budget: usize,
    pub error: Option<String>,
    ancestor_visits: u64,
    page_cursors: Vec<Option<PageContinuation>>,
}
impl Default for Freshness { fn default() -> Self { Self { bytes: Vec::new() } } }
impl UsageRuntime {
    fn bytes(&self) -> usize {
        self.dirs.capacity() * std::mem::size_of::<DirAgg>()
            + self.file_links.capacity() * std::mem::size_of::<FileLinks>()
            + self.freshness.capacity_bytes()
            + self.page_cursors.capacity() * std::mem::size_of::<Option<PageContinuation>>()
            + (self.roots.capacity() + self.tickets.capacity() + self.excluded_ids.capacity()) * 32
    }
    fn reserve(&mut self, n: usize, d: usize) -> Result<(), String> {
        let grow = |len: usize, cap: usize, extra: usize| {
            if len.saturating_add(extra) <= cap { cap } else {
                len.saturating_add(extra).max(cap.saturating_mul(2)).checked_next_power_of_two().unwrap_or(usize::MAX)
            }
        };
        let pn = grow(self.file_links.len(), self.file_links.capacity(), n);
        let pd = grow(self.dirs.len(), self.dirs.capacity(), d);
        let pf = pn.div_ceil(4);
        let predicted = pd.saturating_mul(std::mem::size_of::<DirAgg>())
            .saturating_add(pn.saturating_mul(std::mem::size_of::<FileLinks>()))
            .saturating_add(pf);
        let other=(self.roots.capacity()+self.tickets.capacity()+self.excluded_ids.capacity())*32
            +self.page_cursors.capacity()*std::mem::size_of::<Option<PageContinuation>>();
        if predicted.saturating_add(other) > self.budget {
            return Err("usage resource budget: expanded directory aggregates/file links would exceed byte budget".into());
        }
        self.dirs.try_reserve(d).map_err(|_| "usage directory aggregate allocation failed")?;
        self.file_links.try_reserve(n).map_err(|_| "usage file link allocation failed")?;
        let need = self.file_links.len().saturating_add(n).div_ceil(4);
        if need > self.freshness.bytes.capacity() {
            self.freshness.bytes.try_reserve(need.saturating_sub(self.freshness.bytes.len()))
                .map_err(|_| "usage freshness allocation failed")?;
        }
        if self.bytes() > self.budget {
            return Err("usage actual capacity exceeds byte budget".into());
        }
        Ok(())
    }
    fn ensure_entries(&mut self, g: &IndexStore) -> Result<(), String> {
        let n = g.entries.len();
        if n > self.file_links.len() {
            self.reserve(n - self.file_links.len(), 0)?;
            self.file_links.resize(n, FileLinks::default());
            self.freshness.resize(n)?;
        }
        Ok(())
    }
    fn build(g: &IndexStore, budget: usize) -> Result<Self, String> {
        let n = g.entries.len();
        let d = g.dirs.len();
        if n >= NIL as usize || d >= NIL as usize { return Err("usage namespace exceeds reserved u32 index space".into()); }
        let mut parent_search_valid = true;
        let mut parent_search_deep_valid = true;
        let mut t = Self { budget, freshness: Freshness::with_len(n)?, ..Self::default() };
        t.reserve(n, d)?;
        let staging = d.saturating_mul(9);
        if t.bytes().saturating_add(staging) > budget {
            return Err("usage initialization peak (directory aggregates plus topology staging) exceeds byte budget".into());
        }
        t.file_links.resize(n, FileLinks::default());
        let mut assigned = Vec::new();
        assigned.try_reserve_exact(d).map_err(|_| "usage topology allocation failed")?;
        assigned.resize(d, false);
        let mut roots: Vec<usize> = Vec::new();
        roots.try_reserve(g.volumes.len()).map_err(|_| "usage root staging allocation failed")?;
        t.roots.try_reserve(g.volumes.len()).map_err(|_| "usage root map allocation failed")?;
        for (i, dir) in g.dirs.iter().enumerate() {
            let p = if (dir.parent_idx as usize) < d { dir.parent_idx } else { i as u32 };
            let parent = if p as usize == i { i as u32 } else { p };
            t.dirs.push(DirAgg::empty(parent, 0));
            if parent as usize == i { roots.push(i); }
        }
        if roots.len() != g.volumes.len() {
            return Err("usage identity unresolved: directory roots do not match volumes".into());
        }
        for (root, v) in roots.iter().copied().zip(g.volumes.iter()) {
            t.roots.insert(v.volume_letter, root as u32);
            t.dirs[root].volume_slot = g.volume_slot_of(v.volume_letter).unwrap_or(0) as u8;
            assigned[root] = true;
        }
        // Reuse ancestry staging rather than allocating for each directory.
        let mut path = Vec::new();
        for i in 0..d {
            if assigned[i] { continue; }
            path.clear();
            let mut at = i;
            let slot = loop {
                if assigned[at] { break t.dirs[at].volume_slot; }
                if path.len() > d { return Err("usage directory cycle".into()); }
                path.try_reserve(1).map_err(|_| "usage ancestry staging allocation failed")?;
                path.push(at);
                let p = t.dirs[at].parent as usize;
                if p == at { break 0; }
                at = p;
            };
            for &x in path.iter().rev() {
                t.dirs[x].volume_slot = slot;
                let p=t.dirs[x].parent;
                if g.dirs[x].name_len==0 && (t.dirs[p as usize].parent==p || t.dirs[p as usize].flags & ROOT_ALIAS != 0) {
                    t.dirs[x].flags=ROOT_ALIAS;
                    t.dirs[x].parent=t.logical_parent(p);
                }
                assigned[x] = true;
            }
        }
        // Prefer FileEntry.dir_idx as the authoritative parent, including pre-rebase sentinels.
        for (idx, e) in g.entries.iter().enumerate() {
            if !e.is_dir_entry() { continue; }
            let frn = g.frns.get(idx).copied().unwrap_or(0);
            let Some(letter) = g.volume_letter_u8_for_dir(e.dir_idx) else { continue; };
            if let Some(di) = g.dir_on_volume(letter, frn) {
                let di = di as usize;
                let prior = t.dirs[di].owner_entry;
                if prior != NIL && prior != idx as u32 && !g.entries[prior as usize].is_deleted() {
                    parent_search_valid = false;
                }
                t.dirs[di].owner_entry = idx as u32;
                if t.dirs[di].flags & ROOT_ALIAS != 0 {
                    let root=t.logical_parent(di as u32);
                    t.dirs[root as usize].owner_entry=idx as u32;
                    continue;
                }
                t.dirs[di].flags = if e.is_deleted() { 0 } else { ACTIVE };
                let p = t.logical_parent(e.dir_idx);
                if (p as usize) < d && p as usize != di {
                    t.dirs[di].parent = p;
                }
            }
        }
        for di in 0..d {
            let p = t.dirs[di].parent;
            if p as usize != di && t.dirs[di].active() { t.link_dir(di as u32, p); }
            if t.dirs[di].active() {
                let mut tot = t.dirs[di].totals();
                tot.add(Totals::dir_self());
                t.dirs[di].set_totals(tot)?;
            }
        }
        for (idx, e) in g.entries.iter().enumerate() {
            if e.is_dir_entry() { continue; }
            let p = t.logical_parent(e.dir_idx);
            if (p as usize) >= d { return Err("usage entry volume unresolved".into()); }
            if !e.is_deleted() { t.link_file(idx as u32, p); }
            let unknown = !g.metadata_ready;
            t.freshness.set(idx, u8::from(!unknown));
            if !e.is_deleted() {
                let w = Totals::file(e.size, unknown);
                let mut tot = t.dirs[p as usize].totals();
                tot.add(w);
                t.dirs[p as usize].set_totals(tot)?;
            }
        }
        // Bottom-up: bubble child subtree totals (already include each dir's own dirs=1 and direct files).
        let mut remaining = Vec::new();
        remaining.try_reserve_exact(d).map_err(|_| "usage child-count staging allocation failed")?;
        remaining.resize(d, 0u32);
        for (i, dir) in t.dirs.iter().enumerate() {
            if dir.parent as usize != i { remaining[dir.parent as usize] += 1; }
        }
        let mut leaves: Vec<u32> = Vec::new();
        leaves.try_reserve_exact(d).map_err(|_| "usage leaf staging allocation failed")?;
        leaves.extend(remaining.iter().enumerate().filter(|(_, c)| **c == 0).map(|(i, _)| i as u32));
        let mut seen = 0usize;
        while let Some(id) = leaves.pop() {
            seen += 1;
            let p = t.dirs[id as usize].parent;
            if p as usize == id as usize { continue; }
            let add = if t.dirs[id as usize].active() { t.dirs[id as usize].totals() } else { Totals::default() };
            let parent = &mut t.dirs[p as usize];
            let mut tot = parent.totals();
            tot.add(add);
            parent.set_totals(tot)?;
            remaining[p as usize] -= 1;
            if remaining[p as usize] == 0 { leaves.push(p); }
        }
        if seen != d { return Err("usage graph cycle".into()); }
        // Reuse existing build staging to mark reachable active parents. No
        // additional persistent directory/entry index or path strings are kept.
        assigned.fill(false);
        leaves.clear();
        leaves.extend(t.roots.values().copied());
        while let Some(id) = leaves.pop() {
            if assigned[id as usize] { continue; }
            assigned[id as usize] = true;
            let mut child = t.dirs[id as usize].first_dir;
            while child != NIL {
                leaves.push(child);
                child = t.dirs[child as usize].next_dir;
            }
        }
        for entry in &g.entries {
            if !entry.is_deleted() && !assigned.get(t.logical_parent(entry.dir_idx) as usize).copied().unwrap_or(false) {
                parent_search_deep_valid = false;
                break;
            }
        }
        for &root in t.roots.values() {
            t.dirs[root as usize].flags |= if parent_search_valid { PARENT_SEARCH_VALID } else { 0 }
                | if parent_search_deep_valid { PARENT_SEARCH_DEEP_VALID } else { 0 };
        }
        t.epoch = next_epoch();
        t.generation = 1;
        if t.bytes() > budget { return Err("usage actual capacity exceeds byte budget".into()); }
        Ok(t)
    }
    pub(crate) fn invalidate_parent_search(&mut self, deep_only: bool) {
        let mask = if deep_only { PARENT_SEARCH_DEEP_VALID } else { PARENT_SEARCH_VALID | PARENT_SEARCH_DEEP_VALID };
        for &root in self.roots.values() {
            if let Some(n) = self.dirs.get_mut(root as usize) { n.flags &= !mask; }
        }
    }
    fn logical_parent(&self, p:u32)->u32 {
        match self.dirs.get(p as usize) {Some(n) if n.flags & ROOT_ALIAS != 0=>n.parent,_=>p}
    }
    fn link_dir(&mut self, id: u32, p: u32) {
        let p=self.logical_parent(p);
        let first = self.dirs[p as usize].first_dir;
        self.dirs[id as usize].parent = p;
        self.dirs[id as usize].next_dir = first;
        self.dirs[id as usize].prev_dir = NIL;
        if first != NIL { self.dirs[first as usize].prev_dir = id; }
        self.dirs[p as usize].first_dir = id;
    }
    fn unlink_dir(&mut self, id: u32) {
        let n = self.dirs[id as usize];
        if n.parent == NIL || n.parent as usize == id as usize { return; }
        if n.prev_dir==NIL && n.next_dir==NIL && self.dirs[n.parent as usize].first_dir!=id{return;}
        if n.prev_dir == NIL { self.dirs[n.parent as usize].first_dir = n.next_dir; }
        else { self.dirs[n.prev_dir as usize].next_dir = n.next_dir; }
        if n.next_dir != NIL { self.dirs[n.next_dir as usize].prev_dir = n.prev_dir; }
        self.dirs[id as usize].next_dir = NIL;
        self.dirs[id as usize].prev_dir = NIL;
    }
    fn link_file(&mut self, idx: u32, p: u32) {
        let p=self.logical_parent(p);
        let first = self.dirs[p as usize].first_file;
        self.file_links[idx as usize] = FileLinks { next: first, prev: NIL };
        if first != NIL { self.file_links[first as usize].prev = idx; }
        self.dirs[p as usize].first_file = idx;
    }
    fn unlink_file(&mut self, idx: u32, p: u32) {
        let p=self.logical_parent(p);
        let n = self.file_links[idx as usize];
        if p == NIL { return; }
        if n.prev==NIL && n.next==NIL && self.dirs[p as usize].first_file!=idx{return;}
        if n.prev == NIL { self.dirs[p as usize].first_file = n.next; }
        else { self.file_links[n.prev as usize].next = n.next; }
        if n.next != NIL { self.file_links[n.next as usize].prev = n.prev; }
        self.file_links[idx as usize] = FileLinks::default();
    }
    fn ancestors(&mut self, mut p: u32, w: Totals, add: bool) {
        p=self.logical_parent(p);
        let mut guard = 0usize;
        while p != NIL {
            self.ancestor_visits = self.ancestor_visits.saturating_add(1);
            let n = &mut self.dirs[p as usize];
            let mut tot = n.totals();
            if add { tot.add(w) } else { tot.sub(w) }
            if let Err(error)=n.set_totals(tot) { self.error=Some(error); return; }
            let active = n.active();
            let next = n.parent;
            if !active || next == p { break; }
            p = next;
            guard += 1;
            if guard > self.dirs.len() { break; }
        }
    }
    fn visible_dir(&self, mut id: u32) -> bool {
        id=self.logical_parent(id);
        let mut guard = 0usize;
        while (id as usize) < self.dirs.len() {
            let n = self.dirs[id as usize];
            if !n.active() { return false; }
            if n.parent == id { return true; }
            id = n.parent;
            guard += 1;
            if guard > self.dirs.len() { return false; }
        }
        false
    }
    fn item_dir(&self, g: &IndexStore, di: u32) -> Result<UsageItem, String> {
        let n = self.dirs.get(di as usize).ok_or("unknown usage directory")?;
        let (name, id) = if n.owner_entry != NIL {
            let e = g.entries.get(n.owner_entry as usize).ok_or("usage entry unavailable")?;
            (String::from_utf8_lossy(g.name_bytes(e)).into_owned(), n.owner_entry)
        } else {
            let d = g.dirs.get(di as usize).ok_or("usage directory unavailable")?;
            let start = d.name_offset as usize;
            let bytes = g.names_buf.get(start..start + d.name_len as usize).unwrap_or(b"");
            (String::from_utf8_lossy(bytes).into_owned(), di)
        };
        Ok(UsageItem { id, name, directory: true, logical: n.logical, files: n.files as u64, directories: n.dirs as u64, unknown: n.unknown as u64 })
    }
    fn item_file(&self, g: &IndexStore, idx: u32, size_of_file: &impl Fn(usize) -> u64) -> Result<UsageItem, String> {
        let e = g.entries.get(idx as usize).ok_or("usage entry unavailable")?;
        Ok(UsageItem {
            id: idx,
            name: String::from_utf8_lossy(g.name_bytes(e)).into_owned(),
            directory: false,
            logical: size_of_file(idx as usize),
            files: 1,
            directories: 0,
            unknown: u64::from(self.freshness.get(idx as usize) != 1),
        })
    }
    fn resolve(&self, g: &IndexStore, path: &str, size_of_file: &impl Fn(usize) -> u64) -> Result<u32, String> {
        let path = path.replace('/', "\\");
        let path = path.trim_end_matches('\\');
        if g.excluded_dirs.iter().any(|e| {
            let e=e.trim_end_matches(char::from(92));
            path.get(..e.len()).is_some_and(|p|p.eq_ignore_ascii_case(e)) && (path.len()==e.len() || path.as_bytes().get(e.len())==Some(&92))
        }) {return Err("path explicitly excluded from indexed scope".into())}
        let (v, tail) = g.volumes.iter().find_map(|v| {
            let root = v.root_prefix.trim_end_matches('\\');
            if path.eq_ignore_ascii_case(root) { Some((v, "")) }
            else if path.len() > root.len()
                && path.get(..root.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(root))
                && path.as_bytes().get(root.len()) == Some(&b'\\')
            { Some((v, &path[root.len() + 1..])) }
            else { None }
        }).ok_or("path is outside indexed volumes")?;
        let mut di = *self.roots.get(&v.volume_letter).ok_or("volume root unavailable")?;
        for part in tail.split('\\').filter(|s| !s.is_empty()) {
            if part == "." || part == ".." { return Err("relative path components not allowed".into()); }
            let mut found = None;
            let mut child = self.dirs[di as usize].first_dir;
            while child != NIL {
                if self.dirs[child as usize].active() && {
                    let n=&self.dirs[child as usize];
                    let bytes=if n.owner_entry!=NIL {g.name_bytes(&g.entries[n.owner_entry as usize])} else {let d=&g.dirs[child as usize];&g.names_buf[d.name_offset as usize..d.name_offset as usize+d.name_len as usize]};
                    String::from_utf8_lossy(bytes).eq_ignore_ascii_case(part)
                } {
                    found = Some(child);
                    break;
                }
                child = self.dirs[child as usize].next_dir;
            }
            di = found.ok_or("path not present in indexed scope")?;
        }
        let _ = size_of_file;
        if !self.visible_dir(di) { return Err("path excluded/deleted".into()); }
        Ok(di)
    }
    fn page_after(&self, _g:&IndexStore,root:u32,after:Option<u32>)->Result<(Option<u32>,Option<u32>),String>{
        let Some(token)=after else{return Ok((some_if(self.dirs[root as usize].first_dir),some_if(self.dirs[root as usize].first_file)))};
        let cursor=self.page_cursors.get(token as usize%CURSOR_SLOTS).and_then(|c|*c)
            .filter(|c|c.token==token&&c.epoch==self.epoch&&c.generation==self.generation&&c.root==root)
            .ok_or("usage page cursor expired or outside scope; restart pagination")?;
        Ok((cursor.dir,cursor.file))
    }
    fn continuation(&mut self,root:u32,dir:Option<u32>,file:Option<u32>)->Result<Option<u32>,String>{
        if dir.is_none()&&file.is_none(){return Ok(None)}
        if self.page_cursors.is_empty(){
            let extra=CURSOR_SLOTS*std::mem::size_of::<Option<PageContinuation>>();
            if self.bytes().saturating_add(extra)>self.budget{return Err("usage cursor resource budget exceeded".into())}
            self.page_cursors.try_reserve_exact(CURSOR_SLOTS).map_err(|_|"usage cursor allocation failed")?;
            self.page_cursors.resize(CURSOR_SLOTS,None);
        }
        let seq=CURSOR_SEQUENCE.fetch_add(1,Ordering::Relaxed);
        let token=u32::try_from(seq).map_err(|_|"usage cursor token space exhausted; restart process")?;
        self.page_cursors[token as usize%CURSOR_SLOTS]=Some(PageContinuation{token,epoch:self.epoch,generation:self.generation,root,dir,file});
        Ok(Some(token))
    }
    fn invalidate_ticket(&mut self, idx: usize) { self.tickets.remove(&idx); }
    fn issue_ticket(&mut self, idx: usize) -> Result<u64, String> {
        let cap=self.tickets.capacity();
        let predicted=if self.tickets.len()<cap || self.tickets.contains_key(&idx) {cap} else {cap.max(3).saturating_mul(2)};
        let extra=predicted.saturating_sub(cap).saturating_mul(32);
        let ticket_budget=(self.budget/8).min(8*1024*1024);
        if predicted.saturating_mul(32)>ticket_budget || self.bytes().saturating_add(extra)>self.budget {
            return Err("metadata in-flight ticket byte budget exhausted; producer must drain or retry".into());
        }
        if !self.tickets.contains_key(&idx) {self.tickets.try_reserve(1).map_err(|_| "metadata ticket allocation failed")?;}
        self.ticket_seq=self.ticket_seq.checked_add(1).ok_or("metadata ticket sequence exhausted")?;
        self.tickets.insert(idx,self.ticket_seq);
        Ok(self.ticket_seq)
    }
    fn apply_file_delta(&mut self, parent: u32, old: Totals, new: Totals, active_old: bool, active_new: bool) {
        if active_old { self.ancestors(parent, old, false); }
        if active_new { self.ancestors(parent, new, true); }
        self.generation = self.generation.saturating_add(1);
    }
    fn ensure_dir_slot(&mut self, di: u32, parent: u32, slot: u8) -> Result<(), String> {
        if (di as usize) < self.dirs.len() { return Ok(()); }
        let need = di as usize + 1 - self.dirs.len();
        self.reserve(0, need)?;
        while self.dirs.len() <= di as usize {
            self.dirs.push(DirAgg::empty(parent, slot));
        }
        Ok(())
    }
    pub(crate) fn metadata_sync(&mut self, parent: u32, old_size: u64, new_size: u64, was_unknown: bool, idx: usize) {
        if self.epoch == 0 || self.error.is_some() { return; }
        self.freshness.set(idx, 1);
        self.apply_file_delta(parent, Totals::file(old_size, was_unknown), Totals::file(new_size, false), true, true);
        self.invalidate_ticket(idx);
    }
    pub(crate) fn overlay_delta(&mut self, parent: u32, old_size: u64, new_size: u64, was_unknown: bool, idx: usize) {
        self.metadata_sync(parent, old_size, new_size, was_unknown, idx);
    }
    #[cfg(test)]
    pub(crate) fn payload_stats(&self) -> (usize, usize, usize, usize, usize) {
        (self.dirs.len(), self.dirs.capacity(), self.file_links.len(), self.file_links.capacity(), self.bytes())
    }
    #[cfg(test)]
    pub(crate) fn visits(&self) -> u64 { self.ancestor_visits }
}
fn some_if(v: u32) -> Option<u32> { if v == NIL { None } else { Some(v) } }

#[derive(Clone, Copy)]
struct Snap {
    idx: Option<u32>,
    is_dir: bool,
    parent: u32,
    di: Option<u32>,
    size: u64,
    deleted: bool,
}
enum Prep {
    Noop,
    Apply { scope: ResolvedMutation, new_entry: bool, new_dir: bool, name_bytes: usize },
    Delete { entry: u32 },
}

impl SearchEngine {
    pub fn usage_budget(&self) -> usize { self.usage.lock().budget }
    pub fn usage_available(&self) -> bool {
        let r = self.usage.lock();
        r.epoch != 0 && r.error.is_none()
    }
    pub fn enable_usage(&self, budget: usize) -> Result<(), String> {
        let g = self.store.read();
        let mut usage = self.usage.lock();
        // Reinitialization must not retain the old sidecar throughout the next build.
        *usage = UsageRuntime { budget, ..Default::default() };
        match UsageRuntime::build(&g, budget) {
            Ok(tree) => { *usage = tree; Ok(()) }
            Err(error) => { usage.error = Some(error.clone()); Err(error) }
        }
    }
    fn effective_size(&self, g: &IndexStore, idx: usize) -> u64 {
        self.metadata_overlay.get(idx).map(|t| t.0).unwrap_or_else(|| g.entries.get(idx).map(|e| e.size).unwrap_or(0))
    }
    pub fn usage_page(&self, path: &str, expected: Option<(u64, u64)>, after: Option<u32>, max_bytes: usize) -> Result<UsagePage, String> {
        let g = self.store.read();
        let mut r = self.usage.lock();
        if let Some(e) = &r.error { return Err(e.clone()); }
        if r.epoch == 0 { return Err("indexed usage unavailable / initialization required".into()); }
        if after.is_some() && expected.is_none(){return Err("usage cursor requires epoch/generation; restart pagination".into());}
        if expected.is_some_and(|v| v != (r.epoch, r.generation)) { return Err("usage generation changed; restart page".into()); }
        let size_of_file = |idx: usize| self.effective_size(&g, idx);
        let root = r.resolve(&g, path, &size_of_file)?;
        let (mut next_dir, mut next_file) = r.page_after(&g, root, after)?;
        let mut children = Vec::new();
        let mut bytes = 0usize;
        let mut examined = 0usize;
        let budget = max_bytes.clamp(1024, 256 * 1024);

        while let Some(d) = next_dir {
            examined += std::mem::size_of::<DirAgg>();
            if examined > SCAN_WORK_BYTES { break; }
            let n = r.dirs[d as usize];
            let following = some_if(n.next_dir);
            if n.active() {
                let item = r.item_dir(&g, d)?;
                let cost = item.name.bytes().map(|b|if b<32{6}else if b==34||b==92{2}else{1}).sum::<usize>()+256;
                if cost>budget && children.is_empty(){return Err("usage child exceeds page output budget; request a larger page".into())}
                if bytes + cost > budget { next_dir = Some(d); break; }
                children.try_reserve(1).map_err(|_| "usage page allocation failed")?;
                bytes += cost;
                children.push(item);
            }
            next_dir = following;
        }
        if next_dir.is_none() {
            while let Some(f) = next_file {
                examined += std::mem::size_of::<FileLinks>();
                if examined > SCAN_WORK_BYTES { break; }
                let following = some_if(r.file_links[f as usize].next);
                let e = &g.entries[f as usize];
                if !e.is_deleted() {
                    let item = r.item_file(&g, f, &size_of_file)?;
                    let cost = item.name.bytes().map(|b|if b<32{6}else if b==34||b==92{2}else{1}).sum::<usize>()+256;
                    if cost>budget && children.is_empty(){return Err("usage child exceeds page output budget; request a larger page".into())}
                    if bytes + cost > budget { next_file = Some(f); break; }
                    children.try_reserve(1).map_err(|_| "usage page allocation failed")?;
                    bytes += cost;
                    children.push(item);
                }
                next_file = following;
            }
        }
        let next=r.continuation(root,next_dir,next_file)?;
        Ok(UsagePage {
            epoch: r.epoch,
            generation: r.generation,
            root: r.item_dir(&g, root)?,
            children,
            next,
            resident_bytes: r.bytes(),
            state: "indexed logical bytes; canonical indexed paths; allocation not measured; journal/metadata freshness tracked separately".into(),
        })
    }
    pub fn metadata_ticket(&self, idx: usize) -> Option<MetadataTicket> {
        let g = self.store.read();
        let mut r = self.usage.lock();
        if r.epoch == 0 || r.error.is_some() { return None; }
        let e = g.entries.get(idx)?;
        if e.is_deleted() || e.is_dir_entry() { return None; }
        let frn = *g.frns.get(idx)?;
        let seq = match r.issue_ticket(idx) {
            Ok(seq)=>seq,
            Err(error)=>{ self.set_backfill_error(Some(error)); return None; }
        };
        Some(MetadataTicket { entry: idx, epoch: r.epoch, seq, frn })
    }
    pub fn metadata_ticket_by_id(&self, volume: u8, frn: u64) -> Option<MetadataTicket> {
        let g = self.store.read();
        let idx = g.entry_on_volume(volume, frn)? as usize;
        drop(g);
        self.metadata_ticket(idx)
    }
    pub fn commit_metadata(&self, t: MetadataTicket, size: u64, mtime: u64, ctime: u64) -> Result<bool, String> {
        let mut g = self.store.write();
        let mut r = self.usage.lock();
        if r.epoch == 0 || r.error.is_some() || r.epoch != t.epoch || g.frns.get(t.entry) != Some(&t.frn) {
            return Ok(false);
        }
        if r.tickets.get(&t.entry) != Some(&t.seq) { return Ok(false); }
        let e = g.entries.get(t.entry).ok_or("usage entry unavailable")?;
        if e.is_deleted() || e.is_dir_entry() { return Ok(false); }
        let parent = e.dir_idx;
        let old = Totals::file(self.effective_size(&g, t.entry), r.freshness.get(t.entry) != 1);
        // Ticket validation, store mutation and overlay publication share one lock.
        // Do not confuse incomplete metadata coverage with an inconsistent snapshot.
        self.mirror_metadata_for_persistence(&mut g,t.entry,size,mtime,ctime).map_err(|e| e.to_string())?;
        self.metadata_overlay.put(t.entry, size, crate::index::filetime_to_unix_secs(mtime), crate::index::filetime_to_unix_secs(ctime));
        r.freshness.set(t.entry, 1);
        r.apply_file_delta(parent, old, Totals::file(size, false), true, true);
        r.invalidate_ticket(t.entry);
        self.bump_revision();
        drop(g);
        Ok(true)
    }
    pub fn mark_metadata_dirty(&self, volume: u8, frn: u64) {
        let g = self.store.read();
        let mut r = self.usage.lock();
        if r.epoch == 0 || r.error.is_some() { return; }
        let Some(idx) = g.entry_on_volume(volume, frn) else { return; };
        let idx = idx as usize;
        let Some(e) = g.entries.get(idx) else { return; };
        if e.is_dir_entry() || e.is_deleted() { return; }
        let parent = e.dir_idx;
        let size = self.effective_size(&g, idx);
        let old = Totals::file(size, r.freshness.get(idx) != 1);
        r.freshness.set(idx, 2);
        r.apply_file_delta(parent, old, Totals::file(size, true), true, true);
        r.invalidate_ticket(idx);
    }
    pub fn apply_volume_change(&self, volume: u8, ev: &ChangeEvent) -> Result<(), String> {
        self.apply_volume_events(volume, std::slice::from_ref(ev), None, None).map(|_| ())
    }
    /// Namespace cursor advances only after every event is applied, excluded, or parked.
    /// Fatal failures roll the batch back and leave the watermark unchanged.
    pub fn apply_volume_batch(&self, volume: u8, events: &[ChangeEvent], journal_id: u64, next_usn: u64) -> Result<(), String> {
        self.apply_volume_events(volume, events, Some((journal_id, next_usn)), None).map(|_| ())
    }
    /// A resolved/retried native batch must still belong to this exact RAM index
    /// generation and volume when acquiring the authoritative store write lock.
    pub fn apply_volume_batch_bound(&self,volume:u8,events:&[ChangeEvent],journal_id:u64,next_usn:u64,epoch:u64,serial:u32)->Result<(),String>{
        self.apply_volume_events(volume,events,Some((journal_id,next_usn)),Some((epoch,serial))).map(|_|())
    }
    fn apply_volume_events(&self, volume: u8, events: &[ChangeEvent], cursor: Option<(u64, u64)>, binding:Option<(u64,u32)>) -> Result<bool, String> {
        let lock_started = std::time::Instant::now();
        let mut g = self.store.write();
        let write_wait = lock_started.elapsed();
        let apply_started = std::time::Instant::now();
        if let Some((epoch,serial))=binding {
            if self.usage.lock().epoch!=epoch || !g.volumes.iter().any(|v|v.volume_letter==volume&&v.volume_serial==serial){return Err("resolved namespace index generation/volume changed; cursor retained".into());}
        }
        if let Some((journal_id, next_usn)) = cursor {
            let v = g.volumes.iter().find(|v| v.volume_letter == volume).ok_or("batch volume unavailable")?;
            if v.usn_journal_id != 0 && v.usn_journal_id != journal_id {
                return Err("journal epoch changed; rebuild required".into());
            }
            if next_usn < v.last_usn { return Err("cursor regression rejected".into()); }
        }
        // Idle heartbeats do not repeatedly replay unresolved namespace events.
        if events.is_empty() && self.has_pending_namespace(volume) { return Ok(false); }
        let prior_orphans = self.orphans.lock().take_volume(volume);
        let mut remaining = prior_orphans.clone();
        remaining.extend(events.iter().cloned());
        let excluded_before = self.usage.lock().excluded_ids.clone();
        let mut ck = g.namespace_checkpoint();
        if let Err(error) = g.reserve_namespace_checkpoint(&mut ck, remaining.len()) {
            self.restore_orphans(volume, prior_orphans);
            return Err(error);
        }
        let mut applied_snaps: Vec<(Snap, Snap, ChangeEvent)> = Vec::new();
        // Overlay writes are transactional too: publish only after namespace commit.
        let mut metadata_after: HashMap<usize, (u64, u32, u32)> = HashMap::new();
        let mut leftover: Vec<(u64, ChangeEvent)> = Vec::new();
        while !remaining.is_empty() {
            let before = remaining.len();
            let mut next_round = Vec::new();
            let mut blocked_ids = HashSet::new();
            // A later terminal delete proves the earlier unresolved namespace
            // prefix for this exact volume-qualified FRN no longer exists. Keep
            // per-batch cutoffs so later recreation is not discarded; one linear
            // retain below avoids rescanning the pending queue for every delete.
            let mut superseded_before: HashMap<u64, (usize,Option<usize>)> = HashMap::new();
            for ev in remaining {
                let identity = OrphanQueue::identity(&ev);
                g.checkpoint_identity(&mut ck, volume, identity);
                if matches!(&ev, ChangeEvent::Delete { .. }) && blocked_ids.remove(&identity) {
                    if !superseded_before.contains_key(&identity) {
                        if let Err(e) = superseded_before.try_reserve(1) {
                            g.rollback_namespace(ck);
                            self.restore_orphans(volume, prior_orphans);
                            self.usage.lock().excluded_ids = excluded_before;
                            return Err(format!("terminal namespace recovery allocation failed: {e}"));
                        }
                    }
                    superseded_before.insert(identity, (next_round.len(),None));
                }
                // A later fully resolved placement supersedes only earlier unresolved
                // namespace positions, NOT metadata for this surviving identity.
                if blocked_ids.contains(&identity) && matches!(&ev,ChangeEvent::Rename{..}|ChangeEvent::Create{..}) && matches!(self.prepare_namespace(&g,volume,&ev),Ok(Prep::Apply{..})) {
                    if !superseded_before.contains_key(&identity) {
                        if let Err(e)=superseded_before.try_reserve(1){g.rollback_namespace(ck);self.restore_orphans(volume,prior_orphans);self.usage.lock().excluded_ids=excluded_before;return Err(format!("namespace rebind allocation failed: {e}"));}
                    }
                    blocked_ids.remove(&identity);superseded_before.entry(identity).or_insert((0,None)).1=Some(next_round.len());
                }
                if blocked_ids.contains(&identity) {
                    next_round.push((0, ev));
                    continue;
                }
                match self.prepare_namespace(&g, volume, &ev) {
                    Ok(Prep::Noop) => {}
                    Ok(Prep::Delete { entry }) => {
                        let mut snap = self.snap_entry(&g, volume, Some(entry));
                        if let Some(value) = metadata_after.get(&(entry as usize)) { snap.size = value.0; }
                        g.checkpoint_touch(&mut ck, entry);
                        g.delete_entry(entry);
                        let mut after = self.snap_identity(&g, volume, &ev);
                        if let Some(idx) = after.idx.map(|i| i as usize) {
                            let entry = &g.entries[idx];
                            let base = metadata_after.get(&idx).copied().or_else(|| self.metadata_overlay.get(idx))
                                .unwrap_or((entry.size, entry.mtime, entry.ctime));
                            let updated = match &ev {
                                ChangeEvent::Create { .. } => Some((entry.size, entry.mtime, entry.ctime)),
                                ChangeEvent::DataOrMeta { size, mtime, ctime, .. } => Some((
                                    size.unwrap_or(base.0),
                                    mtime.map(crate::index::filetime_to_unix_secs).unwrap_or(base.1),
                                    ctime.map(crate::index::filetime_to_unix_secs).unwrap_or(base.2))),
                                _ => None,
                            };
                            if !entry.is_dir_entry() {
                                if let Some(value) = updated { metadata_after.insert(idx, value); }
                                if let Some(value) = metadata_after.get(&idx) { after.size = value.0; }
                            }
                        }
                        applied_snaps.push((snap, after, ev));
                    }
                    Ok(Prep::Apply { scope, new_entry, new_dir, name_bytes }) => {
                        if let Some(idx) = scope.entry { g.checkpoint_touch(&mut ck, idx); }
                        if let Err(e) = g.try_reserve_namespace_change(name_bytes, new_entry, new_dir) {
                            g.rollback_namespace(ck);
                            self.restore_orphans(volume, prior_orphans);
                            self.usage.lock().excluded_ids = excluded_before;
                            return Err(e);
                        }
                        let mut snap = self.snap_identity(&g, volume, &ev);
                        if let Some(value) = snap.idx.and_then(|i| metadata_after.get(&(i as usize))) { snap.size = value.0; }
                        if let Err(e) = g.apply_change_event_resolved(&ev, Some(&scope)) {
                            g.rollback_namespace(ck);
                            self.restore_orphans(volume, prior_orphans);
                            self.usage.lock().excluded_ids = excluded_before;
                            return Err(e.to_string());
                        }
                        let mut after = self.snap_identity(&g, volume, &ev);
                        if let Some(idx) = after.idx.map(|i| i as usize) {
                            let entry = &g.entries[idx];
                            let base = metadata_after.get(&idx).copied().or_else(|| self.metadata_overlay.get(idx))
                                .unwrap_or((entry.size, entry.mtime, entry.ctime));
                            let updated = match &ev {
                                ChangeEvent::Create { .. } => Some((entry.size, entry.mtime, entry.ctime)),
                                ChangeEvent::DataOrMeta { size, mtime, ctime, .. } => Some((
                                    size.unwrap_or(base.0),
                                    mtime.map(crate::index::filetime_to_unix_secs).unwrap_or(base.1),
                                    ctime.map(crate::index::filetime_to_unix_secs).unwrap_or(base.2))),
                                _ => None,
                            };
                            if !entry.is_dir_entry() {
                                if let Some(value) = updated { metadata_after.insert(idx, value); }
                                if let Some(value) = metadata_after.get(&idx) { after.size = value.0; }
                            }
                        }
                        applied_snaps.push((snap, after, ev));
                    }
                    Err(e) if e.starts_with("parent identity missing:") => {
                        let parent = e.rsplit(':').next().and_then(|s| s.parse().ok()).unwrap_or(0);
                        blocked_ids.insert(identity);
                        next_round.push((parent, ev));
                    }
                    Err(e) => {
                        g.rollback_namespace(ck);
                        self.restore_orphans(volume, prior_orphans);
                            self.usage.lock().excluded_ids = excluded_before;
                        return Err(e);
                    }
                }
            }
            let mut position = 0;
            next_round.retain(|(_, ev)| {
                let keep = superseded_before.get(&OrphanQueue::identity(ev))
                    .map_or(true, |(terminal,rebind)| position>=*terminal && rebind.map_or(true,|cutoff| position>=cutoff || matches!(ev,ChangeEvent::DataOrMeta{..})));
                position += 1;
                keep
            });
            if next_round.len() == before && !next_round.is_empty() {
                if cursor.is_none() {
                    g.rollback_namespace(ck);
                    self.restore_orphans(volume, prior_orphans);
                            self.usage.lock().excluded_ids = excluded_before;
                    return Err("parent identity missing; batch not committed".into());
                }
                leftover = next_round;
                break;
            }
            remaining = next_round.into_iter().map(|(_, ev)| ev).collect();
        }
        if !leftover.is_empty() {
            let mut orphans = self.orphans.lock();
            for (parent, ev) in leftover {
                if let Err(e) = orphans.park(volume, parent, ev) {
                    drop(orphans);
                    g.rollback_namespace(ck);
                    self.restore_orphans(volume, prior_orphans);
                            self.usage.lock().excluded_ids = excluded_before;
                    return Err(e);
                }
            }
        }
        if let Some((journal_id, next_usn)) = cursor.filter(|_| !self.has_pending_namespace(volume)) {
            let v = g.volumes.iter_mut().find(|v| v.volume_letter == volume).ok_or("batch volume unavailable")?;
            v.usn_journal_id = journal_id;
            v.last_usn = next_usn;
        }
        for (&idx, &(size, mtime, ctime)) in &metadata_after {
            if self.metadata_drain_active() {
                g.patch_entry_metadata(idx, size, crate::index::unix_secs_to_filetime(mtime), crate::index::unix_secs_to_filetime(ctime)).map_err(|e| e.to_string())?;
            }
            self.metadata_overlay.put(idx, size, mtime, ctime);
        }
        if let Err(e) = self.fold_snaps(&g, volume, &applied_snaps) {
            let mut r = self.usage.lock();
            if r.epoch != 0 { r.error = Some(e); }
        }
        if !applied_snaps.is_empty() { self.bump_revision(); }
        drop(g);
        if apply_started.elapsed().as_millis() > 25 || write_wait.as_millis() > 25 {
            crate::progress!("watch_batch events={} applied={} write_wait_ms={} hold_ms={}", events.len(), applied_snaps.len(), write_wait.as_millis(), apply_started.elapsed().as_millis());
        }
        Ok(!applied_snaps.is_empty())
    }
    fn snap_identity(&self, g: &IndexStore, volume: u8, ev: &ChangeEvent) -> Snap {
        let frn = match ev {
            ChangeEvent::Create { entry } => entry.file_id,
            ChangeEvent::CreatePending { file_id, .. } | ChangeEvent::Rename { file_id, .. }
            | ChangeEvent::Delete { file_id } | ChangeEvent::DataOrMeta { file_id, .. } => *file_id,
        };
        self.snap_entry(g, volume, g.entry_on_volume(volume, frn))
    }
    fn snap_entry(&self, g: &IndexStore, volume: u8, idx: Option<u32>) -> Snap {
        let Some(idx) = idx else {
            return Snap { idx: None, is_dir: false, parent: NIL, di: None, size: 0, deleted: true };
        };
        let e = &g.entries[idx as usize];
        Snap {
            idx: Some(idx), is_dir: e.is_dir_entry(), parent: e.dir_idx,
            di: if e.is_dir_entry() { g.dir_on_volume(volume, g.frns[idx as usize]) } else { None },
            size: self.effective_size(g, idx as usize), deleted: e.is_deleted(),
        }
    }
    fn note_excluded_identity(&self, volume: u8, frn: u64) -> Result<(), String> {
        self.note_excluded_identity_with_budget(volume, frn, 4 * 1024 * 1024)
    }
    fn note_excluded_identity_with_budget(&self, volume: u8, frn: u64, budget: usize) -> Result<(), String> {
        let mut usage = self.usage.lock();
        let ids = &mut usage.excluded_ids;
        if ids.contains(&(volume, frn)) { return Ok(()); }
        if ids.len().saturating_add(1).saturating_mul(32) > budget {
            return Err("excluded-identity resource budget exceeded; namespace batch must replay after recovery".into());
        }
        if ids.len() == ids.capacity() && ids.capacity().max(3).saturating_mul(2).saturating_mul(32) > budget {
            return Err("excluded-identity growth would exceed resource budget; cursor retained".into());
        }
        ids.try_reserve(1).map_err(|_| "excluded-identity allocation failed")?;
        if ids.capacity().saturating_mul(32) > budget {
            return Err("excluded-identity capacity budget exceeded; cursor retained".into());
        }
        ids.insert((volume, frn));
        Ok(())
    }
    fn prepare_namespace(&self, g: &IndexStore, volume: u8, ev: &ChangeEvent) -> Result<Prep, String> {
        let extended = match ev {
            ChangeEvent::Create { entry } => entry.file_id_128,
            ChangeEvent::CreatePending { file_id_128, .. } => *file_id_128,
            _ => None,
        };
        if extended.is_some_and(|id| id[8..].iter().any(|&b| b != 0)) {
            return Err("full 128-bit identity binding unavailable; refusing truncated-ID mutation".into());
        }
        let (frn, parent, name, is_new_dir) = match ev {
            ChangeEvent::Create { entry } => (entry.file_id, Some(entry.parent_id), Some(entry.name.as_str()), entry.is_dir),
            ChangeEvent::CreatePending { file_id, parent_id, name, is_dir, .. } => (*file_id, Some(*parent_id), Some(name.as_str()), *is_dir),
            ChangeEvent::Rename { file_id, new_parent_id, new_name } => (*file_id, Some(*new_parent_id), Some(new_name.as_str()), false),
            ChangeEvent::Delete { file_id } => (*file_id, None, None, false),
            ChangeEvent::DataOrMeta { file_id, .. } => (*file_id, None, None, false),
        };
        if let Some(p) = parent {
            let parent_excluded = self.usage.lock().excluded_ids.contains(&(volume, p));
            if parent_excluded {
                if is_new_dir { self.note_excluded_identity(volume, frn)?; }
                if let Some(idx) = g.entry_on_volume(volume, frn) { return Ok(Prep::Delete { entry: idx }); }
                return Ok(Prep::Noop);
            }
        }
        let parent_dir = if let Some(p) = parent {
            match g.dir_on_volume(volume, p) {
                Some(di) => Some(di),
                None => return Err(format!("parent identity missing:{p}")),
            }
        } else { None };
        if let (Some(di), Some(name)) = (parent_dir, name) {
            if g.excluded_under(volume, di, name) {
                if is_new_dir { self.note_excluded_identity(volume, frn)?; }
                if let Some(idx) = g.entry_on_volume(volume, frn) { return Ok(Prep::Delete { entry: idx }); }
                return Ok(Prep::Noop);
            }
        }
        let entry = g.entry_on_volume(volume, frn);
        let directory = g.dir_on_volume(volume, frn);
        if let (Some(di), Some(p)) = (directory, parent_dir) {
            let mut at = p;
            let mut guard = 0;
            while (at as usize) < g.dirs.len() {
                if at == di { return Err("directory move cycle".into()); }
                let n = g.dirs[at as usize].parent_idx;
                if n == at { break; }
                at = n;
                guard += 1;
                if guard > g.dirs.len() { return Err("directory move cycle".into()); }
            }
        }
        let name_bytes = match ev {
            ChangeEvent::Create { entry } => entry.name.len(),
            ChangeEvent::CreatePending { name, .. } => name.len(),
            ChangeEvent::Rename { new_name, .. } => new_name.len(),
            _ => 0,
        };
        let new_entry = entry.is_none() && !matches!(ev, ChangeEvent::Delete { .. } | ChangeEvent::DataOrMeta { .. });
        let new_dir = directory.is_none() && is_new_dir && new_entry;
        Ok(Prep::Apply {
            scope: ResolvedMutation { entry, parent: parent_dir.unwrap_or(0), directory },
            new_entry, new_dir, name_bytes,
        })
    }
    /// Bounded, name-free diagnostics for a pinned namespace watermark.
    /// Inspect at most 64 queued records; never clone their names or paths.
    pub fn pending_namespace_diagnostics(&self, volume: u8) -> String {
        let g = self.store.read();
        let o = self.orphans.lock();
        let mut sampled = 0;
        let (mut creates, mut renames, mut deletes, mut other) = (0, 0, 0, 0);
        let (mut absent_parent, mut deleted_parent, mut directory_mismatch, mut live_parent) = (0, 0, 0, 0);
        let mut known_identity = 0;
        let detailed = std::env::var_os("FINDX2_PENDING_ID_DIAGNOSTICS").is_some();
        let mut identities = String::new();
        for (_, _, ev) in o.ordered.iter().filter(|(v, _, _)| *v == volume).take(64) {
            sampled += 1;
            if g.entry_on_volume(volume, OrphanQueue::identity(ev)).is_some() { known_identity += 1; }
            let parent = match ev {
                ChangeEvent::Create { entry } => { creates += 1; Some(entry.parent_id) }
                ChangeEvent::CreatePending { parent_id, .. } => { creates += 1; Some(*parent_id) }
                ChangeEvent::Rename { new_parent_id, .. } => { renames += 1; Some(*new_parent_id) }
                ChangeEvent::Delete { .. } => { deletes += 1; None }
                _ => { other += 1; None }
            };
            if detailed {
                use std::fmt::Write;
                let id = OrphanQueue::identity(ev);
                let kind = match ev { ChangeEvent::Create { .. } => "create", ChangeEvent::CreatePending { .. } => "create_pending", ChangeEvent::Rename { .. } => "rename", ChangeEvent::Delete { .. } => "delete", _ => "metadata" };
                let state = g.entry_on_volume(volume, id).map(|i| if g.entries[i as usize].is_deleted() { "deleted" } else { "live" }).unwrap_or("absent");
                let _ = write!(identities, " [vol={} type={} id={} seq={} parent={} pseq={} state={}]", volume, kind, id, id >> 48, parent.unwrap_or(0), parent.unwrap_or(0) >> 48, state);
            }
            if let Some(parent) = parent {
                match g.entry_on_volume(volume, parent) {
                    None => absent_parent += 1,
                    Some(i) if g.entries[i as usize].is_deleted() => deleted_parent += 1,
                    Some(_) if g.dir_on_volume(volume, parent).is_none() => directory_mismatch += 1,
                    Some(_) => live_parent += 1,
                }
            }
        }
        format!("sample={sampled}/64 creates={creates} renames={renames} deletes={deletes} other={other} absent_parent={absent_parent} deleted_parent={deleted_parent} directory_mismatch={directory_mismatch} live_parent={live_parent} known_identity={known_identity}{identities}")
    }
    pub fn namespace_identity_live(&self,volume:u8,id:u64)->bool {let g=self.store.read();g.entry_on_volume(volume,id).is_some_and(|i|!g.entries[i as usize].is_deleted())}
    pub fn namespace_parent_resolvable(&self,volume:u8,mut parent:u64,events:&[ChangeEvent])->bool {
        let g=self.store.read();
        for _ in 0..64 {
            if g.dir_on_volume(volume,parent).is_some(){return true;}
            let next=events.iter().find_map(|e|match e{ChangeEvent::CreatePending{file_id,parent_id,is_dir:true,..} if *file_id==parent=>Some(*parent_id),ChangeEvent::Create{entry} if entry.is_dir && entry.file_id==parent=>Some(entry.parent_id),_=>None});
            match next{Some(p) if p!=parent=>parent=p,_=>return false}
        }
        false
    }
    pub fn namespace_recovery_inputs(&self, volume:u8, events:&[ChangeEvent])->(Vec<u64>,Vec<ChangeEvent>) {
        let g=self.store.read();
        let pending:Vec<ChangeEvent>=self.orphans.lock().ordered.iter().filter(|(v,_,_)|*v==volume).take(64).map(|(_,_,e)|e.clone()).collect();
        let mut missing:Vec<u64>=events.iter().chain(pending.iter()).filter_map(|e|match e{ChangeEvent::CreatePending{parent_id,..}=>Some(*parent_id),ChangeEvent::Rename{new_parent_id,..}=>Some(*new_parent_id),_=>None}).filter(|p|g.dir_on_volume(volume,*p).is_none()).collect();
        missing.sort_unstable();missing.dedup();
        let mut prioritized:Vec<u64>=pending.iter().filter_map(|e|match e{ChangeEvent::CreatePending{parent_id,..}=>Some(*parent_id),ChangeEvent::Rename{new_parent_id,..}=>Some(*new_parent_id),_=>None}).filter(|p|g.dir_on_volume(volume,*p).is_none()).collect();
        prioritized.sort_unstable();prioritized.dedup();prioritized.truncate(64);
        for p in missing{if prioritized.len()==64{break;}if !prioritized.contains(&p){prioritized.push(p);}}
        (prioritized,pending)
    }
    pub fn has_pending_namespace(&self, volume: u8) -> bool {
        self.orphans.lock().ordered.iter().any(|(v, _, _)| *v == volume)
    }
    fn restore_orphans(&self, volume: u8, prior: Vec<ChangeEvent>) {
        let mut o = self.orphans.lock();
        // Remove a partially parked failed batch before restoring the exact prior queue.
        let _ = o.take_volume(volume);
        for ev in prior {
            let parent = match &ev {
                ChangeEvent::Create { entry } => entry.parent_id,
                ChangeEvent::CreatePending { parent_id, .. } => *parent_id,
                ChangeEvent::Rename { new_parent_id, .. } => *new_parent_id,
                _ => 0,
            };
            let _ = o.park(volume, parent, ev);
        }
    }
    fn fold_snaps(&self, g: &IndexStore, volume: u8, applied: &[(Snap, Snap, ChangeEvent)]) -> Result<(), String> {
        let mut r = self.usage.lock();
        if r.epoch == 0 || r.error.is_some() { return Ok(()); }
        r.ensure_entries(&g)?;
        for (before, after, ev) in applied {
            if let Some(idx) = after.idx.or(before.idx) { r.invalidate_ticket(idx as usize); }
            self.fold_one(g, &mut r, volume, before, after, ev)?;
        }
        Ok(())
    }
    fn fold_one(&self, g: &IndexStore, r: &mut UsageRuntime, volume: u8, before: &Snap, after: &Snap, ev: &ChangeEvent) -> Result<(), String> {
        let Some(idx) = after.idx.or(before.idx) else { return Ok(()); };
        r.ensure_entries(g)?;
        // Replaced identities and kind changes can invalidate link ownership.
        // Retain the exhaustive query rather than publishing an incomplete set.
        if before.idx.is_some() && after.idx.is_some()
            && (before.idx != after.idx || before.is_dir != after.is_dir) {
            r.invalidate_parent_search(false);
        }
        if after.is_dir || before.is_dir {
            let di = after.di.or(before.di).ok_or("usage missing directory identity")?;
            let slot = g.volume_slot_of(volume).unwrap_or(0) as u8;
            let new_parent = r.logical_parent(after.parent);
            // Also guard the new-directory early return: a later replacement
            // parent can share a stale hidden subtree's searchable path.
            if !after.deleted && !r.visible_dir(new_parent) { r.invalidate_parent_search(true); }
            if before.di.is_none() || (before.di.map(|x| x as usize).unwrap_or(usize::MAX) >= r.dirs.len()) {
                r.ensure_dir_slot(di, new_parent, slot)?;
                r.dirs[di as usize] = DirAgg::empty(new_parent, slot);
                r.dirs[di as usize].owner_entry = idx;
                r.dirs[di as usize].set_totals(Totals::dir_self())?;
                if new_parent != di { r.link_dir(di, new_parent); }
                if !after.deleted { r.ancestors(if new_parent == di { NIL } else { new_parent }, Totals::dir_self(), true); }
                r.generation += 1;
                return Ok(());
            }
            let di = before.di.unwrap_or(di);
            if r.dirs[di as usize].flags & ROOT_ALIAS != 0 {
                if after.deleted {return Err("volume root identity changed; usage requires reinitialization".into())}
                r.generation+=1;return Ok(())
            }
            let old_parent = r.logical_parent(before.parent);
            let w = r.dirs[di as usize].totals();
            // A non-empty deleted directory may retain searchable stale children
            // in the namespace. Its old path must use the legacy scan, not zero.
            if after.deleted && !before.deleted && (w.files != 0 || w.dirs > 1) {
                r.invalidate_parent_search(true);
            }
            if !after.deleted && !r.visible_dir(new_parent) {
                r.invalidate_parent_search(true);
            }
            if !before.deleted { r.ancestors(if old_parent == di { NIL } else { old_parent }, w, false); }
            if old_parent!=di && (!before.deleted || after.deleted) {r.unlink_dir(di);}
            if !after.deleted && new_parent!=di {r.link_dir(di,new_parent);} else {r.dirs[di as usize].parent=new_parent;}

            r.dirs[di as usize].owner_entry = idx;
            r.dirs[di as usize].flags = (if after.deleted { 0 } else { ACTIVE })
                | (r.dirs[di as usize].flags & (PARENT_SEARCH_VALID | PARENT_SEARCH_DEEP_VALID));
            if !after.deleted { r.ancestors(if new_parent == di { NIL } else { new_parent }, w, true); }
            r.invalidate_ticket(idx as usize);
            r.generation += 1;
            return Ok(());
        }
        let new_parent = r.logical_parent(after.parent);
        if !after.deleted && !r.visible_dir(new_parent) { r.invalidate_parent_search(true); }
        let new_size = after.size;
        let old_unknown = r.freshness.get(idx as usize) != 1;
        let known = match ev {
            ChangeEvent::CreatePending { .. } => Some(false),
            ChangeEvent::Create { .. } | ChangeEvent::DataOrMeta { size: Some(_), .. } => Some(true),
            _ => None,
        };
        if let Some(k) = known { r.freshness.set(idx as usize, u8::from(k)); }
        let new_unknown = r.freshness.get(idx as usize) != 1;
        if before.idx.is_none() {
            r.link_file(idx, new_parent);
            if !after.deleted {
                r.ancestors(new_parent, Totals::file(new_size, new_unknown), true);
                r.generation += 1;
            }
            return Ok(());
        }
        let old = Totals::file(before.size, old_unknown);
        if after.deleted && !before.deleted {
            r.ancestors(before.parent, old, false);
            r.unlink_file(idx, before.parent);
            r.invalidate_ticket(idx as usize);
            r.generation += 1;
            return Ok(());
        }
        if before.deleted && !after.deleted {
            r.link_file(idx, new_parent);
            r.ancestors(new_parent, Totals::file(new_size, new_unknown), true);
            r.invalidate_ticket(idx as usize);
            r.generation += 1;
            return Ok(());
        }
        if r.logical_parent(before.parent) != new_parent {
            if !before.deleted { r.ancestors(before.parent, old, false); r.unlink_file(idx, before.parent); }
            r.link_file(idx, new_parent);
            if !after.deleted { r.ancestors(new_parent, Totals::file(new_size, new_unknown), true); }
            r.invalidate_ticket(idx as usize);
            r.generation += 1;
            return Ok(());
        }
        if !before.deleted && !after.deleted && (before.size != new_size || old_unknown != new_unknown) {
            r.apply_file_delta(new_parent, old, Totals::file(new_size, new_unknown), true, true);
            r.invalidate_ticket(idx as usize);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn raw(id: u64, parent: u64, name: &str, size: u64, dir: bool) -> crate::RawEntry {
        crate::RawEntry {
            file_id: id, file_id_128: None, parent_id: parent, name: name.into(), size,
            mtime: 0, ctime: 0, attrs: if dir { 0x10 } else { 0 }, is_dir: dir,
        }
    }
    fn store(v: u8) -> IndexStore {
        crate::IndexBuilder::new(v, 1, 7, 10)
            .build_from_raw(
                vec![raw(10, 2, "a.bin", 100, false)],
                vec![raw(1, 0, "root", 0, true), raw(2, 1, "left", 0, true), raw(3, 1, "right", 0, true)],
                true,
            ).unwrap()
    }
    fn engine() -> SearchEngine {
        let e = SearchEngine::new(store(b'C'));
        e.enable_usage(16 * 1024 * 1024).unwrap();
        e
    }
    fn size(e: &SearchEngine) -> u64 { e.usage_page("C:\\", None, None, 1024).unwrap().root.logical }
    fn ntfs_shape(letter:u8)->IndexStore {
        crate::IndexBuilder::new(letter,1,7,10).build_from_raw(
            vec![raw(10,4,"leaf.bin",100,false),raw(11,5,"root.bin",20,false),raw(12,1,"orphan.bin",7,false)],
            vec![raw(1,0,"sentinel",0,true),raw(5,1,"",0,true),raw(2,5,"Users",0,true),raw(3,2,"tester",0,true),raw(4,3,"completion",0,true),raw(6,5,"Windows",0,true),raw(7,5,"right",0,true)],true).unwrap()
    }
    #[test]
    fn transparent_ntfs_root_paths_and_namespace_deltas(){
        let e=SearchEngine::new(ntfs_shape(b'C'));e.enable_usage(16*1024*1024).unwrap();
        let root=e.usage_page("C:/",None,None,4096).unwrap();
        assert_eq!(root.root.logical,127);assert_eq!(root.root.directories,6);
        assert!(root.children.iter().any(|c|c.name=="Users"));
        assert!(root.children.iter().any(|c|c.name=="root.bin"));
        assert!(root.children.iter().all(|c|!c.name.is_empty()));
        for path in ["C:/Users/tester/completion/","c://uSeRs//TESTER/completion","C:/Users/tester/completion"].map(|p|p.replace('/',&char::from(92).to_string())) {
            assert_eq!(e.usage_page(&path,None,None,4096).unwrap().root.logical,100);
        }
        assert!(e.usage_page("C:/Users/tester/missing",None,None,1024).is_err());
        assert!(e.usage_page("C:/Users/../Windows",None,None,1024).is_err());
        e.apply_volume_change(b'C',&ChangeEvent::Create{entry:raw(13,5,"added",30,false)}).unwrap();
        assert_eq!(size(&e),157);
        let t=e.metadata_ticket_by_id(b'C',13).unwrap();assert!(e.commit_metadata(t,40,0,0).unwrap());assert_eq!(size(&e),167);
        e.apply_volume_change(b'C',&ChangeEvent::Rename{file_id:2,new_parent_id:7,new_name:"Users".into()}).unwrap();
        assert_eq!(size(&e),167);assert_eq!(e.usage_page("C:/right/Users/tester/completion",None,None,4096).unwrap().root.logical,100);
        assert!(e.usage_page("C:/Users/tester/completion",None,None,4096).is_err());
        e.apply_volume_change(b'C',&ChangeEvent::Delete{file_id:13}).unwrap();assert_eq!(size(&e),127);
        e.apply_volume_change(b'C',&ChangeEvent::DataOrMeta{file_id:5,size:None,mtime:Some(0),ctime:None,create_if_missing:None}).unwrap();
        assert!(e.usage_available());assert_eq!(size(&e),127);
    }
    #[test]
    fn explicit_exclusion_is_not_reported_as_a_missing_parent(){
        let mut g=ntfs_shape(b'C');g.excluded_dirs=vec!["C:/Users/tester/completion".replace('/',&char::from(92).to_string())];
        let e=SearchEngine::new(g);e.enable_usage(16*1024*1024).unwrap();
        assert!(e.usage_page("c:/users/tester/completion",None,None,1024).unwrap_err().contains("explicitly excluded"));
        assert!(e.usage_page("C:/Users/tester/completion-other",None,None,1024).unwrap_err().contains("not present"));
    }
    #[test]
    fn transparent_roots_on_two_volumes_do_not_mix_identity(){
        let g=crate::merge_index_stores(vec![ntfs_shape(b'C'),ntfs_shape(b'D')]).unwrap();
        let e=SearchEngine::new(g);e.enable_usage(16*1024*1024).unwrap();
        for v in ['C','D'] {assert_eq!(e.usage_page(&format!("{v}:/Users/tester/completion"),None,None,4096).unwrap().root.logical,100);}
    }
    #[test]
    fn live_chain_build_omits_deleted_prefix_and_resurrection_is_safe(){
        let mut g=ntfs_shape(b'C');let template=*g.entries.iter().find(|e|!e.is_dir_entry()).unwrap();
        for i in 0..17000 {let mut f=template;f.attrs|=crate::FileEntry::ATTR_DELETED;f.dir_idx=0;g.entries.push(f);g.frns.push(1000+i);}
        let e=SearchEngine::new(g);e.enable_usage(16*1024*1024).unwrap();
        let page=e.usage_page("C:/",None,None,1024).unwrap();assert!(!page.children.is_empty());
        assert_eq!(page.root.logical,127);
        // Retained inactive prefix explicitly exercises the server's independent
        // work budget/continuation, not a fake reparenting of live input records.
        {let mut r=e.usage.lock();for idx in 10..17000 {r.link_file(idx,0);}}
        let mut page=e.usage_page("C:/",None,None,1024).unwrap();let stamp=(page.epoch,page.generation);let mut empty=0;let mut observed=0;
        for _ in 0..20 {observed+=page.children.len();if page.children.is_empty()&&page.next.is_some(){empty+=1;}
            let Some(next)=page.next else{break};page=e.usage_page("C:/",Some(stamp),Some(next),1024).unwrap();}
        assert!(empty>=1);assert!(observed>0);assert!(page.next.is_none());
    }
    #[test]
    fn opaque_cursors_are_scoped_constant_seek_and_expire_explicitly(){
        let files=(0..200).map(|i|raw(100+i,1,&format!("file-{i}"),1,false)).collect();
        let g=crate::IndexBuilder::new(b'C',1,7,10).build_from_raw(files,vec![raw(1,0,"root",0,true),raw(2,1,"other",0,true)],true).unwrap();
        let e=SearchEngine::new(g);e.enable_usage(16*1024*1024).unwrap();
        let mut page=e.usage_page("C:/",None,None,1024).unwrap();let first=page.next.unwrap();let stamp=(page.epoch,page.generation);let mut seen=std::collections::HashSet::new();let mut pages=0;
        loop {for child in &page.children{assert!(seen.insert(child.name.clone()));}pages+=1;
            let Some(next)=page.next else{break};page=e.usage_page("C:/",Some(stamp),Some(next),1024).unwrap();assert!(pages<100);}
        assert_eq!(seen.len(),201);assert!(pages>50);
        assert!(e.usage_page("C:/",None,Some(first),1024).unwrap_err().contains("epoch/generation"));
        assert!(e.usage_page("C:/other",Some(stamp),Some(first),1024).unwrap_err().contains("scope"));
        for _ in 0..CURSOR_SLOTS+1{e.usage_page("C:/",None,None,1024).unwrap();}
        assert!(e.usage_page("C:/",Some(stamp),Some(first),1024).unwrap_err().contains("expired"));
        assert!(e.usage.lock().page_cursors.capacity()<=CURSOR_SLOTS);
    }
    #[test]
    fn page_output_budget_rejects_oversized_item_without_truncating_coverage(){
        let name="x".repeat(1100);let g=crate::IndexBuilder::new(b'C',1,7,10).build_from_raw(vec![raw(10,1,&name,1,false)],vec![raw(1,0,"root",0,true)],true).unwrap();
        let e=SearchEngine::new(g);e.enable_usage(1024*1024).unwrap();
        assert!(e.usage_page("C:/",None,None,1024).unwrap_err().contains("output budget"));
        let p=e.usage_page("C:/",None,None,4096).unwrap();assert_eq!(p.children.len(),1);assert!(p.next.is_none());
    }
    #[test]
    fn replacement_revokes_tickets_observations_and_overlay_for_reused_slots() {
        let e=engine();
        let ticket=e.metadata_ticket_by_id(b'C',10).unwrap();
        let epoch=ticket.epoch;
        let stamp=e.begin_metadata_observation(epoch).unwrap();
        let mut fresh=store(b'C');
        fresh.metadata_ready=false;
        fresh.patch_entry_metadata(ticket.entry,7,0,0).unwrap();
        e.replace_index_store(fresh);
        assert_eq!(e.metadata_overlay_len(),0);
        assert_eq!(e.metadata_ticket_storage().0,0);
        e.enable_usage(16*1024*1024).unwrap();
        assert!(!e.commit_metadata(ticket,999,0,0).unwrap());
        assert!(e.commit_metadata_observation(b'C',stamp,&[(10,888,0,0)],1024).is_err());
        assert!(e.flush_metadata_overlay_for_epoch(Some(epoch)).is_err());
        assert!(e.complete_metadata_if_covered(epoch).is_err());
        assert_eq!(e.index_store().entries[ticket.entry].size,7);
        assert!(!e.index_store().metadata_ready);
    }
    #[test]
    fn replacement_between_flush_pages_rejects_old_finalizer() {
        let mut initial=store(b'C');
        let file=*initial.entries.iter().find(|e|!e.is_dir_entry()).unwrap();
        for i in 0..8192 {initial.entries.push(file);initial.frns.push(1000+i);}
        let e=SearchEngine::new(initial);e.enable_usage(16*1024*1024).unwrap();
        e.set_metadata_ready(false).unwrap();
        let ticket=e.metadata_ticket_by_id(b'C',10).unwrap();
        assert!(e.commit_metadata(ticket,555,0,0).unwrap());
        let result=e.complete_metadata_with_pages(ticket.epoch, |_| {
            assert!(!e.index_store().metadata_ready);
            let mut fresh=store(b'C');fresh.metadata_ready=false;
            fresh.patch_entry_metadata(ticket.entry,7,0,0).unwrap();
            e.replace_index_store(fresh);
            e.enable_usage(16*1024*1024).unwrap();
        });
        assert!(result.is_err());
        assert_eq!(e.index_store().entries[ticket.entry].size,7);
        assert!(!e.index_store().metadata_ready);
        assert!(!e.metadata_drain_active());
    }
    #[test]
    fn completion_rechecks_unknown_after_each_bounded_drain() {
        let e=engine(); e.set_metadata_ready(false).unwrap();
        let epoch=e.metadata_epoch();
        assert!(!e.complete_metadata_with_pages(epoch, |_| {
            assert!(!e.index_store().metadata_ready);
            e.mark_metadata_dirty(b'C',10);
        }).unwrap());
        assert!(!e.index_store().metadata_ready);
        assert_eq!(e.metadata_coverage(epoch).unwrap().unknown,1);
        assert!(!e.metadata_drain_active());
    }
    #[test]
    fn late_ticket_and_namespace_metadata_survive_bounded_drain_clear() {
        let e=engine(); e.set_metadata_ready(false).unwrap();
        let epoch=e.metadata_epoch();
        assert!(e.complete_metadata_with_pages(epoch, |_| {
            assert!(!e.index_store().metadata_ready);
            let t=e.metadata_ticket_by_id(b'C',10).unwrap();
            assert!(e.commit_metadata(t,777,0,0).unwrap());
            e.apply_volume_change(b'C',&ChangeEvent::DataOrMeta{file_id:10,size:None,mtime:Some(crate::index::unix_secs_to_filetime(123)),ctime:None,create_if_missing:None}).unwrap();
        }).unwrap());
        let g=e.index_store();let idx=g.entry_on_volume(b'C',10).unwrap() as usize;
        assert_eq!(g.entries[idx].size,777);assert_eq!(g.entries[idx].mtime,123);
        assert!(g.metadata_ready);assert_eq!(e.metadata_overlay_len(),0);
        assert!(!e.metadata_drain_active());
    }
    #[test]
    fn incremental_namespace_and_metadata() {
        let e = engine();
        assert_eq!(size(&e), 100);
        e.apply_volume_change(b'C', &ChangeEvent::Create { entry: raw(11, 2, "new", 50, false) }).unwrap();
        assert_eq!(size(&e), 150);
        for s in [200, 20] {
            e.apply_volume_change(b'C', &ChangeEvent::DataOrMeta { file_id: 11, size: Some(s), mtime: None, ctime: None, create_if_missing: None }).unwrap();
            assert_eq!(size(&e), 100 + s);
        }
        e.apply_volume_change(b'C', &ChangeEvent::Rename { file_id: 11, new_parent_id: 3, new_name: "moved".into() }).unwrap();
        assert_eq!(e.usage_page("C:\\right", None, None, 1024).unwrap().root.logical, 20);
        e.apply_volume_change(b'C', &ChangeEvent::Rename { file_id: 2, new_parent_id: 3, new_name: "tree".into() }).unwrap();
        assert_eq!(e.usage_page("C:\\right", None, None, 1024).unwrap().root.logical, 120);
        e.apply_volume_change(b'C', &ChangeEvent::Delete { file_id: 2 }).unwrap();
        assert_eq!(size(&e), 20);
        e.apply_volume_change(b'C', &ChangeEvent::Delete { file_id: 10 }).unwrap();
        assert_eq!(size(&e), 20);
        e.apply_volume_change(b'C', &ChangeEvent::Delete { file_id: 11 }).unwrap();
        assert_eq!(size(&e), 0);
        assert!(e.usage_available());
        let (_dl, _dc, fl, _fc, bytes) = e.usage.lock().payload_stats();
        assert!(bytes < 8 * fl + 64 * 16 + 4096, "sidecar must stay compact, got {bytes}");
    }
    #[test]
    fn metadata_order_overlay_flush_and_epoch() {
        let e = engine();
        let old = e.metadata_ticket_by_id(b'C', 10).unwrap();
        e.apply_volume_change(b'C', &ChangeEvent::DataOrMeta { file_id: 10, size: Some(200), mtime: None, ctime: None, create_if_missing: None }).unwrap();
        assert!(!e.commit_metadata(old, 1, 0, 0).unwrap());
        assert_eq!(size(&e), 200);
        let t = e.metadata_ticket_by_id(b'C', 10).unwrap();
        assert!(e.commit_metadata(t, 300, 0, 0).unwrap());
        e.flush_metadata_overlay_into_store().unwrap();
        assert_eq!(size(&e), 300);
        e.extend_metadata_overlay_batch(&[(e.entry_idx(b'C', 10), 400, 0, 0)]);
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
        let e = SearchEngine::new(crate::merge_index_stores(vec![store(b'C'), store(b'D')]).unwrap());
        e.enable_usage(16 * 1024 * 1024).unwrap();
        let p = e.usage_page("C:\\", None, None, 1024).unwrap();
        e.apply_volume_change(b'D', &ChangeEvent::DataOrMeta { file_id: 10, size: Some(999), mtime: None, ctime: None, create_if_missing: None }).unwrap();
        assert_eq!(size(&e), 100);
        assert_eq!(e.usage_page("D:\\", None, None, 1024).unwrap().root.logical, 999);
        assert!(e.usage_page("C:\\", Some((p.epoch, p.generation)), None, 1024).is_err());
        assert!(e.usage_page("C:\\leftover", None, None, 1024).is_err());
    }
    #[test]
    fn two_volumes_same_frn_are_distinct() {
        let e = SearchEngine::new(crate::merge_index_stores(vec![store(b'C'), store(b'D')]).unwrap());
        e.enable_usage(16 * 1024 * 1024).unwrap();
        e.apply_volume_change(b'C', &ChangeEvent::Create { entry: raw(44, 1, "c-only", 5, false) }).unwrap();
        e.apply_volume_change(b'D', &ChangeEvent::Create { entry: raw(44, 1, "d-only", 7, false) }).unwrap();
        assert_eq!(e.usage_page("C:\\", None, None, 4096).unwrap().root.logical, 105);
        assert_eq!(e.usage_page("D:\\", None, None, 4096).unwrap().root.logical, 107);
        assert!(e.index_store().entry_on_volume(b'C', 44).is_some());
        assert!(e.index_store().entry_on_volume(b'D', 44).is_some());
        assert_ne!(e.index_store().entry_on_volume(b'C', 44), e.index_store().entry_on_volume(b'D', 44));
    }
    #[test]
    fn allocation_failure_and_missing_parent_do_not_advance() {
        let e = SearchEngine::new(store(b'C'));
        assert!(e.enable_usage(1).is_err());
        let e = engine();
        let p = e.usage_page("C:\\", None, None, 1024).unwrap();
        assert!(e.apply_volume_change(b'C', &ChangeEvent::Create { entry: raw(77, 999, "orphan", 1, false) }).is_err());
        assert_eq!(e.usage_page("C:\\", None, None, 1024).unwrap().generation, p.generation);
        assert_eq!(e.index_store().entry_count(), store(b'C').entry_count());
    }
    #[test]
    fn excluded_child_of_missing_parent_is_noop_and_acks() {
        let mut g = store(b'F');
        g.excluded_dirs.push("f:\\root\\excluded".into());
        let e = SearchEngine::new(g);
        e.enable_usage(1024 * 1024).unwrap();
        e.apply_volume_batch(b'F', &[
            ChangeEvent::CreatePending { file_id: 888, file_id_128: None, parent_id: 1, name: "excluded".into(), attrs: 0x10, is_dir: true },
            ChangeEvent::CreatePending { file_id: 889, file_id_128: None, parent_id: 888, name: "child".into(), attrs: 0, is_dir: false },
        ], 7, 30).unwrap();
        assert_eq!(e.index_store().volumes[0].last_usn, 30);
        assert!(e.index_store().entry_on_volume(b'F', 888).is_none());
        assert!(e.index_store().entry_on_volume(b'F', 889).is_none());
        assert_eq!(e.usage_page("F:\\", None, None, 4096).unwrap().root.logical, 100);
    }
    #[test]
    fn missing_parent_is_parked_then_attached() {
        // Only the volume root exists: FRN 2/3 must not already be left/right,
        // otherwise this would mutate an existing directory instead of parking.
        let g = crate::IndexBuilder::new(b'F', 1, 7, 10)
            .build_from_raw(vec![raw(10, 1, "a.bin", 100, false)], vec![raw(1, 0, "root", 0, true)], true)
            .unwrap();
        let e = SearchEngine::new(g);
        e.enable_usage(1024 * 1024).unwrap();
        let before = e.index_store().entry_count();
        e.apply_volume_batch(b'F', &[ChangeEvent::Create { entry: raw(3, 2, "file", 10, false) }], 7, 20).unwrap();
        assert_eq!(e.index_store().volumes[0].last_usn, 10, "unpersisted pending events must pin the durable restart cursor");
        assert!(e.has_pending_namespace(b'F'));
        assert_eq!(e.index_store().entry_count(), before);
        e.apply_volume_batch(b'F', &[ChangeEvent::Create { entry: raw(2, 1, "parent", 0, true) }], 7, 21).unwrap();
        assert_eq!(e.index_store().volumes[0].last_usn, 21);
        assert_eq!(e.usage_page("F:\\", None, None, 4096).unwrap().root.logical, 110);
        assert!(e.apply_volume_batch(b'F', &[ChangeEvent::Delete { file_id: 3 }], 99, 22).is_err());
        assert_eq!(e.usage_page("F:\\", None, None, 4096).unwrap().root.logical, 110);
        let t = e.metadata_ticket_by_id(b'F', 3).unwrap();
        e.mark_metadata_dirty(b'F', 3);
        assert!(!e.commit_metadata(t, 99, 0, 0).unwrap());
        let t = e.metadata_ticket_by_id(b'F', 3).unwrap();
        e.note_external_mutation();
        e.enable_usage(1024 * 1024).unwrap();
        assert!(!e.commit_metadata(t, 99, 0, 0).unwrap());
    }
    #[test]
    fn missing_parent_deleted_identity_unpins_cursor() {
        let e = engine();
        let before = e.usage_page("C:\\", None, None, 4096).unwrap().root.logical;
        e.apply_volume_batch(b'C', &[ChangeEvent::Create { entry: raw(777, 999, "transient", 17, false) }], 7, 20).unwrap();
        assert!(e.has_pending_namespace(b'C'));
        e.apply_volume_batch(b'C', &[ChangeEvent::Delete { file_id: 777 }], 7, 21).unwrap();
        assert!(!e.has_pending_namespace(b'C'), "terminal delete supersedes unmaterialized create, rather than parking forever");
        assert_eq!(e.index_store().volumes[0].last_usn, 21);
        assert!(e.index_store().entry_on_volume(b'C', 777).is_none());
        assert_eq!(e.usage_page("C:\\", None, None, 4096).unwrap().root.logical, before);
    }

    #[test]
    fn missing_parent_rename_then_delete_removes_existing_entry() {
        let e = engine();
        e.apply_volume_batch(b'C', &[
            ChangeEvent::Rename { file_id: 10, new_parent_id: 999, new_name: "unresolved".into() },
            ChangeEvent::Delete { file_id: 10 },
        ], 7, 23).unwrap();
        assert!(!e.has_pending_namespace(b'C'));
        assert_eq!(e.index_store().volumes[0].last_usn, 23);
        assert!(e.index_store().entries[e.index_store().entry_on_volume(b'C', 10).unwrap() as usize].is_deleted());
        assert_eq!(e.usage_page("C:\\", None, None, 4096).unwrap().root.logical, 0);
    }

    #[test]
    fn missing_parent_delete_does_not_cancel_later_recreation() {
        let e = engine();
        e.apply_volume_batch(b'C', &[
            ChangeEvent::Create { entry: raw(777, 999, "old", 17, false) },
            ChangeEvent::Delete { file_id: 777 },
            ChangeEvent::Create { entry: raw(777, 998, "new", 19, false) },
        ], 7, 25).unwrap();
        assert!(e.has_pending_namespace(b'C'));
        assert_ne!(e.index_store().volumes[0].last_usn, 25);
        e.apply_volume_batch(b'C', &[ChangeEvent::Create { entry: raw(998, 1, "recovered", 0, true) }], 7, 26).unwrap();
        assert!(!e.has_pending_namespace(b'C'));
        assert_eq!(e.index_store().volumes[0].last_usn, 26);
        assert_eq!(e.usage_page("C:\\", None, None, 4096).unwrap().root.logical, 119);
    }

    #[test]
    fn prefix_failure_rolls_back_namespace() {
        let e = engine();
        let before = e.index_store().entry_count();
        let cursor = e.index_store().volumes[0].last_usn;
        let good = ChangeEvent::Create { entry: raw(50, 1, "ok", 4, false) };
        let bad = ChangeEvent::CreatePending { file_id: 51, file_id_128: Some([0,0,0,0,0,0,0,0,1,0,0,0,0,0,0,0]), parent_id: 1, name: "wide".into(), attrs: 0, is_dir: false };
        assert!(e.apply_volume_batch(b'C', &[good, bad], 7, 80).is_err());
        assert_eq!(e.index_store().entry_count(), before);
        assert_eq!(e.index_store().volumes[0].last_usn, cursor);
        assert_eq!(size(&e), 100);
    }
    #[test]
    fn same_batch_child_before_parent_resolves() {
        let e = SearchEngine::new(store(b'F'));
        e.enable_usage(1024 * 1024).unwrap();
        e.apply_volume_batch(b'F', &[
            ChangeEvent::CreatePending { file_id: 101, file_id_128: None, parent_id: 100, name: "child".into(), attrs: 0, is_dir: false },
            ChangeEvent::CreatePending { file_id: 100, file_id_128: None, parent_id: 1, name: "parent".into(), attrs: 0x10, is_dir: true },
        ], 7, 40).unwrap();
        assert_eq!(e.index_store().volumes[0].last_usn, 40);
        assert_eq!(e.usage_page("F:\\", None, None, 4096).unwrap().root.files, 2);
    }
    #[test]
    fn excluded_identity_resource_budget_is_explicit_and_bounded() {
        let e=engine();
        let mut accepted=0;
        for id in 1000..1100 {
            match e.note_excluded_identity_with_budget(b'C',id,256) {
                Ok(()) => accepted+=1,
                Err(message) => { assert!(message.contains("budget")); break; }
            }
        }
        assert!(accepted>0 && accepted<100);
        let usage=e.usage.lock();
        assert_eq!(usage.excluded_ids.len(),accepted);
        assert!(usage.excluded_ids.capacity()*32<=256);
    }
    #[test]
    fn namespace_metadata_supersedes_overlay_and_stale_ticket() {
        let e=engine();
        let idx=e.entry_idx(b'C',10);
        e.extend_metadata_overlay_batch(&[(idx,300,0,0)]);
        let stale=e.metadata_ticket_by_id(b'C',10).unwrap();
        e.apply_volume_batch(b'C',&[
            ChangeEvent::DataOrMeta { file_id:10,size:Some(400),mtime:None,ctime:None,create_if_missing:None },
            ChangeEvent::Rename { file_id:10,new_parent_id:3,new_name:"moved".into() },
            ChangeEvent::DataOrMeta { file_id:10,size:None,mtime:Some(crate::index::unix_secs_to_filetime(123)),ctime:None,create_if_missing:None },
        ],7,20).unwrap();
        assert_eq!(size(&e),400);
        assert_eq!(e.metadata_overlay.get(idx).unwrap().0,400);
        assert_eq!(e.metadata_overlay.get(idx).unwrap().1,123);
        assert!(!e.commit_metadata(stale,1,0,0).unwrap());
        e.flush_metadata_overlay_into_store().unwrap();
        assert_eq!(e.index_store().entries[idx].size,400);
        assert_eq!(size(&e),400);
        let bad=ChangeEvent::CreatePending {file_id:80,file_id_128:Some([1;16]),parent_id:1,name:"bad".into(),attrs:0,is_dir:false};
        assert!(e.apply_volume_batch(b'C',&[ChangeEvent::DataOrMeta {file_id:10,size:Some(900),mtime:None,ctime:None,create_if_missing:None},bad],7,21).is_err());
        assert_eq!(e.metadata_overlay.get(idx).unwrap().0,400,"rolled-back metadata cannot leak into overlay");
        assert_eq!(size(&e),400);
    }
    #[test]
    fn repeated_identity_batch_matches_sequential() {
        let batch = engine();
        let sequential = engine();
        let events = vec![
            ChangeEvent::Create { entry: raw(99, 2, "created", 4, false) },
            ChangeEvent::DataOrMeta { file_id: 99, size: Some(31), mtime: None, ctime: None, create_if_missing: None },
            ChangeEvent::Rename { file_id: 99, new_parent_id: 3, new_name: "moved".into() },
            ChangeEvent::Delete { file_id: 99 },
            ChangeEvent::Create { entry: raw(99, 2, "revived", 12, false) },
            ChangeEvent::Rename { file_id: 2, new_parent_id: 3, new_name: "tree".into() },
            ChangeEvent::DataOrMeta { file_id: 10, size: Some(200), mtime: None, ctime: None, create_if_missing: None },
            ChangeEvent::Rename { file_id: 2, new_parent_id: 1, new_name: "left".into() },
        ];
        for ev in &events { sequential.apply_volume_change(b'C', ev).unwrap(); }
        batch.apply_volume_batch(b'C', &events, 7, 60).unwrap();
        for path in ["C:\\", "C:\\left", "C:\\right"] {
            let a = batch.usage_page(path, None, None, 4096).unwrap();
            let b = sequential.usage_page(path, None, None, 4096).unwrap();
            assert_eq!((a.root.logical, a.root.files, a.root.unknown), (b.root.logical, b.root.files, b.root.unknown), "{path}");
        }
        assert_eq!(size(&batch), 212);
    }
    #[test]
    fn pending_order_restart_replay_and_budget_rollback() {
        let e = engine();
        let events = vec![
            ChangeEvent::Create { entry: raw(80, 700, "pending", 2, false) },
            ChangeEvent::DataOrMeta { file_id: 80, size: Some(40), mtime: None, ctime: None, create_if_missing: None },
            ChangeEvent::Rename { file_id: 80, new_parent_id: 701, new_name: "later".into() },
        ];
        e.apply_volume_batch(b'C', &events, 7, 30).unwrap();
        assert_eq!(e.index_store().volumes[0].last_usn, 10);
        e.apply_volume_batch(b'C', &[], 7, 31).unwrap();
        assert_eq!(e.index_store().volumes[0].last_usn, 10, "heartbeats cannot acknowledge lost pending events");
        // Simulated process restart: only persisted namespace/cursor survives, not pending queue.
        let restarted = SearchEngine::new(e.index_store().clone());
        restarted.enable_usage(16 * 1024 * 1024).unwrap();
        restarted.apply_volume_batch(b'C', &events, 7, 30).unwrap();
        let parents = [
            ChangeEvent::Create { entry: raw(700, 1, "parent1", 0, true) },
            ChangeEvent::Create { entry: raw(701, 1, "parent2", 0, true) },
        ];
        for engine in [&e, &restarted] {
            engine.apply_volume_batch(b'C', &parents, 7, 32).unwrap();
            assert!(!engine.has_pending_namespace(b'C'));
            assert_eq!(engine.index_store().volumes[0].last_usn, 32);
            assert_eq!(size(engine), 140);
        }
        e.orphans.lock().budget = 1;
        let before = e.index_store().entry_count();
        assert!(e.apply_volume_batch(b'C', &[
            ChangeEvent::Create { entry: raw(900, 1, "prefix", 1, false) },
            ChangeEvent::Create { entry: raw(901, 9999, "unresolved", 1, false) },
        ], 7, 40).is_err());
        assert_eq!(e.index_store().entry_count(), before);
        assert_eq!(e.index_store().volumes[0].last_usn, 32);
        assert!(!e.has_pending_namespace(b'C'));
        assert_eq!(size(&e), 140);
        let g=e.index_store();
        for bucket in g.ext_filter.iter().flatten() { assert!(bucket.iter().all(|idx| (idx as usize) < before)); }
    }
    #[test]
    fn reserve_without_name_change_does_not_grow_names() {
        let e = engine();
        {
            let mut g = e.index_store_mut();
            g.names_buf.shrink_to_fit();
            let cap = g.names_buf.capacity();
            g.try_reserve_namespace_change(0, false, false).unwrap();
            assert_eq!(g.names_buf.capacity(), cap);
        }
        let cap = e.index_store().names_buf.capacity();
        e.apply_volume_change(b'C', &ChangeEvent::DataOrMeta { file_id: 10, size: Some(120), mtime: None, ctime: None, create_if_missing: None }).unwrap();
        assert_eq!(e.index_store().names_buf.capacity(), cap);
        assert_eq!(size(&e), 120);
    }
    #[test]
    fn directory_tree_move_is_o_depth_not_o_descendants() {
        let mut files = vec![raw(10, 2, "a.bin", 100, false)];
        for i in 0..200u64 { files.push(raw(1000+i, 2, "f", 1, false)); }
        let g = crate::IndexBuilder::new(b'C', 1, 7, 10)
            .build_from_raw(files, vec![raw(1, 0, "root", 0, true), raw(2, 1, "left", 0, true), raw(3, 1, "right", 0, true)], true)
            .unwrap();
        let e = SearchEngine::new(g);
        e.enable_usage(16 * 1024 * 1024).unwrap();
        let before = e.usage.lock().visits();
        e.apply_volume_change(b'C', &ChangeEvent::Rename { file_id: 2, new_parent_id: 3, new_name: "tree".into() }).unwrap();
        let visits = e.usage.lock().visits() - before;
        assert!(visits <= 16, "directory move must be O(depth), visits={visits}");
        let right = e.usage_page("C:\\right", None, None, 16 * 1024).unwrap();
        assert_eq!(right.root.logical, 300);
        assert!(right.root.files >= 201);
    }
    #[test]
    fn usage_error_does_not_block_namespace() {
        let e = engine();
        e.usage.lock().error = Some("injected".into());
        e.apply_volume_change(b'C', &ChangeEvent::Create { entry: raw(70, 1, "kept", 3, false) }).unwrap();
        assert!(e.index_store().entry_on_volume(b'C', 70).is_some());
        assert!(e.usage_page("C:\\", None, None, 1024).is_err());
    }
}

#[cfg(test)]
impl SearchEngine {
    fn entry_idx(&self, volume: u8, frn: u64) -> usize {
        self.index_store().entry_on_volume(volume, frn).unwrap() as usize
    }
}

#[cfg(test)]
mod scale_tests {
    use super::*;

    fn process_mem() -> (usize, usize, usize, usize) {
        #[cfg(windows)]
        {
            #[repr(C)]
            struct PmcEx {
                cb: u32,
                page_fault_count: u32,
                peak_working_set_size: usize,
                working_set_size: usize,
                quota_peak_paged_pool_usage: usize,
                quota_paged_pool_usage: usize,
                quota_peak_non_paged_pool_usage: usize,
                quota_non_paged_pool_usage: usize,
                pagefile_usage: usize,
                peak_pagefile_usage: usize,
                private_usage: usize,
            }
            #[link(name = "psapi")]
            extern "system" {
                fn GetProcessMemoryInfo(h: *mut core::ffi::c_void, p: *mut PmcEx, cb: u32) -> i32;
            }
            #[link(name = "kernel32")]
            extern "system" {
                fn GetCurrentProcess() -> *mut core::ffi::c_void;
            }
            unsafe {
                let mut m: PmcEx = std::mem::zeroed();
                m.cb = std::mem::size_of::<PmcEx>() as u32;
                if GetProcessMemoryInfo(GetCurrentProcess(), &mut m, m.cb) != 0 {
                    return (m.working_set_size, m.peak_working_set_size, m.private_usage, m.peak_pagefile_usage);
                }
            }
        }
        (0, 0, 0, 0)
    }
    fn record_mem(label: &str, before: (usize, usize, usize, usize), after: (usize, usize, usize, usize), extra: &str) {
        let line = format!(
            "{label} ws_before={} peak_ws_before={} private_before={} peak_pf_before={} ws_after={} peak_ws_after={} private_after={} peak_pf_after={} d_ws={} d_private={} d_peak_ws={} {extra}",
            before.0, before.1, before.2, before.3, after.0, after.1, after.2, after.3,
            after.0 as i64 - before.0 as i64, after.2 as i64 - before.2 as i64, after.1 as i64 - before.1 as i64
        );
        eprintln!("{line}");
        if let Some(path) = option_env!("FINDX_T109_MEM").map(std::path::PathBuf::from).or_else(|| {
            std::env::var_os("FINDX_T109_MEM").map(std::path::PathBuf::from)
        }).or_else(|| {
            let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/usage-t109-memory.txt");
            Some(p)
        }) {
            let mut body = std::fs::read_to_string(&path).unwrap_or_default();
            if !body.contains(&label) {
                body.push_str(&line);
                body.push('\n');
                let _ = std::fs::write(&path, body);
            }
        }
    }
    #[test]
    fn over_two_million_synthetic_entries_have_no_count_cutoff() {
        let raw = |id: u64, parent: u64, dir: bool| crate::RawEntry {
            file_id: id, file_id_128: None, parent_id: parent,
            name: if dir { "root".into() } else { "x".into() },
            size: 1, mtime: 0, ctime: 0, attrs: if dir { 0x10 } else { 0 }, is_dir: dir,
        };
        let mut g = crate::IndexBuilder::new(b'C', 1, 1, 1)
            .build_from_raw(vec![raw(2, 1, false)], vec![raw(1, 0, true)], true)
            .unwrap();
        let n = 2_000_017usize;
        let file = g.entries[0];
        g.entries.try_reserve(n).unwrap();
        g.frns.try_reserve(n).unwrap();
        for i in 0..n { g.entries.push(file); g.frns.push(100 + i as u64); }
        let e = SearchEngine::new(g);
        let start = std::time::Instant::now();
        let mem0 = process_mem();
        e.enable_usage(64 * 1024 * 1024).unwrap();
        let p = e.usage_page("C:\\", None, None, 4096).unwrap();
        let mem1 = process_mem();
        record_mem("enable_usage_2m", mem0, mem1, &format!("files={} resident_capacity_bytes={} build_ms={}", n + 1, p.resident_bytes, start.elapsed().as_millis()));
        assert_eq!(p.root.files, (n + 1) as u64);
        assert_eq!(p.root.logical, (n + 1) as u64);
        assert!(p.next.is_some());
        assert!(p.resident_bytes < 24 * 1024 * 1024, "2M-file compact sidecar should be well under 24MiB, got {}", p.resident_bytes);
        eprintln!("synthetic logical tree entries={} resident_capacity_bytes={} build_ms={}", n + 1, p.resident_bytes, start.elapsed().as_millis());
    }
    #[test]
    #[ignore = "explicit bounded memory experiment; run alone with --ignored --exact"]
    fn representative_ratio_initialization_memory() {
        // Quarter-scale N and D; names are synthetic, no filesystem or real watcher.
        let n = 1_540_069usize;
        let d = 179_981usize;
        let raw = |id: u64, parent: u64, dir: bool| crate::RawEntry {
            file_id: id, file_id_128: None, parent_id: parent, name: "synthetic".into(),
            size: u64::from(!dir), mtime: 0, ctime: 0, attrs: if dir { 0x10 } else { 0 }, is_dir: dir,
        };
        let dirs = (0..d).map(|i| raw(1+i as u64, if i == 0 { 0 } else { 1+((i-1)/8) as u64 }, true)).collect();
        let mut g = crate::IndexBuilder::new(b'F', 1, 7, 10)
            .build_from_raw(vec![raw(10_000_000, 1, false)], dirs, true).unwrap();
        let template = g.entries.iter().find(|e| !e.is_dir_entry()).copied().unwrap();
        let extra = n-g.entries.len();
        g.entries.try_reserve_exact(extra).unwrap();
        g.frns.try_reserve_exact(extra).unwrap();
        for i in 0..extra {
            let mut file=template;
            file.dir_idx=(i % d) as u32;
            g.entries.push(file);
            g.frns.push(10_000_001+i as u64);
        }
        let engine=SearchEngine::new(g);
        let before=process_mem();
        let done=std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal=done.clone();
        let sampler=std::thread::spawn(move || {
            let mut peaks=before;
            while !signal.load(Ordering::Relaxed) {
                let m=process_mem();
                peaks.0=peaks.0.max(m.0); peaks.2=peaks.2.max(m.2);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            peaks
        });
        let start=std::time::Instant::now();
        engine.enable_usage(96*1024*1024).unwrap();
        let elapsed=start.elapsed().as_millis();
        let after=process_mem();
        done.store(true,Ordering::Relaxed);
        let peak=sampler.join().unwrap();
        let r=engine.usage.lock();
        let (dl,dc,fl,fc,bytes)=r.payload_stats();
        record_mem("ratio_quarter_enable_usage",before,after,&format!("N={n} D={d} dir_len={dl} dir_cap={dc} links_len={fl} links_cap={fc} capacity_bytes={bytes} build_ms={elapsed} sampled_peak_ws={} sampled_peak_private={} sampled_peak_ws_delta={} sampled_peak_private_delta={}",peak.0,peak.2,peak.0.saturating_sub(before.0),peak.2.saturating_sub(before.2)));
        drop(r);
        let page=engine.usage_page("F:\\",None,None,4096).unwrap();
        assert_eq!(page.root.files,(n-d) as u64);
        assert_eq!(page.root.logical,(n-d) as u64);
    }
    #[test]
    fn compact_payload_formula_on_production_scale_arrays() {
        let n = 6_160_274usize;
        let d = 719_923usize;
        let mem0 = process_mem();
        let mut dirs: Vec<DirAgg> = Vec::new();
        dirs.try_reserve(d).unwrap();
        dirs.resize(d, DirAgg::empty(0, 0));
        let mut links: Vec<FileLinks> = Vec::new();
        links.try_reserve(n).unwrap();
        links.resize(n, FileLinks::default());
        let fresh = Freshness::with_len(n).unwrap();
        let mem1 = process_mem();
        let payload = dirs.capacity() * std::mem::size_of::<DirAgg>()
            + links.capacity() * std::mem::size_of::<FileLinks>()
            + fresh.capacity_bytes();
        record_mem("sidecar_arrays_6m", mem0, mem1, &format!("payload_capacity_bytes={payload} dir_len={} dir_cap={} file_len={} file_cap={} fresh_cap={}", dirs.len(), dirs.capacity(), links.len(), links.capacity(), fresh.capacity_bytes()));
        let expected = 48 * d + 8 * n + n.div_ceil(4);
        assert_eq!(std::mem::size_of::<DirAgg>(), 48);
        assert_eq!(std::mem::size_of::<FileLinks>(), 8);
        assert!(payload <= expected + 48 * d, "capacity may grow but must stay near tight payload, got {payload} vs {expected}");
        assert!(payload < 120 * 1024 * 1024, "production-scale sidecar payload should stay near the compact payload model, got {payload}");
        eprintln!("production-scale compact payload bytes={payload} tight={expected} dir_cap={} file_cap={}", dirs.capacity(), links.capacity());
    }
}

#[cfg(test)]
mod allocation_tests {
    use super::*;
    #[test]
    fn capacity_overflow_is_a_fallible_allocation_error_not_a_panic() {
        let mut runtime = UsageRuntime { budget: usize::MAX, ..Default::default() };
        let e = runtime.reserve(usize::MAX, 0).unwrap_err();
        assert!(e.contains("allocation failed") || e.contains("budget") || e.contains("exceed"), "{e}");
    }
}

/// A read observation is bounded to one directory syscall page. Unrelated writes
/// conservatively reject the page; the single linear fallback pass retries misses.
#[derive(Clone, Copy, Debug)]
pub struct MetadataObservation { epoch:u64, revision:u64 }
#[derive(Clone, Copy, Debug)]
pub struct MetadataWorkItem { pub ticket:MetadataTicket, pub volume:u8, pub frn:u64 }
pub struct MetadataWorkBatch { pub next_index:usize, pub items:Vec<MetadataWorkItem> }
#[derive(Clone, Copy, Debug)]
pub struct MetadataCoverage {pub files:usize,pub known:usize,pub unknown:usize}
impl SearchEngine {
    pub fn metadata_epoch(&self)->u64 { self.usage.lock().epoch }
    pub fn metadata_ticket_storage(&self)->(usize,usize,usize) {
        let r=self.usage.lock();(r.tickets.len(),r.tickets.capacity(),r.tickets.capacity()*32)
    }
    pub fn release_metadata_ticket(&self,t:MetadataTicket) {
        let mut r=self.usage.lock();
        if r.epoch==t.epoch && r.tickets.get(&t.entry)==Some(&t.seq) {r.tickets.remove(&t.entry);}
    }
    pub fn reclaim_metadata_ticket_capacity(&self) {
        let mut r=self.usage.lock();
        // Only pass completion/cancellation, never per-file shrink churn.
        if r.tickets.capacity()*32>64*1024 && r.tickets.len()<r.tickets.capacity()/4 {
            let keep=r.tickets.len().saturating_mul(2);
            r.tickets.shrink_to(keep);
        }
    }
    pub fn metadata_coverage(&self,epoch:u64)->Result<MetadataCoverage,String> {
        let _g=self.store.read();let r=self.usage.lock();
        if r.epoch!=epoch||r.error.is_some(){return Err("metadata coverage epoch unavailable or changed".into());}
        let mut c=MetadataCoverage{files:0,known:0,unknown:0};
        for &root in r.roots.values(){let d=&r.dirs[root as usize];c.files+=d.files as usize;c.unknown+=d.unknown as usize;}
        c.known=c.files.saturating_sub(c.unknown);Ok(c)
    }
    pub fn metadata_work_batch(&self,epoch:u64,start:usize,end:usize,active_bytes:usize)->Result<MetadataWorkBatch,String> {
        // Includes item, registry growth, IO vectors and returned updates. This is
        // a work-window charge, NOT a total-file truncation limit.
        let cap=active_bytes/256;
        if cap==0{return Err("metadata active byte budget cannot admit one work item".into());}
        let g=self.store.read();let mut r=self.usage.lock();
        if r.epoch!=epoch||r.error.is_some()||epoch==0{return Err("metadata work epoch unavailable or changed".into());}
        let mut items:Vec<MetadataWorkItem>=Vec::new();
        items.try_reserve_exact(cap).map_err(|_|"metadata work-window allocation failed")?;
        let mut next=start;
        let scan_end=start.saturating_add(32768).min(end).min(g.entries.len());
        while next<scan_end && items.len()<cap {
            let idx=next;next+=1;let e=&g.entries[idx];
            if e.is_deleted()||e.is_dir_entry()||r.freshness.get(idx)==1{continue;}
            let seq=match r.issue_ticket(idx) {Ok(seq)=>seq,Err(error)=>{
                for item in &items { if r.tickets.get(&item.ticket.entry)==Some(&item.ticket.seq){r.tickets.remove(&item.ticket.entry);} }
                return Err(error);
            }};
            let frn=g.frns[idx];
            items.push(MetadataWorkItem{ticket:MetadataTicket{entry:idx,epoch,seq,frn},volume:g.volumes[r.dirs[e.dir_idx as usize].volume_slot as usize].volume_letter,frn});
        }
        Ok(MetadataWorkBatch{next_index:next,items})
    }
    pub fn pending_metadata_directory(&self,epoch:u64,di:usize)->Result<Option<(u8,u64)>,String> {
        let g=self.store.read();let r=self.usage.lock();
        if r.epoch!=epoch||r.error.is_some(){return Err("metadata directory epoch changed".into());}
        let Some(dir)=r.dirs.get(di) else{return Ok(None)};
        if !dir.active(){return Ok(None);}
        if dir.unknown>0 && dir.first_file!=NIL {
            return Ok(Some((g.volumes[dir.volume_slot as usize].volume_letter,if dir.owner_entry!=NIL {g.frns[dir.owner_entry as usize]} else {g.dirs[di].frn})));
        }
        Ok(None)
    }
    pub fn begin_metadata_observation(&self,epoch:u64)->Result<MetadataObservation,String> {
        let _g=self.store.read();let r=self.usage.lock();
        if r.epoch!=epoch||r.error.is_some(){return Err("metadata observation epoch changed".into());}
        Ok(MetadataObservation{epoch,revision:self.metadata_revision()})
    }
    pub fn commit_metadata_observation(&self,volume:u8,stamp:MetadataObservation,records:&[(u64,u64,u64,u64)],active_bytes:usize)->Result<usize,String> {
        if records.len().saturating_mul(32)>active_bytes{return Err("metadata observation page exceeds byte budget".into());}
        let mut g=self.store.write();let mut r=self.usage.lock();
        if r.epoch!=stamp.epoch||r.error.is_some(){return Err("metadata observation epoch changed".into());}
        if self.metadata_revision()!=stamp.revision{return Ok(0);}
        let mut committed=0;
        for &(frn,size,mtime,ctime) in records {
            let Some(idx)=g.entry_on_volume(volume,frn).map(|i|i as usize) else{continue};
            let e=&g.entries[idx];
            if e.is_deleted()||e.is_dir_entry()||r.freshness.get(idx)==1 {continue;}
            let parent=e.dir_idx;let old=Totals::file(self.effective_size(&g,idx),true);
            self.mirror_metadata_for_persistence(&mut g,idx,size,mtime,ctime).map_err(|e|e.to_string())?;
            self.metadata_overlay.put(idx,size,crate::index::filetime_to_unix_secs(mtime),crate::index::filetime_to_unix_secs(ctime));
            r.freshness.set(idx,1);r.invalidate_ticket(idx);
            r.apply_file_delta(parent,old,Totals::file(size,false),true,true);committed+=1;
        }
        if committed>0 {self.bump_revision();}
        Ok(committed)
    }
}

impl SearchEngine {
    pub fn complete_metadata_if_covered(&self,epoch:u64)->Result<bool,String> {
        self.complete_metadata_with_pages(epoch, |_| {})
    }
    fn complete_metadata_with_pages(&self,epoch:u64,after_page:impl FnMut(usize))->Result<bool,String> {
        let _drain = self.begin_metadata_drain();
        {
            let _g=self.store.write();let r=self.usage.lock();
            if r.epoch!=epoch||r.error.is_some(){return Err("metadata completion epoch changed".into());}
            if r.roots.values().any(|&id|r.dirs[id as usize].unknown>0){return Ok(false);}
        }
        self.flush_metadata_overlay_pages(Some(epoch),after_page).map_err(|e|e.to_string())?;
        let mut g=self.store.write();let r=self.usage.lock();
        if r.epoch!=epoch||r.error.is_some(){return Err("metadata completion epoch changed".into());}
        // A watcher may dirty metadata between flush pages. Recheck coverage
        // before publishing ready; producers mirror late writes during drain.
        if r.roots.values().any(|&id|r.dirs[id as usize].unknown>0){return Ok(false);}
        if !g.metadata_ready {g.mark_full_resync();}
        g.metadata_ready=true;
        self.metadata_overlay.clear();self.bump_revision();Ok(true)
    }
}

#[cfg(test)]
mod internal_unlink_transaction_tests {
 use super::*;
 #[test]
 fn parked_internal_alias_rebind_is_atomic_and_idempotent(){
  let raw=|id,parent,name:&str,dir|crate::RawEntry{file_id:id,file_id_128:None,parent_id:parent,name:name.into(),size:if dir{0}else{100},mtime:0,ctime:0,attrs:if dir{16}else{0},is_dir:dir};
  let store=crate::IndexBuilder::new(b'C',1,7,10).build_from_raw(vec![raw(42,2,"old.bin",false)],vec![raw(1,0,"root",true),raw(2,1,"ordinary",true)],true).unwrap();
  let e=SearchEngine::new(store);e.enable_usage(16*1024*1024).unwrap();
  e.apply_volume_batch(b'C',&[ChangeEvent::CreatePending{file_id:42,file_id_128:None,parent_id:900,name:"internal".into(),attrs:0,is_dir:false},ChangeEvent::DataOrMeta{file_id:42,size:Some(150),mtime:None,ctime:None,create_if_missing:None}],7,20).unwrap();assert!(e.has_pending_namespace(b'C'));
  let recovered=[ChangeEvent::Rename{file_id:42,new_parent_id:2,new_name:"surviving.bin".into()}];
  for cursor in [30,40]{e.apply_volume_batch(b'C',&recovered,7,cursor).unwrap();assert!(!e.has_pending_namespace(b'C'));let page=e.usage_page("C:\\",None,None,4096).unwrap();assert_eq!(page.root.logical,150);assert_eq!(page.root.files,1);}
 }
}

#[cfg(test)]mod compact_aggregate_tests {
 use super::*;
 #[test]fn compact_counts_keep_public_u64_and_full_logical_range(){let mut d=DirAgg::empty(0,0);d.set_totals(Totals{bytes:u64::MAX,files:u32::MAX as u64,dirs:u32::MAX as u64,unknown:u32::MAX as u64}).unwrap();assert_eq!(std::mem::size_of::<DirAgg>(),48);assert_eq!(d.totals().bytes,u64::MAX);assert_eq!(d.totals().unknown,u32::MAX as u64);}
 #[test]fn overflowing_count_fails_atomically_not_wrapped(){let mut d=DirAgg::empty(0,0);d.set_totals(Totals::file(7,true)).unwrap();let old=d.totals();for v in [Totals{files:u32::MAX as u64+1,..old},Totals{dirs:u32::MAX as u64+1,..old},Totals{unknown:u32::MAX as u64+1,..old}]{assert!(d.set_totals(v).is_err());assert_eq!(d.totals(),old);}}
 #[test]fn ancestor_overflow_is_truthful_usage_error(){let mut r=UsageRuntime::default();r.dirs.push(DirAgg::empty(0,0));r.dirs[0].set_totals(Totals{files:u32::MAX as u64,..Totals::default()}).unwrap();r.ancestors(0,Totals::file(1,false),true);assert!(r.error.is_some());assert_eq!(r.dirs[0].totals().files,u32::MAX as u64);}
}

impl SearchEngine {
    /// A conservative exact-parent shortcut. Unsupported predicates and uncertain
    /// graph states return None, leaving the complete legacy query unchanged.
    #[inline(never)]
    pub(crate) fn direct_parent_candidates(&self, g: &IndexStore, q: &crate::ParsedQuery) -> Option<Vec<u32>> {
        let needle = q.parent_path.as_deref()?;
        let drive = q.drive?.to_ascii_uppercase() as u8;
        let mut supported = crate::ParsedQuery::default();
        supported.parent_path = q.parent_path.clone();
        supported.drive = q.drive;
        supported.sort_by = q.sort_by;
        supported.sort_desc = q.sort_desc;
        supported.limit = q.limit;
        supported.only_files = q.only_files;
        supported.only_dirs = q.only_dirs;
        if *q != supported || needle.split('/').any(|p| p == "." || p == "..") { return None; }
        let volumes: Vec<_> = g.volumes.iter().filter(|v| v.volume_letter == drive).collect();
        if volumes.len() != 1 || volumes[0].is_unix() { return None; }
        if !self.orphans.lock().ordered.is_empty() { return None; }
        let r = self.usage.lock();
        if r.epoch == 0 || r.error.is_some()
            || r.dirs.len() != g.dirs.len() || r.file_links.len() != g.entries.len() { return None; }
        let root = *r.roots.get(&drive)?;
        let flags = r.dirs.get(root as usize)?.flags;
        if flags & PARENT_SEARCH_VALID == 0
            || !needle.is_empty() && flags & PARENT_SEARCH_DEEP_VALID == 0 { return None; }
        let mut frontier = vec![root];
        // Enumerate ALL case-fold-equal branches, unlike the Usage view's
        // single-directory resolver. This also preserves case-sensitive NTFS
        // directories queried with the search language's insensitive parent key.
        for part in needle.split('/').filter(|p| !p.is_empty()) {
            let mut next = Vec::new();
            for &parent in &frontier {
                let mut d = r.dirs.get(parent as usize)?.first_dir;
                let mut steps = 0;
                while d != NIL {
                    let n = r.dirs.get(d as usize)?;
                    let raw = g.dirs.get(d as usize)?;
                    let name = if n.owner_entry != NIL { g.name_bytes(g.entries.get(n.owner_entry as usize)?) }
                        else { g.names_buf.get(raw.name_offset as usize..raw.name_offset as usize + raw.name_len as usize)? };
                    if String::from_utf8_lossy(name).eq_ignore_ascii_case(part) { next.push(d); }
                    d = n.next_dir;
                    steps += 1;
                    if steps > r.dirs.len() { return None; }
                }
            }
            // An unreachable old directory can still have indexed children.
            // Missing paths deliberately fall back; do not invent an empty set.
            if next.is_empty() { return None; }
            frontier = next;
        }
        let mut ids = Vec::new();
        for &parent in &frontier {
            let mut d = r.dirs.get(parent as usize)?.first_dir;
            let mut steps = 0;
            while d != NIL {
                let n = r.dirs.get(d as usize)?;
                if n.owner_entry != NIL { ids.push(n.owner_entry); }
                d = n.next_dir;
                steps += 1;
                if steps > r.dirs.len() { return None; }
            }
            let mut f = r.dirs.get(parent as usize)?.first_file;
            steps = 0;
            while f != NIL {
                ids.push(f);
                f = r.file_links.get(f as usize)?.next;
                steps += 1;
                if steps > g.entries.len() { return None; }
            }
            if parent == root {
                for (di, n) in r.dirs.iter().enumerate() {
                    if (di as u32 == root || n.flags & ROOT_ALIAS != 0 && r.logical_parent(di as u32) == root)
                        && n.owner_entry != NIL { ids.push(n.owner_entry); }
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids.retain(|&i| {
            let Some(e) = g.entries.get(i as usize) else { return false; };
            !e.is_deleted() && (!q.only_files || !e.is_dir_entry()) && (!q.only_dirs || e.is_dir_entry())
                && g.volume_for_entry(i as usize).is_some_and(|v| v.volume_letter == drive)
                && crate::search::match_parent_path_exact(g, i as usize, needle)
        });
        Some(ids)
    }
}

#[cfg(test)]
mod parent_direct_capability_tests {
 use super::*;
 use crate::{IndexBuilder, QueryParser, SearchOptions, RawEntry};
 fn raw(id:u64,parent:u64,name:&str,dir:bool)->RawEntry {RawEntry{file_id:id,file_id_128:None,parent_id:parent,name:name.into(),size:0,mtime:0,ctime:0,attrs:if dir{16}else{0},is_dir:dir}}
 fn engine()->SearchEngine {let mut s=IndexBuilder::new(b'C',1,2,3).build_from_raw(vec![raw(30,20,"inside",false),raw(31,10,"outside",false)],vec![raw(10,0,"",true),raw(20,10,"dev",true)],true).unwrap();s.set_track_mutations(true);let e=SearchEngine::new(s);e.enable_usage(16*1024*1024).unwrap();e}
 #[test]fn eligible_root_and_deep_use_links_not_fallback(){let e=engine();for text in ["parent:C:/","parent:C:/dev"]{let q=QueryParser::parse(text).unwrap();let direct=e.direct_parent_candidates(&e.store.read(),&q).expect("eligible branch must not silently scan");assert!(!direct.is_empty());let (legacy,total)=e.search(&q,&SearchOptions::default()).unwrap();assert_eq!(direct.len(),total as usize);let mut ids:Vec<_>=legacy.iter().map(|h|h.entry_idx).collect();ids.sort_unstable();assert_eq!(direct,ids);}}
 #[test]fn parked_namespace_disables_links_until_attachment(){let e=engine();let q=QueryParser::parse("parent:C:/").unwrap();e.apply_volume_batch(b'C',&[ChangeEvent::Create{entry:raw(44,999,"parked",false)}],2,4).unwrap();assert!(e.direct_parent_candidates(&e.store.read(),&q).is_none());e.apply_volume_batch(b'C',&[ChangeEvent::Create{entry:raw(999,10,"attached",true)}],2,5).unwrap();assert!(e.direct_parent_candidates(&e.store.read(),&q).is_some());let q=QueryParser::parse("parent:C:/attached").unwrap();assert_eq!(e.direct_parent_candidates(&e.store.read(),&q).unwrap().len(),1);}
 #[test]fn checkpoint_drain_preserves_graph_and_atomic_cursor(){let e=engine();e.apply_volume_batch(b'C',&[ChangeEvent::Create{entry:raw(32,20,"new",false)}],2,4).unwrap();let batch=e.take_persistence_batch().expect("mutation checkpoint");assert!(!batch.mutations.is_empty());let g=e.index_store();assert_eq!(batch.entry_count,g.entries.len());assert_eq!(batch.dir_count,g.dirs.len());assert_eq!(batch.volumes[0].last_usn,g.volumes[0].last_usn);assert_eq!(batch.volumes[0].last_usn,4);drop(g);let q=QueryParser::parse("parent:C:/dev").unwrap();let ids=e.direct_parent_candidates(&e.store.read(),&q).expect("checkpoint is not a namespace mutation");assert_eq!(ids.len(),2);assert!(e.take_persistence_batch().is_none());}
 #[test]fn uncertain_graph_and_predicates_fall_back(){let e=engine();for text in ["parent:dev","parent:C:/missing","parent:C:/dev size:>0","parent:C:/dev ext:log"]{let q=QueryParser::parse(text).unwrap();assert!(e.direct_parent_candidates(&e.store.read(),&q).is_none(),"{text}");}e.apply_volume_change(b'C',&ChangeEvent::Delete{file_id:20}).unwrap();let q=QueryParser::parse("parent:C:/dev").unwrap();assert!(e.direct_parent_candidates(&e.store.read(),&q).is_none());let q=QueryParser::parse("parent:C:/").unwrap();assert!(e.direct_parent_candidates(&e.store.read(),&q).is_some());drop(e.index_store_mut());assert!(e.direct_parent_candidates(&e.store.read(),&q).is_none());}
}

#[cfg(test)] mod namespace_binding_tests {
 use super::*;
 fn engine()->SearchEngine{
  let raw=|id,parent,name:&str,dir|crate::RawEntry{file_id:id,file_id_128:None,parent_id:parent,name:name.into(),size:if dir{0}else{7},mtime:0,ctime:0,attrs:if dir{16}else{0},is_dir:dir};
  let store=crate::IndexBuilder::new(b'F',123,7,10).build_from_raw(vec![raw(2,1,"keep.txt",false)],vec![raw(1,0,"root",true)],true).unwrap();SearchEngine::new(store)
 }
 #[test]fn namespace_binding_wrong_epoch_serial_or_journal_never_mutates_or_advances(){
  let e=engine();let epoch=e.metadata_epoch();let events=[crate::ChangeEvent::Delete{file_id:2}];
  for (ep,serial,journal) in [(epoch.wrapping_add(1),123,7),(epoch,124,7),(epoch,123,8)]{
   assert!(e.apply_volume_batch_bound(b'F',&events,journal,20,ep,serial).is_err());let g=e.index_store();assert_eq!(g.volumes[0].last_usn,10);assert!(!g.entries[g.entry_on_volume(b'F',2).unwrap() as usize].is_deleted());
  }
  e.apply_volume_batch_bound(b'F',&events,7,20,epoch,123).unwrap();assert_eq!(e.index_store().volumes[0].last_usn,20);
 }
}
