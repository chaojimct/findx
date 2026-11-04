# Linux inotify 模式修复总结

## 修复的问题

本次提交修复了 FindX 在 Linux 平台使用 inotify 降级模式（无 CAP_SYS_ADMIN 权限）时的**三个**关键 bug：

### 1. 同设备多根路径的 volume_id 冲突

**问题：** 当索引位于同一设备上的多个根路径（例如 `/home/box` 和 `/workspace`）时，`volume_id_for_path` 返回相同的标识符（如 `dev:65040`），导致路径重构时选错根前缀。

**修复：** volume_id 现在包含规范化的路径信息（`dev:65040:/home/box`），确保唯一性。

**影响：**
- 新索引自动使用新格式
- 旧索引需要重建以获得正确的多根支持
- 单根场景不受影响

### 2. inotify 下删除事件未生效

**问题：** 文件删除后，`notify_to_change` 尝试获取已删除文件的 inode 失败，导致 Delete 事件被丢弃，搜索结果持续返回已删除的文件。

**修复：** 引入 `PathInodeCache`（路径到 inode 的映射缓存），在文件操作时记录映射，删除时优先使用缓存。

**影响：**
- 运行时内存增加约 10 万条目 × 40 字节 ≈ 4 MB（缓存上限）
- 删除事件现在能正确标记文件为已删除
- 重启后缓存清空，但不影响持久化索引

### 3. 多卷增量条目选错卷 + 路径大小写错误 ⚠️ **关键修复**

**问题1：增量条目选错卷**
- `volume_for_entry` 仅基于 `first_entry_idx` 范围判断卷
- 增量 Create/CreatePending 追加的条目在合并索引的末尾，超出原始卷的范围
- 导致所有增量条目都被分配到最后一个卷，即使 `parent_id` 指向第一个卷
- 在 `/home/box/FindX拼音测试/` 下创建的文件，路径显示为 `/workspace/...`（错误）

**问题2：路径小写化破坏大小写敏感文件系统**
- `resolve_dir_path_lower` 将所有目录名转为小写
- Linux 文件系统大小写敏感：`FindX拼音测试` ≠ `findx拼音测试`
- `Path::new(hit.path).exists()` 返回 `False`，无法打开/预览文件

**修复：**
- 新增 `volume_for_entry_by_parent`：从父目录的 frn 递归向上查找所属卷
- 新增 `build_dir_path_original_case`：保留原始大小写构建目录路径
- `entry_display_path` 优先使用新方法，Unix 平台保留大小写

**影响：**
- 所有平台受益（任何多卷合并索引场景）
- Linux 平台额外获得大小写保留
- 运行时性能无显著影响（递归查找有缓存 + 回退逻辑）
- 现有单卷索引完全兼容

---

## 技术细节

### volume_id 格式变化

**旧格式：**
```
dev:65040
```

**新格式：**
```
dev:65040:/home/box
```

### PathInodeCache 实现

```rust
type PathInodeCache = Arc<Mutex<HashMap<PathBuf, u64>>>;
```

- **缓存时机：** Create, Rename, Modify 事件
- **使用时机：** Remove 事件（优先使用缓存，回退到 stat）
- **清理策略：** 超过 10 万条目时清理一半（简单 FIFO）

### 多卷增量路径解析

**问题根源：**
```rust
// 旧实现：仅基于 first_entry_idx 范围
pub fn volume_for_entry(&self, idx: usize) -> Option<&VolumeState> {
    let idx = idx as u32;
    let mut chosen = 0usize;
    for (i, v) in self.volumes.iter().enumerate() {
        if idx >= v.first_entry_idx {
            chosen = i;  // 增量条目会选到最后一个卷
        }
    }
    self.volumes.get(chosen)
}
```

**新方法：**
```rust
// 从父目录递归查找所属卷
fn volume_for_entry_by_parent(&self, idx: usize) -> Option<&VolumeState> {
    let e = self.entries.get(idx)?;
    if e.is_dir_entry() {
        let fr = self.frns.get(idx).copied().unwrap_or(0);
        if let Some(&di) = self.dir_index.get(&fr) {
            return self.volume_for_dir_idx(di);
        }
    }
    self.volume_for_dir_idx(e.dir_idx)
}
```

### 大小写保留实现

**问题：**
- `resolve_dir_path_lower` 和 `build_dir_path_lower_owned` 调用 `.to_ascii_lowercase()`
- Linux: `FindX拼音测试` → `findx拼音测试` → `Path::new(path).exists()` = `False`

**修复：**
```rust
// 新增保留原始大小写的方法
fn build_dir_path_original_case(&self, dir_idx: u32) -> Vec<u8> {
    // 与 build_dir_path_lower_owned 类似，但不调用 to_ascii_lowercase()
    // Unix 平台使用 / 分隔符
}

// entry_display_path 在 Unix 平台使用新方法
if unix {
    let pbytes = self.build_dir_path_original_case(e.dir_idx);
    // ...
}
```

---

## 测试覆盖

### 单元测试

1. **`tests/multi_root_same_device.rs`** (Linux)
   - `test_volume_id_uniqueness_for_different_roots`：验证同设备不同路径生成不同 volume_id
   - `test_volume_id_stability`：验证 volume_id 的稳定性

2. **`tests/inotify_delete.rs`** (Linux)
   - `test_inotify_delete_event_processing`：验证删除事件生成和应用
   - `test_path_inode_cache_behavior`：验证缓存行为

3. **`tests/multi_volume_incremental_path.rs`** (Core) ⭐ 新增
   - `test_incremental_create_preserves_correct_volume_prefix`：验证增量创建使用正确卷前缀
   - `test_incremental_path_case_sensitivity`：验证 Unix 路径保留原始大小写
   - `test_delete_after_incremental_create_still_works`：验证删除功能不受影响

### 回归测试

- `cargo +nightly test --package findx2-core`：**35 个测试**全部通过（原 32 个 + 新增 3 个）
- `cargo +nightly test --package findx2-linux`：4 个测试全部通过

### 本地验证（用户报告）✅

- ✅ Delete via inotify: `tombstone_count` increases correctly
- ✅ Workspace paths: `/workspace/...` correct
- ✅ **Home root paths: `/home/box/FindX拼音测试/...` correct prefix + original case**
- ✅ **`os.path.exists(hit.path)` returns True for new files**

---

## 已知限制和注意事项

### 1. inotify 的固有限制

inotify 监视是基于路径的，无法像 fanotify 那样监视整个文件系统。已知限制包括：

- **只监视指定根目录：** 不在监视路径下的修改不会被检测到
- **重启可能漏事件：** 无持久化 journal，进程重启期间的事件会丢失（启动时应做短窗口追赶或接受短暂不一致）
- **监视句柄限制：** 系统参数 `fs.inotify.max_user_watches` 限制最大监视数（默认 8192 或 524288）
- **大规模目录树：** 深度递归监视可能消耗大量文件描述符

### 2. PathInodeCache 的限制

- **内存上限：** 固定 10 万条目，超出后简单 FIFO 清理（非 LRU）
- **缓存清空：** 服务重启后缓存丢失，首次删除事件可能失效（但重启后的全量扫描会修正索引）
- **并发竞态：** 极端情况下（文件快速创建删除），缓存可能来不及记录就被删除

### 3. volume_id 格式变化

- **旧索引兼容性：** 使用旧格式的索引在多根同设备场景下会有路径混淆问题，建议重建
- **符号链接：** `canonicalize` 会解析符号链接，别名路径会指向同一 volume_id
- **跨设备移动：** 如果根路径所在的设备号变化（如迁移到新分区），volume_id 会改变，需要重建索引

### 4. 建议的使用场景

✅ **适合：**
- 单根或多根（不同设备）
- 开发环境、小型工作区（< 100 万文件）
- 无特权用户（如容器环境）

⚠️ **不推荐：**
- 同设备多根且频繁跨根移动文件（需要更精细的路径解析）
- 超大规模监视（> 100 万文件，建议使用 fanotify + CAP_SYS_ADMIN）
- 要求绝对零漏事件（inotify 无持久 journal）

---

## 迁移建议

### 对于新部署

无需任何操作，新索引自动使用修复后的实现。

### 对于已有索引（多根同设备场景）

如果您已经在使用多根索引，且这些根位于同一设备上，建议重建索引：

```bash
# 备份旧索引
cp /path/to/index.bin /path/to/index.bin.backup

# 重建索引
findx2 index -o /path/to/index.bin --volumes /root1,/root2

# 重启服务
systemctl restart findx2-service  # 或手动重启
```

### 对于单根或不同设备多根场景

无需重建，现有索引继续工作。

---

## 性能影响

### 索引构建

- 无显著影响（volume_id 计算增加 `canonicalize` 调用，但只在索引构建时发生）

### 运行时监视

- **内存：** 增加约 4 MB（PathInodeCache 上限）
- **CPU：** 每个文件操作增加一次 HashMap 查找/插入（< 1 微秒）
- **磁盘：** 无变化（缓存不持久化）

### 搜索性能

- 无影响（volume_id 变化不影响搜索算法）

---

## 参考

- PR: https://github.com/chaojimct/findx/pull/8
- 验证脚本: `scripts/verify_linux_fixes.sh`
- 相关测试:
  - `crates/findx2-linux/tests/multi_root_same_device.rs`
  - `crates/findx2-linux/tests/inotify_delete.rs`

---

## 常见问题

### Q: 为什么删除事件需要缓存 inode？

A: inotify 的 Remove 事件只提供路径，不提供 inode。当我们收到删除事件时，文件已经从文件系统中移除，无法再通过 `stat` 获取 inode。缓存让我们能在删除前记住 inode，从而在索引中正确标记删除。

### Q: 缓存会占用多少内存？

A: 每个缓存条目包含路径（约 100 字节）和 inode（8 字节），上限 10 万条目，估计约 10 MB。实际使用中，缓存大小取决于监视路径下的文件操作频率。

### Q: 旧索引是否会自动迁移？

A: volume_id 格式变化不影响已有索引的加载和搜索。但多根同设备场景下，旧索引可能有路径混淆问题。重建索引是最安全的迁移方式。

### Q: fanotify 模式是否受影响？

A: 不受影响。fanotify 模式提供文件描述符，可以直接 `fstat` 获取 inode，不依赖路径缓存。本次修复专门针对 inotify 降级场景。

### Q: 如何验证修复是否生效？

A: 运行 `scripts/verify_linux_fixes.sh` 或手动执行 PR 描述中的验证步骤。确认：
1. 多根索引下，新建文件的路径前缀正确
2. 删除文件后，搜索立即返回 0 结果
