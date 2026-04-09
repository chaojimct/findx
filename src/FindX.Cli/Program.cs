using System.Diagnostics;
using FindX.Client;
using FindX.Core.Update;

namespace FindX.Cli;

public static class Program
{
    public static async Task<int> Main(string[] args)
    {
        if (args.Length == 0)
        {
            PrintUsage();
            return 1;
        }

        var command = args[0].ToLowerInvariant();
        using var client = new FindXClient();

        try
        {
            return command switch
            {
                "search" or "s" => await RunSearch(client, args),
                "status" => await RunStatus(client),
                "reindex" => await RunReindex(client),
                "update" => await RunUpdate(args),
                _ => PrintUsage(),
            };
        }
        catch (TimeoutException)
        {
            Console.Error.WriteLine("错误: 无法连接到 FindX 服务，请确认服务已启动。");
            return 2;
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"错误: {ex.Message}");
            return 3;
        }
    }

    private static async Task<int> RunSearch(FindXClient client, string[] args)
    {
        if (args.Length < 2)
        {
            Console.Error.WriteLine("用法: fx search <query> [--max N] [--path <filter>] [--json]");
            return 1;
        }

        var query = args[1];
        int maxResults = 20;
        string? pathFilter = null;
        var jsonOut = false;

        for (int i = 2; i < args.Length; i++)
        {
            if (args[i] == "--max" && i + 1 < args.Length)
                int.TryParse(args[++i], out maxResults);
            else if (args[i] == "--path" && i + 1 < args.Length)
                pathFilter = args[++i];
            else if (args[i] == "--json")
                jsonOut = true;
        }

        var result = await client.SearchAsync(query, maxResults, pathFilter);
        if (result == null)
        {
            var detail = string.IsNullOrEmpty(client.LastError) ? "" : $": {client.LastError}";
            Console.Error.WriteLine($"搜索失败{detail}");
            return 1;
        }

        Console.WriteLine($"找到 {result.TotalCount} 个结果 (耗时 {result.ElapsedMs:F1}ms)");
        Console.WriteLine();

        if (jsonOut)
        {
            foreach (var item in result.Items)
            {
                Console.WriteLine(
                    System.Text.Json.JsonSerializer.Serialize(new
                    {
                        path = item.Path,
                        name = item.Name,
                        isDir = item.IsDir,
                        size = item.Size,
                        lastWriteUtcTicks = item.LastWriteUtcTicks,
                        score = item.Score,
                    }));
            }

            return 0;
        }

        int idx = 1;
        foreach (var item in result.Items)
        {
            var icon = item.IsDir ? "[D]" : "[F]";
            Console.WriteLine($"  {idx,2}. {icon} {item.Name}");
            Console.WriteLine($"      {item.Path}");
            var sizePart = item.IsDir ? "" : $"大小 {FormatSize(item.Size)}  ";
            var timePart = item.LastWriteUtcTicks > 0
                ? $"修改 {new DateTime(item.LastWriteUtcTicks, DateTimeKind.Utc).ToLocalTime():yyyy-MM-dd HH:mm:ss}  "
                : "修改 (索引无/未知)  ";
            Console.WriteLine($"      {sizePart}{timePart}Score={item.Score}");
            idx++;
        }

        return 0;
    }

    private static async Task<int> RunStatus(FindXClient client)
    {
        var status = await client.GetStatusAsync();
        if (status == null)
        {
            var detail = string.IsNullOrEmpty(client.LastError) ? "" : $": {client.LastError}";
            Console.Error.WriteLine($"获取状态失败{detail}");
            Console.Error.WriteLine("提示: 请确认已启动与本 fx 同版本编出的 FindX.exe（命名管道 \\\\.\\pipe\\FindX），且勿与其它程序占用同名管道。");
            return 1;
        }

        Console.WriteLine("FindX 索引状态:");
        if (!status.IndexReady)
            Console.WriteLine("  状态: 正在建立索引（全量扫描中；文件数量与内存持续上涨为正常，完成后趋于稳定）");
        else
            Console.WriteLine("  状态: 就绪（增量监控已运行）");
        Console.WriteLine($"  文件数量: {status.FileCount:N0}");
        Console.WriteLine($"  内存占用: {status.MemoryMb:F1} MB");
        return 0;
    }

    private static async Task<int> RunReindex(FindXClient client)
    {
        Console.WriteLine("正在请求重新索引...");
        var ok = await client.ReindexAsync();
        Console.WriteLine(ok ? "重新索引已触发" : "请求失败");
        return ok ? 0 : 1;
    }

    private static async Task<int> RunUpdate(string[] args)
    {
        var apply = args.Any(a => string.Equals(a, "--install", StringComparison.OrdinalIgnoreCase)
                                  || string.Equals(a, "-i", StringComparison.OrdinalIgnoreCase));

        Console.WriteLine($"当前版本: v{UpdateChecker.GetCurrentVersion()}");
        Console.WriteLine("正在检查更新...");

        using var checker = new UpdateChecker();
        var info = await checker.CheckAsync();

        if (info == null)
        {
            Console.Error.WriteLine("检查更新失败，请检查网络连接。");
            return 1;
        }

        if (!info.HasUpdate)
        {
            Console.WriteLine($"当前已是最新版本 (v{info.CurrentVersion})。");
            return 0;
        }

        Console.WriteLine($"发现新版本: v{info.LatestVersion}");
        if (info.PublishedAt.HasValue)
            Console.WriteLine($"发布时间: {info.PublishedAt.Value:yyyy-MM-dd HH:mm}");
        if (!string.IsNullOrWhiteSpace(info.ReleaseNotes))
        {
            Console.WriteLine();
            Console.WriteLine("更新说明:");
            var notes = info.ReleaseNotes.Length > 500
                ? info.ReleaseNotes[..500] + "..."
                : info.ReleaseNotes;
            Console.WriteLine(notes);
        }
        Console.WriteLine();
        if (!string.IsNullOrEmpty(info.DownloadUrl))
            Console.WriteLine($"下载地址: {info.DownloadUrl}");
        if (!string.IsNullOrEmpty(info.ReleaseUrl))
            Console.WriteLine($"发布页面: {info.ReleaseUrl}");

        if (!apply)
            return 0;

        if (string.IsNullOrEmpty(info.DownloadUrl))
        {
            Console.Error.WriteLine("错误: 发布中未包含 setup 安装包，请到发布页手动下载。");
            return 1;
        }

        Console.WriteLine();
        Console.WriteLine("正在下载安装包…");
        try
        {
            var path = await UpdateInstaller.DownloadInstallerAsync(info.DownloadUrl, info.LatestVersion, null);
            Console.WriteLine($"已保存: {path}");
            Console.WriteLine("正在启动安装向导（图形界面，可能需要 UAC）…");
            UpdateInstaller.LaunchInstaller(path);
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"下载或启动失败: {ex.Message}");
            return 1;
        }

        return 0;
    }

    private static int PrintUsage()
    {
        Console.WriteLine("FindX — 高性能文件搜索引擎 CLI");
        Console.WriteLine();
        Console.WriteLine("用法: fx <command> [options]");
        Console.WriteLine();
        Console.WriteLine("命令:");
        Console.WriteLine("  search <query> [--max N] [--path <filter>] [--json]  搜索（每行打印大小、修改时间、--json 为 NDJSON）");
        Console.WriteLine("  status                                       查看索引状态");
        Console.WriteLine("  reindex                                      触发重新索引");
        Console.WriteLine("  update [--install|-i]                         检查更新；加 --install 则下载并启动安装");
        return 1;
    }

    private static string FormatSize(long bytes)
    {
        if (bytes < 1024) return $"{bytes} B";
        if (bytes < 1024 * 1024) return $"{bytes / 1024.0:F1} KB";
        if (bytes < 1024L * 1024 * 1024) return $"{bytes / 1024.0 / 1024:F1} MB";
        return $"{bytes / 1024.0 / 1024 / 1024:F2} GB";
    }
}
