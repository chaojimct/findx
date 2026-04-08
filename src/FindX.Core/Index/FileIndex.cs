using System.Buffers;
using System.Runtime.InteropServices;
using System.Text;
using FindX.Core.Interop;

namespace FindX.Core.Index;

/// <summary>
/// 主索引：Rust <c>findx_engine</c> 紧凑存储（字符串池 + 排序数组 文件名/拼音序），USN 增量 O(log n) 立即参与检索。
/// </summary>
public sealed class FileIndex
{
    /// <summary>单次前缀检索从 Rust 引擎取回的最大条数。排序数组下此值仅影响返回缓冲区大小（4B×8192=32KB），开销很小。</summary>
    internal const int PrefixSearchHitCap = 8192;

    private const int SearchIndexCap = PrefixSearchHitCap;

    private readonly ReaderWriterLockSlim _lock = new();
    private IntPtr _engine;

    private static readonly RustIndexNative.PersistRowFn PersistDelegate = PersistRowStatic;
    private static readonly RustIndexNative.VisitLiveFn VisitLiveDelegate = VisitLiveStatic;

    [ThreadStatic] private static BinaryWriter? _persistBw;

    [ThreadStatic] private static GCHandle _visitHandle;
    [ThreadStatic] private static Func<FileEntry, int, bool>? _visitFn;

    public FileIndex()
    {
        _engine = RustIndexNative.findx_engine_create();
        if (_engine == IntPtr.Zero)
            throw new InvalidOperationException(
                "findx_engine_create 失败：请将与配置匹配的 findx_engine.dll 放在进程目录（通常与 FindX.exe 同级），并确保已用 cargo 编译 native/findx-engine。");
    }

    ~FileIndex()
    {
        if (_engine != IntPtr.Zero)
        {
            RustIndexNative.findx_engine_destroy(_engine);
            _engine = IntPtr.Zero;
        }
    }

    public int Count
    {
        get
        {
            _lock.EnterReadLock();
            try { return RustIndexNative.findx_engine_live_count(_engine); }
            finally { _lock.ExitReadLock(); }
        }
    }

    public int CountSnapshot => RustIndexNative.findx_engine_live_count(_engine);

    public bool IsInBulkLoad => RustIndexNative.findx_engine_is_in_bulk_load(_engine) != 0;

    public void AddEntry(FileEntry entry)
    {
        entry.IsDeleted = false;
        _lock.EnterWriteLock();
        try { WriteEntryUtf16(entry); }
        finally { _lock.ExitWriteLock(); }
    }

    public void UpsertEntry(FileEntry entry)
    {
        entry.IsDeleted = false;
        _lock.EnterWriteLock();
        try { WriteUpsertUtf16(entry); }
        finally { _lock.ExitWriteLock(); }
    }

    /// <summary>
    /// 与 <see cref="EndBulk"/> 配对：期间只追加记录不建 BTree，结束时一次性重建（全盘加载/首次扫盘）。
    /// </summary>
    public void BeginBulk()
    {
        _lock.EnterWriteLock();
        try { RustIndexNative.findx_engine_begin_bulk(_engine); }
        finally { _lock.ExitWriteLock(); }
    }

    /// <summary>结束批量模式并触发原生排序索引构建（含拼音批处理）。</summary>
    public void EndBulk()
    {
        _lock.EnterWriteLock();
        try { RustIndexNative.findx_engine_end_bulk(_engine); }
        finally { _lock.ExitWriteLock(); }
    }

    public void AddBulk(IReadOnlyList<FileEntry> entries)
    {
        const int chunk = 8192;
        for (int offset = 0; offset < entries.Count; offset += chunk)
        {
            var n = Math.Min(chunk, entries.Count - offset);
            AddBulkChunk(entries, offset, n);
        }
    }

    private void AddBulkChunk(IReadOnlyList<FileEntry> entries, int offset, int n)
    {
        _lock.EnterWriteLock();
        try
        {
            for (int j = 0; j < n; j++)
            {
                var e = entries[offset + j];
                e.IsDeleted = false;
                WriteEntryUtf16(e);
            }
        }
        finally { _lock.ExitWriteLock(); }
    }

    private unsafe void WriteEntryUtf16(char vol, ulong fileRef, ulong parentRef, string name, uint attr, long size,
        long mtime)
    {
        fixed (char* pn = name)
        {
            RustIndexNative.findx_engine_add_entry_utf16(_engine, vol, fileRef, parentRef, (IntPtr)pn, name.Length, attr,
                size, mtime);
        }
    }

    private unsafe void WriteEntryUtf16(FileEntry e) =>
        WriteEntryUtf16(e.VolumeLetter, e.FileRef, e.ParentRef, e.Name, e.Attributes, e.Size, e.LastWriteTimeTicks);

    private unsafe void WriteUpsertUtf16(FileEntry entry)
    {
        fixed (char* pn = entry.Name)
        {
            RustIndexNative.findx_engine_upsert_entry_utf16(_engine, entry.VolumeLetter, entry.FileRef, entry.ParentRef,
                (IntPtr)pn, entry.Name.Length, entry.Attributes, entry.Size, entry.LastWriteTimeTicks);
        }
    }

    public void RebuildNameIndex()
    {
        _lock.EnterWriteLock();
        try { RustIndexNative.findx_engine_rebuild_indexes(_engine); }
        finally { _lock.ExitWriteLock(); }
    }

    /// <summary>
    /// 在任意前缀搜索前调用一次：有 live 条且非 Bulk、但三棵 BTree 未齐时单次 rebuild。
    /// 避免 GatherCandidates 里连打 3 次 native 搜索各自 while(-3) 反复全量重建或卡死 IPC。
    /// </summary>
    internal void EnsureSearchIndexesReady()
    {
        if (RustIndexNative.findx_engine_live_count(_engine) == 0) return;
        if (RustIndexNative.findx_engine_is_in_bulk_load(_engine) != 0) return;
        if (RustIndexNative.findx_engine_is_sort_ready(_engine) != 0) return;

        _lock.EnterWriteLock();
        try
        {
            if (RustIndexNative.findx_engine_live_count(_engine) == 0) return;
            if (RustIndexNative.findx_engine_is_in_bulk_load(_engine) != 0) return;
            if (RustIndexNative.findx_engine_is_sort_ready(_engine) != 0) return;
            RustIndexNative.findx_engine_rebuild_indexes(_engine);
        }
        finally { _lock.ExitWriteLock(); }
    }

    public List<int> SearchNamePrefix(string prefixLower, int maxResults)
    {
        if (maxResults <= 0)
            return new List<int>();
        EnsureSearchIndexesReady();
        if (CountSnapshot == 0)
            return new List<int>();

        var rent = ArrayPool<uint>.Shared.Rent(SearchIndexCap);
        var utfRent = ArrayPool<byte>.Shared.Rent(Math.Max(1024, Encoding.UTF8.GetMaxByteCount(prefixLower.Length)));
        try
        {
            int blen = Encoding.UTF8.GetBytes(prefixLower.AsSpan(), utfRent);
            int rc;
            _lock.EnterReadLock();
            try
            {
                unsafe
                {
                    fixed (byte* pb = utfRent)
                    fixed (uint* po = rent)
                    {
                        rc = RustIndexNative.findx_engine_search_name_prefix(_engine, (IntPtr)pb, blen, (IntPtr)po,
                            SearchIndexCap);
                    }
                }
            }
            finally { _lock.ExitReadLock(); }

            if (rc < 0)
                return new List<int>();

            var list = new List<int>(Math.Min(rc, maxResults));
            int take = Math.Min(rc, maxResults);
            for (int i = 0; i < take; i++)
                list.Add((int)rent[i]);
            return list;
        }
        finally
        {
            ArrayPool<uint>.Shared.Return(rent);
            ArrayPool<byte>.Shared.Return(utfRent);
        }
    }

    public List<int> SearchPinyinInitialsPrefix(string prefixLower, int maxResults)
    {
        if (maxResults <= 0)
            return new List<int>();
        EnsureSearchIndexesReady();
        if (CountSnapshot == 0)
            return new List<int>();

        var rent = ArrayPool<uint>.Shared.Rent(SearchIndexCap);
        var utfRent = ArrayPool<byte>.Shared.Rent(Math.Max(1024, Encoding.UTF8.GetMaxByteCount(prefixLower.Length)));
        try
        {
            int blen = Encoding.UTF8.GetBytes(prefixLower.AsSpan(), utfRent);
            int rc;
            _lock.EnterReadLock();
            try
            {
                unsafe
                {
                    fixed (byte* pb = utfRent)
                    fixed (uint* po = rent)
                    {
                        rc = RustIndexNative.findx_engine_search_pinyin_prefix(_engine, (IntPtr)pb, blen, (IntPtr)po,
                            SearchIndexCap);
                    }
                }
            }
            finally { _lock.ExitReadLock(); }

            if (rc < 0)
                return new List<int>();

            var list = new List<int>(Math.Min(rc, maxResults));
            int take = Math.Min(rc, maxResults);
            for (int i = 0; i < take; i++)
                list.Add((int)rent[i]);
            return list;
        }
        finally
        {
            ArrayPool<uint>.Shared.Return(rent);
            ArrayPool<byte>.Shared.Return(utfRent);
        }
    }

    /// <summary>连续全拼 ASCII 前缀（如 nihao），由 Rust 全拼索引服务，避免 CJK 40 万次 DP。</summary>
    public List<int> SearchFullPinyinCompactPrefix(string prefixLower, int maxResults)
    {
        if (maxResults <= 0)
            return new List<int>();
        EnsureSearchIndexesReady();
        if (CountSnapshot == 0)
            return new List<int>();

        var rent = ArrayPool<uint>.Shared.Rent(SearchIndexCap);
        var utfRent = ArrayPool<byte>.Shared.Rent(Math.Max(1024, Encoding.UTF8.GetMaxByteCount(prefixLower.Length)));
        try
        {
            int blen = Encoding.UTF8.GetBytes(prefixLower.AsSpan(), utfRent);
            int rc;
            _lock.EnterReadLock();
            try
            {
                unsafe
                {
                    fixed (byte* pb = utfRent)
                    fixed (uint* po = rent)
                    {
                        rc = RustIndexNative.findx_engine_search_full_py_prefix(_engine, (IntPtr)pb, blen, (IntPtr)po,
                            SearchIndexCap);
                    }
                }
            }
            finally { _lock.ExitReadLock(); }

            if (rc < 0)
                return new List<int>();

            var list = new List<int>(Math.Min(rc, maxResults));
            int take = Math.Min(rc, maxResults);
            for (int i = 0; i < take; i++)
                list.Add((int)rent[i]);
            return list;
        }
        finally
        {
            ArrayPool<uint>.Shared.Return(rent);
            ArrayPool<byte>.Shared.Return(utfRent);
        }
    }

    public void ForEachLiveEntry(Func<FileEntry, int, bool> visitor)
    {
        _lock.EnterReadLock();
        try
        {
            _visitFn = visitor;
            _visitHandle = GCHandle.Alloc(this);
            RustIndexNative.findx_engine_visit_live(_engine, GCHandle.ToIntPtr(_visitHandle), VisitLiveDelegate);
        }
        finally
        {
            if (_visitHandle.IsAllocated)
                _visitHandle.Free();
            _visitFn = null;
            _lock.ExitReadLock();
        }
    }

    public FileEntry? GetByRef(char vol, ulong fileRef)
    {
        _lock.EnterReadLock();
        try
        {
            if (RustIndexNative.findx_engine_try_get_index(_engine, vol, fileRef, out var idx) == 0 || idx < 0)
                return null;
            return MaterializeCore(idx);
        }
        finally { _lock.ExitReadLock(); }
    }

    public FileEntry? GetByIndex(int idx)
    {
        _lock.EnterReadLock();
        try { return MaterializeCore(idx); }
        finally { _lock.ExitReadLock(); }
    }

    public string BuildFullPath(int idx)
    {
        _lock.EnterReadLock();
        try
        {
            Span<char> span = stackalloc char[32768];
            unsafe
            {
                fixed (char* pc = span)
                {
                    int n = RustIndexNative.findx_engine_build_path_utf16(_engine, idx, (IntPtr)pc, span.Length);
                    if (n <= 0)
                        return string.Empty;
                    return new string(span[..n]);
                }
            }
        }
        finally { _lock.ExitReadLock(); }
    }

    public void RemoveByRef(char vol, ulong fileRef)
    {
        _lock.EnterWriteLock();
        try { RustIndexNative.findx_engine_remove_by_ref(_engine, vol, fileRef); }
        finally { _lock.ExitWriteLock(); }
    }

    public void WritePersistedEntries(BinaryWriter bw, Dictionary<char, ulong> volumeUsns)
    {
        _lock.EnterReadLock();
        try
        {
            var live = RustIndexNative.findx_engine_live_count(_engine);
            bw.Write(live);
            bw.Write(volumeUsns.Count);
            _persistBw = bw;
            try { RustIndexNative.findx_engine_for_each_persist(_engine, IntPtr.Zero, PersistDelegate); }
            finally { _persistBw = null; }
        }
        finally { _lock.ExitReadLock(); }

        foreach (var (vol, usn) in volumeUsns)
        {
            bw.Write(vol);
            bw.Write(usn);
        }
    }

    public IReadOnlyList<FileEntry> GetAllEntries() => Array.Empty<FileEntry>();

    /// <summary>
    /// 直接将 Rust 引擎内存状态（含排序索引）序列化到文件，加载时无需 rebuild。
    /// Volume USN 附加在 Rust 数据之后。
    /// </summary>
    public unsafe void SaveBinary(string path, Dictionary<char, ulong> volumeUsns)
    {
        _lock.EnterReadLock();
        try
        {
            fixed (char* pp = path)
            {
                var rc = RustIndexNative.findx_engine_save_file(_engine, (IntPtr)pp, path.Length);
                if (rc != 0) throw new IOException($"findx_engine_save_file failed: {rc}");
            }
        }
        finally { _lock.ExitReadLock(); }

        using var fs = new FileStream(path, FileMode.Open, FileAccess.Write, FileShare.None);
        fs.Seek(0, SeekOrigin.End);
        using var bw = new BinaryWriter(fs);
        bw.Write(volumeUsns.Count);
        foreach (var (vol, usn) in volumeUsns)
        {
            bw.Write(vol);
            bw.Write(usn);
        }
    }

    /// <summary>
    /// 从二进制文件直接恢复 Rust 引擎状态（含排序索引），不需要 BeginBulk/EndBulk。
    /// </summary>
    /// <returns>live 条目数；失败返回 -1。</returns>
    public unsafe int LoadBinary(string path, Dictionary<char, ulong> volumeUsns)
    {
        int live;
        _lock.EnterWriteLock();
        try
        {
            fixed (char* pp = path)
            {
                live = RustIndexNative.findx_engine_load_file(_engine, (IntPtr)pp, path.Length);
            }
        }
        finally { _lock.ExitWriteLock(); }

        if (live < 0) return -1;

        var fi = new FileInfo(path);
        var rustDataEnd = fi.Length - sizeof(int); // at least usn_count(4)
        using var fs = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read);
        // Rust data is followed by: usn_count(4) + [vol(2) + usn(8)] × N
        // We need to find where Rust data ends. Since Rust writes the file then C# appends,
        // we read from the end backwards.
        // usn section size = 4 + usnCount × 10
        // Try: seek to end - 4 to read usnCount first
        fs.Seek(-4, SeekOrigin.End);
        using var br = new BinaryReader(fs);
        // We don't know usnCount yet from end. Instead, use the approach:
        // After Rust save, the Rust data occupies bytes 0..X. Then C# writes:
        //   usnCount(4) + usnCount×(vol(2)+usn(8))
        // Total appendix = 4 + usnCount×10
        // We need to try reading from various offsets. Simpler: save usnCount at end too.
        // BUT the current format already saves usnCount first then data.
        // We have no reliable way to find the split point.
        // Fix: save a footer marker instead. For now, use a simple approach:
        // try all reasonable usn counts (typically 1-26 drives).
        // Actually, let's just try to read from after Rust data.
        // The Rust format has fixed header: magic(8) + header(20) + then variable data.
        // We can calculate Rust data size from the header.
        fs.Seek(8, SeekOrigin.Begin);
        var hdr = br.ReadBytes(20);
        var recordsCount = BitConverter.ToUInt32(hdr, 0);
        var liveCount = BitConverter.ToUInt32(hdr, 4);
        var namePoolLen = BitConverter.ToUInt32(hdr, 8);
        var refKeysLen = BitConverter.ToUInt32(hdr, 12);

        long rustSize = 8 + 20
                        + (long)recordsCount * 40
                        + namePoolLen
                        + (long)refKeysLen * 8
                        + (long)refKeysLen * 4
                        + (long)liveCount * 4 * 3;

        fs.Seek(rustSize, SeekOrigin.Begin);
        var usnCount = br.ReadInt32();
        for (int i = 0; i < usnCount; i++)
        {
            var vol = br.ReadChar();
            var usn = br.ReadUInt64();
            volumeUsns[vol] = usn;
        }

        return live;
    }

    public void Clear()
    {
        _lock.EnterWriteLock();
        try { RustIndexNative.findx_engine_clear(_engine); }
        finally { _lock.ExitWriteLock(); }
    }

    /// <summary>调用方已持有 <see cref="_lock"/> 读锁（或写锁）。</summary>
    private FileEntry? MaterializeCore(int idx)
    {
        if (RustIndexNative.findx_engine_get_live_record(_engine, idx, out var fr, out var pr, out var vol, out var at,
                out var sz, out var mt) != 1)
            return null;

        Span<char> nb = stackalloc char[8192];
        unsafe
        {
            fixed (char* pc = nb)
            {
                int nw = RustIndexNative.findx_engine_get_name_utf16(_engine, idx, (IntPtr)pc, nb.Length);
                if (nw <= 0)
                    return null;
                return new FileEntry
                {
                    FileRef = fr,
                    ParentRef = pr,
                    Name = new string(nb[..nw]),
                    Attributes = at,
                    Size = sz,
                    LastWriteTimeTicks = mt,
                    VolumeLetter = (char)vol,
                    IsDeleted = false,
                    PinyinInitials = "",
                };
            }
        }
    }

    private static int VisitLiveStatic(IntPtr user, int idx)
    {
        var self = (FileIndex)GCHandle.FromIntPtr(user).Target!;
        return self.VisitLiveStep(idx);
    }

    private int VisitLiveStep(int idx)
    {
        var v = _visitFn;
        if (v == null) return 0;
        var e = MaterializeCore(idx);
        if (e == null) return 0;
        return v(e, idx) ? 1 : 0;
    }

    private static int PersistRowStatic(IntPtr user, ulong fileRef, ulong parentRef, IntPtr nameUtf16, int nameLen,
        uint attr, long size, long mtime, ushort vol)
    {
        _ = user;
        var bw = _persistBw ?? throw new InvalidOperationException("findx persist: BinaryWriter 未就绪");
        bw.Write(fileRef);
        bw.Write(parentRef);
        bw.Write(NativeInterop.PtrToString(nameUtf16, nameLen));
        bw.Write(attr);
        bw.Write(size);
        bw.Write(mtime);
        bw.Write((char)vol);
        return 1;
    }
}
