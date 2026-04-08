using System.Diagnostics;
using System.IO;
using System.Threading;
using FindX.Core.FileSystem;
using FindX.Core.Interop;
using FindX.Core.Index;
using FindX.Core.Pinyin;
using FindX.Core.Search;
using FindX.Core.Storage;
using FindX.Core.Update;
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
    private EverythingIpcServer? _everythingIpc;

    private readonly Dictionary<char, ulong> _volumeUsns = new();
    private readonly List<string> _logs = new();
    private readonly UpdateChecker _updateChecker = new();
    private UpdateInfo? _latestUpdateInfo;

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
            var swTotal = Stopwatch.StartNew();
            try
            {
                var swLoad = Stopwatch.StartNew();
                bool loaded = TryLoadBinaryIndex();
                swLoad.Stop();

                if (loaded)
                {
                    Log($"二进制索引加载耗时: {swLoad.Elapsed.TotalSeconds:F2}s");
                    var swScan = Stopwatch.StartNew();
                    await ScanAllVolumesAsync(skipSave: true);
                    swScan.Stop();
                    Log($"ScanAllVolumes 耗时: {swScan.Elapsed.TotalSeconds:F2}s");
                }
                else
                {
                    _index.BeginBulk();
                    try
                    {
                        TryLoadLegacyIndex();
                        swLoad.Stop();
                        Log($"旧格式索引加载耗时: {swLoad.Elapsed.TotalSeconds:F2}s");

                        var swScan = Stopwatch.StartNew();
                        await ScanAllVolumesAsync(skipSave: true);
                        swScan.Stop();
                        Log($"ScanAllVolumes 耗时: {swScan.Elapsed.TotalSeconds:F2}s");
                    }
                    finally
                    {
                        var swRebuild = Stopwatch.StartNew();
                        _index.EndBulk();
                        swRebuild.Stop();
                        Log($"EndBulk(rebuild_indexes) 耗时: {swRebuild.Elapsed.TotalSeconds:F2}s");
                    }
                }

                SaveIndex();
            }
            catch (Exception ex)
            {
                Log($"后台索引任务异常: {ex.Message}");
            }
            finally
            {
                swTotal.Stop();
                Log($"索引就绪总耗时: {swTotal.Elapsed.TotalSeconds:F2}s");
                Volatile.Write(ref _indexBuildInProgress, 0);

                _ = CheckForUpdateAsync();
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
                    await ScanAllVolumesAsync(skipSave: true);
                }
                finally
                {
                    _index.EndBulk();
                }
                SaveIndex();
            }
            finally
            {
                Volatile.Write(ref _indexBuildInProgress, 0);
            }
        };
        _ipcServer.Start();

        _everythingIpc = new EverythingIpcServer(_index, _searchEngine);
        _everythingIpc.Log += Log;
        _everythingIpc.GetIndexReady = () => Volatile.Read(ref _indexBuildInProgress) == 0;
        _everythingIpc.Start();
    }

    private async Task ScanAllVolumesAsync(bool skipSave = false)
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
        if (!skipSave) SaveIndex();
    }

    /// <summary>尝试加载 FXBIN02 快速二进制格式（无需 rebuild）。</summary>
    private bool TryLoadBinaryIndex()
    {
        try
        {
            var loaded = IndexSerializer.TryLoadBinary(IndexPath, _index, _volumeUsns);
            if (loaded >= 0)
            {
                Log($"二进制索引加载完成: {loaded:N0} 条");
                return true;
            }
        }
        catch (Exception ex)
        {
            Log($"二进制索引加载异常: {ex.Message}");
        }
        return false;
    }

    /// <summary>加载旧 FINDX01 格式（需要在 bulk mode 下调用）。</summary>
    private void TryLoadLegacyIndex()
    {
        try
        {
            var loaded = IndexSerializer.LoadStreaming(IndexPath, _index, _volumeUsns);
            if (loaded < 0) { Log("无已有索引"); return; }
            Log($"旧格式索引加载完成: {loaded:N0} 条");
        }
        catch (Exception ex)
        {
            Log($"旧格式索引加载异常: {ex.Message}");
        }
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
        _everythingIpc?.Dispose();
        _journalWatcher.Stop();
        _fallbackWatcher.Dispose();
        _ipcServer?.Dispose();
        SaveIndex();
        FlushLogs();
    }

    public List<SearchResult> Search(string query, int maxResults = 100)
        => _searchEngine.Search(query, maxResults);

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

    public UpdateInfo? LatestUpdateInfo => _latestUpdateInfo;

    public async Task<UpdateInfo?> CheckForUpdateAsync()
    {
        try
        {
            var info = await _updateChecker.CheckAsync();
            _latestUpdateInfo = info;
            if (info?.HasUpdate == true)
                Log($"发现新版本: v{info.LatestVersion}（当前: v{info.CurrentVersion}）");
            return info;
        }
        catch (Exception ex)
        {
            Log($"检查更新失败: {ex.Message}");
            return null;
        }
    }

    /// <summary>使用最近一次检查到的 <see cref="UpdateInfo.DownloadUrl"/> 下载 setup 并启动静默安装，随后请求退出 UI 进程。</summary>
    public async Task<(bool Ok, string? Error)> TryDownloadAndApplyUpdateAsync(CancellationToken ct = default)
    {
        var info = _latestUpdateInfo;
        if (info?.HasUpdate != true)
            return (false, "当前没有可用的新版本信息，请先点击「检查更新」。");
        if (string.IsNullOrEmpty(info.DownloadUrl))
            return (false, "发布页未包含 setup.exe 资源，请使用「发布页」手动下载。");

        try
        {
            Log($"开始下载 v{info.LatestVersion} 安装包…");
            var path = await UpdateInstaller.DownloadInstallerAsync(info.DownloadUrl, info.LatestVersion, null, ct);
            Log($"安装包已保存: {path}");
            UpdateInstaller.LaunchInstaller(path);
            Log("已启动安装程序，将退出 FindX 以完成更新…");
            RequestShutdown();
            return (true, null);
        }
        catch (OperationCanceledException)
        {
            return (false, "已取消下载。");
        }
        catch (Exception ex)
        {
            Log($"下载或启动安装失败: {ex.Message}");
            return (false, ex.Message);
        }
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
        _everythingIpc?.Dispose();
        _journalWatcher.Dispose();
        _fallbackWatcher.Dispose();
        _ipcServer?.Dispose();
        _updateChecker.Dispose();
    }
}
