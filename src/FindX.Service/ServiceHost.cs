using System.Diagnostics;
using System.IO;
using System.Threading;
using FindX.Core.FileSystem;
using FindX.Core.Interop;
using FindX.Core.Index;
using FindX.Core.Pinyin;
using FindX.Core.Search;
using FindX.Core.Storage;
using Microsoft.Win32;

namespace FindX.Service;

/// <summary>
/// FindX 服务主机：管理索引生命周期、IPC 服务、系统托盘。
/// 支持开机自启，索引持久化，增量更新。
/// </summary>
public sealed class ServiceHost : IDisposable
{
    private readonly FileIndex _index = new();
    private readonly SearchEngine _searchEngine;
    private readonly VolumeScanner _scanner;
    private readonly JournalWatcher _journalWatcher;
    private readonly FallbackWatcher _fallbackWatcher;
    private IpcServer? _ipcServer;

    private readonly Dictionary<char, ulong> _volumeUsns = new();
    private readonly List<string> _logs = new();

    /// <summary>非 0：正在执行 TryLoadIndex / 全量扫盘；0 表示首轮建立完成。用于 status 与托盘提示。</summary>
    private int _indexBuildInProgress = 1;

    private string IndexPath => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "FindX", "index.dat");

    private string LogPath => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "FindX", "findx.log");

    public ServiceHost()
    {
        _searchEngine = new SearchEngine(_index);
        _scanner = new VolumeScanner(_index);
        _journalWatcher = new JournalWatcher(_index);
        _fallbackWatcher = new FallbackWatcher(_index);

        _scanner.Log += Log;
        _journalWatcher.Log += Log;
        _fallbackWatcher.Log += Log;
    }

    public void Run(string[] args)
    {
        PinyinTable.EnsureInitialized();
        Log("FindX 服务启动中...");
        VolumeAccessHelper.PrepareOnce(Log);

        // 必须先起 IPC：否则加载大索引（数百万条）会阻塞主线程数秒至数十秒，CLI/客户端连接必超时
        StartIpc();

        _ = Task.Run(async () =>
        {
            try
            {
                _index.BeginBulk();
                try
                {
                    TryLoadIndex();
                    await ScanAllVolumesAsync();
                }
                finally
                {
                    _index.EndBulk();
                }
            }
            catch (Exception ex)
            {
                Log($"后台索引任务异常: {ex.Message}");
            }
            finally
            {
                Volatile.Write(ref _indexBuildInProgress, 0);
            }
        });

        if (args.Contains("--no-tray"))
        {
            Log("无托盘模式，按 Ctrl+C 退出");
            var mre = new ManualResetEvent(false);
            Console.CancelKeyPress += (_, e) => { e.Cancel = true; mre.Set(); };
            mre.WaitOne();
        }
        else
        {
            RunWpfTray();
        }

        Shutdown();
    }

    private void StartIpc()
    {
        _ipcServer = new IpcServer(_index, _searchEngine);
        _ipcServer.Log += Log;
        _ipcServer.GetIndexReady = () => Volatile.Read(ref _indexBuildInProgress) == 0;
        _ipcServer.OnReindexRequested = async () =>
        {
            Log("收到重新索引请求");
            Volatile.Write(ref _indexBuildInProgress, 1);
            try
            {
                _index.Clear();
                _index.BeginBulk();
                try
                {
                    await ScanAllVolumesAsync();
                }
                finally
                {
                    _index.EndBulk();
                }
            }
            finally
            {
                Volatile.Write(ref _indexBuildInProgress, 0);
            }
        };
        _ipcServer.Start();
    }

    private async Task ScanAllVolumesAsync()
    {
        var sw = Stopwatch.StartNew();
        Log("开始扫描卷...");

        var drives = DriveInfo.GetDrives()
            .Where(d => d.IsReady && d.DriveType is DriveType.Fixed or DriveType.Removable)
            .ToList();

        foreach (var drive in drives)
        {
            var vol = drive.Name[0];
            try
            {
                var startUsn = _volumeUsns.GetValueOrDefault(vol);
                if (startUsn > 0)
                {
                    Log($"  {vol}: 增量更新 (USN={startUsn})...");
                    _journalWatcher.SetStartUsn(vol, startUsn);
                }
                else
                {
                    var result = await _scanner.ScanVolumeAsync(vol);
                    _volumeUsns[vol] = result.NextUsn;
                    _journalWatcher.SetStartUsn(vol, result.NextUsn);
                }
            }
            catch (Exception ex) { Log($"  {vol}: 扫描失败 - {ex.Message}"); }
        }

        _journalWatcher.Start();
        sw.Stop();

        Log($"索引完成: {_index.Count:N0} 条记录，耗时 {sw.Elapsed.TotalSeconds:F1}s");
        SaveIndex();
    }

    private void TryLoadIndex()
    {
        var snapshot = IndexSerializer.Load(IndexPath);
        if (snapshot == null) { Log("无已有索引"); return; }

        Log($"加载已有索引: {snapshot.Entries.Count:N0} 条...");
        _index.AddBulk(snapshot.Entries);
        foreach (var (vol, usn) in snapshot.VolumeUsns)
            _volumeUsns[vol] = usn;
        Log($"索引加载完成");
    }

    private void SaveIndex()
    {
        try
        {
            IndexSerializer.Save(IndexPath, _index, _volumeUsns);
            Log($"索引已保存到 {IndexPath}");
        }
        catch (Exception ex) { Log($"保存索引失败: {ex.Message}"); }
    }

    private void RunWpfTray()
    {
        var app = new System.Windows.Application();
        app.ShutdownMode = System.Windows.ShutdownMode.OnExplicitShutdown;

        var trayWindow = new TrayWindow(this);
        trayWindow.Show();
        trayWindow.Hide();

        app.Run();
    }

    public void RequestShutdown()
    {
        System.Windows.Application.Current?.Dispatcher.Invoke(() =>
        {
            System.Windows.Application.Current.Shutdown();
        });
    }

    private void Shutdown()
    {
        Log("服务关闭中...");
        _journalWatcher.Stop();
        _fallbackWatcher.Dispose();
        _ipcServer?.Dispose();
        SaveIndex();
        FlushLogs();
    }

    public int IndexCount => _index.CountSnapshot;

    /// <summary>若 true，CLI/托盘应提示「建立中」：全量扫描批量入库时文件数与内存会连续上升，属正常。</summary>
    public bool IndexBuildInProgress => Volatile.Read(ref _indexBuildInProgress) != 0;
    public IReadOnlyList<string> RecentLogs => _logs.TakeLast(50).ToList();

    public void SetAutoStart(bool enable)
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(
                @"Software\Microsoft\Windows\CurrentVersion\Run", true);
            if (enable)
            {
                var exePath = Process.GetCurrentProcess().MainModule?.FileName;
                if (exePath != null)
                    key?.SetValue("FindX", $"\"{exePath}\"");
            }
            else
            {
                key?.DeleteValue("FindX", false);
            }
        }
        catch (Exception ex) { Log($"设置开机自启失败: {ex.Message}"); }
    }

    public bool IsAutoStartEnabled()
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(
                @"Software\Microsoft\Windows\CurrentVersion\Run");
            return key?.GetValue("FindX") != null;
        }
        catch { return false; }
    }

    private void Log(string msg)
    {
        var line = $"[{DateTime.Now:HH:mm:ss}] {msg}";
        lock (_logs) _logs.Add(line);
        Console.WriteLine(line);
    }

    private void FlushLogs()
    {
        try
        {
            var dir = Path.GetDirectoryName(LogPath);
            if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);
            lock (_logs) File.AppendAllLines(LogPath, _logs);
        }
        catch { }
    }

    public void Dispose()
    {
        _journalWatcher.Dispose();
        _fallbackWatcher.Dispose();
        _ipcServer?.Dispose();
    }
}
