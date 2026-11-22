//! Overlay layout bake-off at production scale (N=10M):
//! dense Vec<AtomicU64> x2  vs  FxHashMap<u32,(u64,u32,u32)>  vs
//! sorted-Vec sidecar (Vec<u32> idx + parallel values, binary search).
//! Measures resident bytes + random-read latency at 10/50/100% fill.
//! Run: cargo run --release --example overlay_bench -p findx2-core
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use fxhash::FxHashMap;

const N: usize = 10_000_000;
const READS: usize = 1_000_000;

// Deterministic xorshift64 (no extra deps).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

fn main() {
    for &fill in &[0.1f64, 0.5, 1.0] {
        let m = (N as f64 * fill) as usize;
        println!("=== fill {fill} (m={m}) ===");

        // ---- dense: 2 x Vec<AtomicU64>, 16B/entry always ----
        let t = Instant::now();
        let mut ds = Vec::with_capacity(N);
        let mut dm = Vec::with_capacity(N);
        for _ in 0..N {
            ds.push(AtomicU64::new(u64::MAX));
            dm.push(AtomicU64::new(0));
        }
        let mut rng = Rng(0x1234);
        for _ in 0..m {
            let i = (rng.next() % N as u64) as usize;
            ds[i].store(100, Ordering::Relaxed);
            dm[i].store(1, Ordering::Relaxed);
        }
        let build = t.elapsed();
        let bytes = ds.capacity() * 8 + dm.capacity() * 8;
        let mut rng = Rng(0x5678);
        let mut sink = 0u64;
        let t = Instant::now();
        for _ in 0..READS {
            let i = (rng.next() % N as u64) as usize;
            let s = ds[i].load(Ordering::Relaxed);
            if s != u64::MAX {
                sink = sink.wrapping_add(s.wrapping_add(dm[i].load(Ordering::Relaxed)));
            }
        }
        let per_read = t.elapsed().as_nanos() as f64 / READS as f64;
        println!("dense   : {bytes:>12} bytes  build={build:?}  read={per_read:.1}ns/op sink={sink}");
        drop((ds, dm));

        // ---- sparse: FxHashMap ----
        let t = Instant::now();
        let mut hm: FxHashMap<u32, (u64, u32, u32)> = FxHashMap::default();
        hm.reserve(m);
        let mut rng = Rng(0x1234);
        for _ in 0..m {
            let i = (rng.next() % N as u64) as u32;
            hm.insert(i, (100, 1, 2));
        }
        let build = t.elapsed();
        // Resident estimate: buckets (power of 2, load ~0.7) * (8B ctrl+slot overhead approx 16B/entry incl. overhead) + payload.
        // Measured below via capacity-ish accounting: len + bucket count.
        let buckets = hm.capacity();
        let bytes_est = buckets * 16 + hm.len() * 16;
        let mut rng = Rng(0x5678);
        let mut sink = 0u64;
        let t = Instant::now();
        for _ in 0..READS {
            let i = (rng.next() % N as u64) as u32;
            if let Some(&(s, m1, m2)) = hm.get(&i) {
                sink = sink.wrapping_add(s.wrapping_add(m1 as u64).wrapping_add(m2 as u64));
            }
        }
        let per_read = t.elapsed().as_nanos() as f64 / READS as f64;
        println!("fxhash  : ~{bytes_est:>11} bytes  build={build:?}  read={per_read:.1}ns/op sink={sink} (buckets={buckets} len={})", hm.len());
        drop(hm);

        // ---- sparse: std HashMap (baseline of record: the documented 430MB claim) ----
        let t = Instant::now();
        let mut hm: HashMap<u32, (u64, u32, u32)> = HashMap::with_capacity(m);
        let mut rng = Rng(0x1234);
        for _ in 0..m {
            let i = (rng.next() % N as u64) as u32;
            hm.insert(i, (100, 1, 2));
        }
        let build = t.elapsed();
        let buckets = hm.capacity();
        let bytes_est = buckets * 16 + hm.len() * 16;
        let mut rng = Rng(0x5678);
        let mut sink = 0u64;
        let t = Instant::now();
        for _ in 0..READS {
            let i = (rng.next() % N as u64) as u32;
            if let Some(&(s, m1, m2)) = hm.get(&i) {
                sink = sink.wrapping_add(s.wrapping_add(m1 as u64).wrapping_add(m2 as u64));
            }
        }
        let per_read = t.elapsed().as_nanos() as f64 / READS as f64;
        println!("stdhash : ~{bytes_est:>11} bytes  build={build:?}  read={per_read:.1}ns/op sink={sink} (buckets={buckets} len={})", hm.len());
        drop(hm);

        // ---- sparse: sorted sidecar (Vec<u32> + parallel values, binary search) ----
        let t = Instant::now();
        let mut idx: Vec<u32> = (0..m as u32).collect();
        // Simulate scattered fill: shuffle-ish via strided order (already sorted here;
        // use a pseudo-random permutation through sort by hash for realism).
        idx.sort_by_key(|&i| (i as u64).wrapping_mul(0x9E3779B97F4A7C15));
        idx.sort_unstable();
        idx.dedup();
        let vals: Vec<(u64, u32, u32)> = idx.iter().map(|_| (100, 1, 2)).collect();
        let build = t.elapsed();
        let bytes = idx.capacity() * 4 + vals.capacity() * 16;
        let mut rng = Rng(0x5678);
        let mut sink = 0u64;
        let t = Instant::now();
        for _ in 0..READS {
            let i = (rng.next() % N as u64) as u32;
            if let Ok(pos) = idx.binary_search(&i) {
                let (s, m1, m2) = vals[pos];
                sink = sink.wrapping_add(s.wrapping_add(m1 as u64).wrapping_add(m2 as u64));
            }
        }
        let per_read = t.elapsed().as_nanos() as f64 / READS as f64;
        println!("sidecar : {bytes:>12} bytes  build={build:?}  read={per_read:.1}ns/op sink={sink} (len={})", idx.len());
        println!();
    }
}
