//! 按 `persist.rs` 的真实分段顺序走一遍 index.bin，打印每段偏移与长度。
//!
//! 用途：定位 `Persist("frns 计数异常（过大）")` 这类加载失败 —— 是文件损坏（长度对不上）
//! 还是命中了人为上限。`cargo run --release -p findx2-core --example index_layout_probe -- <index.bin>`

use std::path::PathBuf;

const FORMAT_VERSION_V2: u32 = 2;
const FORMAT_VERSION_V5: u32 = 5;
const FORMAT_VERSION_V6: u32 = 6;
const FILE_ENTRY_V5_DISK_SIZE: usize = 32;
const FILE_ENTRY_V4_DISK_SIZE: usize = 36;
/// 与 `persist.rs` 保持一致：`frns` 计数的上限是**相对** `entry_count` 的，
/// 不是固定常量。旧的固定常量 `64 * 1024 * 1024` 曾在 3 亿条目级的库上误杀合法索引。
fn frns_cap(entry_count: u64) -> u64 {
    entry_count.saturating_mul(8).saturating_add(1 << 20)
}

struct Cursor<'a> {
    d: &'a [u8],
    cur: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize, what: &str) -> Result<&'a [u8], String> {
        let end = self
            .cur
            .checked_add(n)
            .ok_or_else(|| format!("{what}: 长度溢出"))?;
        if end > self.d.len() {
            return Err(format!(
                "{what}: 越界（需要 {n} 字节，偏移 {} 处只剩 {}）",
                self.cur,
                self.d.len().saturating_sub(self.cur)
            ));
        }
        let s = &self.d[self.cur..end];
        self.cur = end;
        Ok(s)
    }
    fn u64(&mut self, what: &str) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8, what)?.try_into().unwrap()))
    }
}

fn main() {
    let path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("index.bin"));
    let d = std::fs::read(&path).expect("read");
    println!(
        "文件: {}  {} 字节 / {:.2} GB",
        path.display(),
        d.len(),
        d.len() as f64 / 1073741824.0
    );

    let mut c = Cursor { d: &d, cur: 0 };
    let h = c.take(64, "header").expect("header");
    let magic = u32::from_le_bytes(h[0..4].try_into().unwrap());
    let version = u32::from_le_bytes(h[4..8].try_into().unwrap());
    let entry_count = u64::from_le_bytes(h[8..16].try_into().unwrap());
    let dir_count = u64::from_le_bytes(h[16..24].try_into().unwrap());
    let names_len = u64::from_le_bytes(h[24..32].try_into().unwrap());
    let flags = u32::from_le_bytes(h[32..36].try_into().unwrap());
    println!("magic=0x{magic:X} version={version} flags={flags}");
    println!("entry_count={entry_count}  dir_count={dir_count}  names_buf_len={names_len}");

    // IndexHeader::read 把 h[36..40] 当 u32 读出来作为 nvol。
    let nvol = u32::from_le_bytes(h[36..40].try_into().unwrap()) as usize;
    println!("nvol={nvol}");

    macro_rules! seg {
        ($name:expr, $n:expr) => {{
            let before = c.cur;
            match c.take($n, &$name) {
                Ok(_) => println!("  {:>6}..{:<12} {:>12} 字节  {}", before, c.cur, $n, $name),
                Err(e) => {
                    println!("  ✗ {e}");
                    return;
                }
            }
        }};
    }

    println!("\n── 分段 ──");
    seg!("卷表", nvol * 32);

    if version >= FORMAT_VERSION_V6 {
        for i in 0..nvol {
            let n = match c.take(2, "root_prefix len") {
                Ok(b) => u16::from_le_bytes(b.try_into().unwrap()) as usize,
                Err(e) => {
                    println!("  ✗ {e}");
                    return;
                }
            };
            seg!(format!("root_prefix[{i}]"), n);
            let n2 = match c.take(2, "volume_id len") {
                Ok(b) => u16::from_le_bytes(b.try_into().unwrap()) as usize,
                Err(e) => {
                    println!("  ✗ {e}");
                    return;
                }
            };
            seg!(format!("volume_id[{i}]"), n2);
        }
    }

    seg!("names_buf", names_len as usize);

    if version >= FORMAT_VERSION_V2 {
        let dlen = match c.u64("dir_paths len") {
            Ok(v) => v as usize,
            Err(e) => {
                println!("  ✗ {e}");
                return;
            }
        };
        seg!("dir_paths_buf", dlen);
        let nr = match c.u64("dir_path_ranges count") {
            Ok(v) => v as usize,
            Err(e) => {
                println!("  ✗ {e}");
                return;
            }
        };
        println!("  （dir_path_ranges 条数 = {nr}）");
        seg!("dir_path_ranges", nr * 8);
    }

    let esz = if version >= FORMAT_VERSION_V5 {
        FILE_ENTRY_V5_DISK_SIZE
    } else {
        FILE_ENTRY_V4_DISK_SIZE
    };
    println!("  （每条目 {esz} 字节）");
    seg!("entries", entry_count as usize * esz);
    seg!("dirs", dir_count as usize * 24);

    // frns：先 8 字节计数，再 n*8
    let frns_off = c.cur;
    let n_stored = match c.u64("frns count") {
        Ok(v) => v as usize,
        Err(e) => {
            println!("  ✗ {e}");
            return;
        }
    };
    println!(
        "  frns 段: 计数字段在偏移 {frns_off}，值 = {n_stored}（{:.3} 亿）",
        n_stored as f64 / 1e8
    );
    let cap = frns_cap(entry_count);
    println!(
        "  阈值 = entry_count×8 + 1 MiB = {cap}（entry_count = {entry_count}）"
    );
    if n_stored as u64 > cap {
        println!("  ✗ 超过阈值 {n_stored} > {cap} → 加载会在此报错");
        println!(
            "    该计数若视为条数，数据区需 {} GB",
            n_stored as f64 * 8.0 / 1073741824.0
        );
    } else {
        println!("  ✓ 未超阈值（计数与 entry_count 一致则属正常布局）");
        seg!("frns 数据", n_stored * 8);
    }
    println!("\n解析到此偏移 {} / 总 {}", c.cur, d.len());
}
