using FindX.Core.Index;
using FindX.Core.Interop;

namespace FindX.Core.FileSystem;

/// <summary>
/// USN Journal 增量监控：定期轮询 FSCTL_READ_USN_JOURNAL 获取文件变更，
/// 更新内存索引。支持文件创建、删除、重命名。
/// </summary>
public sealed class JournalWatcher : IDisposable
{
    private readonly FileIndex _index;
    private readonly Dictionary<char, ulong> _volumeUsns = new();
    private CancellationTokenSource? _cts;
    private Task? _watchTask;

    public int PollIntervalMs { get; set; } = 2000;
    public event Action<string>? Log;

    public JournalWatcher(FileIndex index) => _index = index;

    public void SetStartUsn(char vol, ulong usn) => _volumeUsns[vol] = usn;

    public void Start()
    {
        _cts = new CancellationTokenSource();
        _watchTask = Task.Run(() => PollLoop(_cts.Token));
    }

    public void Stop()
    {
        _cts?.Cancel();
        _watchTask?.Wait(3000);
    }

    private async Task PollLoop(CancellationToken ct)
    {
        while (!ct.IsCancellationRequested)
        {
            try
            {
                await Task.Delay(PollIntervalMs, ct);
            }
            catch (OperationCanceledException) { break; }

            foreach (var (vol, startUsn) in _volumeUsns.ToArray())
            {
                if (ct.IsCancellationRequested) break;
                try { PollVolume(vol, startUsn); }
                catch (Exception ex) { Log?.Invoke($"Journal poll {vol}: error: {ex.Message}"); }
            }
        }
    }

    private void PollVolume(char vol, ulong startUsn)
    {
        int created = 0, deleted = 0, renamed = 0;

        FindXJournalCallback callback = (reason, fileRef, parentRef, namePtr, nameLen, attrs) =>
        {
            var name = NativeInterop.PtrToString(namePtr, nameLen);

            // 顺序：重命名先于「创建」判断，避免单条记录同时带多种 Reason 时走错分支
            if ((reason & NativeInterop.USN_REASON_FILE_DELETE) != 0
                && (reason & NativeInterop.USN_REASON_CLOSE) != 0)
            {
                _index.RemoveByRef(vol, fileRef);
                deleted++;
            }
            else if ((reason & NativeInterop.USN_REASON_RENAME_NEW_NAME) != 0
                     && (reason & NativeInterop.USN_REASON_CLOSE) != 0)
            {
                _index.UpsertEntry(new FileEntry
                {
                    FileRef = fileRef,
                    ParentRef = parentRef,
                    Name = name,
                    Attributes = attrs,
                    VolumeLetter = vol,
                });
                renamed++;
            }
            // CREATE 与 CLOSE 常拆成两条 USN 记录，不能只认 CREATE|CLOSE 同现
            else if ((reason & NativeInterop.USN_REASON_FILE_CREATE) != 0)
            {
                _index.UpsertEntry(new FileEntry
                {
                    FileRef = fileRef,
                    ParentRef = parentRef,
                    Name = name,
                    Attributes = attrs,
                    VolumeLetter = vol,
                });
                created++;
            }
        };

        try
        {
            int rc = NativeInterop.FindX_ReadJournal((ushort)vol, startUsn, callback, out var nextUsn);
            if (rc >= 0)
                _volumeUsns[vol] = nextUsn;
            GC.KeepAlive(callback);

            if (created + deleted + renamed > 0)
                Log?.Invoke($"Journal {vol}: +{created} -{deleted} ~{renamed}");
        }
        catch (DllNotFoundException) { }
    }

    public void Dispose()
    {
        Stop();
        _cts?.Dispose();
    }
}
