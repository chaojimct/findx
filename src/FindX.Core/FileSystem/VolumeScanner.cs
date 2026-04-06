using System.Runtime.InteropServices;
using FindX.Core.Index;
using FindX.Core.Interop;
using FindX.Core.Pinyin;

namespace FindX.Core.FileSystem;

/// <summary>
/// 基于 Native DLL 的 NTFS 卷全量扫描器。
/// 通过 FSCTL_ENUM_USN_DATA 遍历 MFT，构建完整文件索引。
/// 非 NTFS 卷使用 Directory.EnumerateFileSystemEntries 回退。
/// </summary>
public sealed class VolumeScanner
{
    private readonly FileIndex _index;

    public VolumeScanner(FileIndex index)
    {
        _index = index;
    }

    public event Action<string>? Log;

    public record ScanResult(int TotalEntries, ulong NextUsn, TimeSpan Elapsed, bool UsedNative);

    public async Task<ScanResult> ScanVolumeAsync(char driveLetter, CancellationToken ct = default)
    {
        PinyinTable.EnsureInitialized();
        var sw = System.Diagnostics.Stopwatch.StartNew();

        try
        {
            return await Task.Run(() => ScanViaNative(driveLetter, ct), ct);
        }
        catch (DllNotFoundException)
        {
            Log?.Invoke($"FindXNative.dll 未找到，使用回退扫描 {driveLetter}:");
            return await Task.Run(() => ScanFallback(driveLetter, sw, ct), ct);
        }
        catch (EntryPointNotFoundException)
        {
            Log?.Invoke($"FindXNative.dll 入口点缺失，使用回退扫描 {driveLetter}:");
            return await Task.Run(() => ScanFallback(driveLetter, sw, ct), ct);
        }
    }

    private ScanResult ScanViaNative(char driveLetter, CancellationToken ct)
    {
        var sw = System.Diagnostics.Stopwatch.StartNew();
        var batch = new List<FileEntry>(8192);
        int total = 0;

        FindXEnumCallback callback = (fileRef, parentRef, namePtr, nameLen, attrs, size, lastWrite) =>
        {
            var name = NativeInterop.PtrToString(namePtr, nameLen);
            if (string.IsNullOrEmpty(name)) return;

            var entry = new FileEntry
            {
                FileRef = fileRef,
                ParentRef = parentRef,
                Name = name,
                Attributes = attrs,
                Size = (long)size,
                LastWriteTimeTicks = lastWrite,
                VolumeLetter = driveLetter,
            };
            batch.Add(entry);

            if (batch.Count >= 8192)
            {
                _index.AddBulk(batch);
                total += batch.Count;
                batch.Clear();
                Log?.Invoke($"  {driveLetter}: 已索引 {total:N0} 条...");
            }
        };

        var result = NativeInterop.FindX_EnumVolume((ushort)driveLetter, callback, out var nextUsn);

        if (batch.Count > 0)
        {
            _index.AddBulk(batch);
            total += batch.Count;
        }

        sw.Stop();
        GC.KeepAlive(callback);

        if (result < 0)
        {
            var lastErr = (uint)Marshal.GetLastWin32Error();
            var lastMsg = Win32Message.Format(lastErr);
            var diag = NativeInterop.FindX_DiagnoseVolume((ushort)driveLetter, out var openErr, out var journalErr);
            var openMsg = Win32Message.Format(openErr);
            var jourMsg = Win32Message.Format(journalErr);
            Log?.Invoke($"  {driveLetter}: FindX_EnumVolume 失败 result={result} GetLastWin32Error={lastErr} {lastMsg}");
            Log?.Invoke($"  {driveLetter}: FindX_DiagnoseVolume={diag} OpenErr={openErr} {openMsg} | JournalErr={journalErr} {jourMsg}");
            if (lastErr == 5 || openErr == 5)
                Log?.Invoke($"  {driveLetter}: 提示: 拒绝访问时可右键「以管理员身份运行」FindX，或检查组策略/防病毒对卷设备的限制。");
            Log?.Invoke($"  {driveLetter}: 将回退目录扫描（设置环境变量 FINDX_NO_FALLBACK=1 可禁止回退，仅用于排查）");
            if (!string.Equals(Environment.GetEnvironmentVariable("FINDX_NO_FALLBACK"), "1", StringComparison.Ordinal))
                return ScanFallback(driveLetter, sw, ct);
            return new ScanResult(total, 0, sw.Elapsed, false);
        }

        Log?.Invoke($"  {driveLetter}: 扫描完成 {total:N0} 条，耗时 {sw.Elapsed.TotalSeconds:F1}s");
        return new ScanResult(total, nextUsn, sw.Elapsed, true);
    }

    private ScanResult ScanFallback(char driveLetter, System.Diagnostics.Stopwatch sw, CancellationToken ct)
    {
        var root = $"{driveLetter}:\\";
        if (!Directory.Exists(root))
            return new ScanResult(0, 0, sw.Elapsed, false);

        var batch = new List<FileEntry>(4096);
        int total = 0;
        ulong fakeRef = 1;

        try
        {
            foreach (var path in Directory.EnumerateFileSystemEntries(root, "*", new EnumerationOptions
            {
                RecurseSubdirectories = true,
                IgnoreInaccessible = true,
                AttributesToSkip = 0,
            }))
            {
                ct.ThrowIfCancellationRequested();

                try
                {
                    var info = new FileInfo(path);
                    var parentDir = Path.GetDirectoryName(path) ?? root;
                    var parentHash = (ulong)parentDir.GetHashCode(StringComparison.OrdinalIgnoreCase);

                    var entry = new FileEntry
                    {
                        FileRef = fakeRef++,
                        ParentRef = parentHash,
                        Name = Path.GetFileName(path),
                        Attributes = (uint)(info.Attributes & (FileAttributes)0xFFFF),
                        Size = info.Exists ? info.Length : 0,
                        LastWriteTimeTicks = info.LastWriteTimeUtc.Ticks,
                        VolumeLetter = driveLetter,
                    };
                    batch.Add(entry);

                    if (batch.Count >= 4096)
                    {
                        _index.AddBulk(batch);
                        total += batch.Count;
                        batch.Clear();
                    }
                }
                catch { }
            }
        }
        catch (OperationCanceledException) { throw; }
        catch { }

        if (batch.Count > 0)
        {
            _index.AddBulk(batch);
            total += batch.Count;
        }

        sw.Stop();
        Log?.Invoke($"  {driveLetter}: 回退扫描完成 {total:N0} 条，耗时 {sw.Elapsed.TotalSeconds:F1}s");
        return new ScanResult(total, 0, sw.Elapsed, false);
    }
}
