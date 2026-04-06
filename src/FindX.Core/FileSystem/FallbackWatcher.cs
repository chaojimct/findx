using FindX.Core.Index;

namespace FindX.Core.FileSystem;

/// <summary>
/// 基于 FileSystemWatcher (ReadDirectoryChangesW) 的回退监控。
/// 用于非 NTFS 卷（FAT32、exFAT、网络驱动器等）。
/// </summary>
public sealed class FallbackWatcher : IDisposable
{
    private readonly FileIndex _index;
    private readonly List<FileSystemWatcher> _watchers = new();

    public event Action<string>? Log;

    public FallbackWatcher(FileIndex index) => _index = index;

    public void Watch(string path)
    {
        if (!Directory.Exists(path)) return;

        var fsw = new FileSystemWatcher(path)
        {
            IncludeSubdirectories = true,
            NotifyFilter = NotifyFilters.FileName | NotifyFilters.DirectoryName
                         | NotifyFilters.LastWrite | NotifyFilters.Size,
            EnableRaisingEvents = true,
        };

        fsw.Created += (_, e) => OnCreated(e.FullPath);
        fsw.Deleted += (_, e) => OnDeleted(e.FullPath);
        fsw.Renamed += (_, e) => OnRenamed(e.OldFullPath, e.FullPath);
        fsw.Error += (_, e) => Log?.Invoke($"FallbackWatcher error: {e.GetException().Message}");

        _watchers.Add(fsw);
        Log?.Invoke($"FallbackWatcher: monitoring {path}");
    }

    private void OnCreated(string fullPath)
    {
        try
        {
            var name = Path.GetFileName(fullPath);
            var parentDir = Path.GetDirectoryName(fullPath) ?? "";
            var vol = fullPath.Length >= 2 ? fullPath[0] : '?';
            var entry = new FileEntry
            {
                FileRef = (ulong)fullPath.GetHashCode(StringComparison.OrdinalIgnoreCase),
                ParentRef = (ulong)parentDir.GetHashCode(StringComparison.OrdinalIgnoreCase),
                Name = name,
                Attributes = Directory.Exists(fullPath) ? 0x10u : 0u,
                VolumeLetter = vol,
            };
            _index.AddEntry(entry);
        }
        catch { }
    }

    private void OnDeleted(string fullPath)
    {
        var vol = fullPath.Length >= 2 ? fullPath[0] : '?';
        var fakeRef = (ulong)fullPath.GetHashCode(StringComparison.OrdinalIgnoreCase);
        _index.RemoveByRef(vol, fakeRef);
    }

    private void OnRenamed(string oldPath, string newPath)
    {
        OnDeleted(oldPath);
        OnCreated(newPath);
    }

    public void Dispose()
    {
        foreach (var w in _watchers)
        {
            w.EnableRaisingEvents = false;
            w.Dispose();
        }
        _watchers.Clear();
    }
}
