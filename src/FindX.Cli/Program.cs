using System.Diagnostics;
using FindX.Client;
using FindX.Core.Index;
using FindX.Core.Search;
using FindX.Core.Storage;
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
        if (command is "test" or "t")
            return RunLocalSmokeTests(args);

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

        var wall = Stopwatch.StartNew();
        var result = await client.SearchAsync(query, maxResults, pathFilter);
        wall.Stop();
        if (result == null)
        {
            var detail = string.IsNullOrEmpty(client.LastError) ? "" : $": {client.LastError}";
            Console.Error.WriteLine($"搜索失败{detail}");
            return 1;
        }

        Console.WriteLine(
            $"找到 {result.TotalCount} 个结果 (服务端 Search {result.ElapsedMs:F1} ms, 含 IPC/JSON 往返 {wall.Elapsed.TotalMilliseconds:F1} ms)");
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
        Console.WriteLine("    注意: search 走本机 FindX 命名管道服务；索引/Rust 优化需将 findx_engine.dll 与 FindX.exe 同目录部署并重启服务后生效。");
        Console.WriteLine("  status                                       查看索引状态");
        Console.WriteLine("  reindex                                      触发重新索引");
        Console.WriteLine("  update [--install|-i]                         检查更新；加 --install 则下载并启动安装");
        Console.WriteLine("  test [--index <path>]                         本进程加载 index.dat，测 Core+Rust 搜索耗时（与 search 是否重启服务无关）");
        Console.WriteLine("  test --synthetic [--quick] [N]               内存合成索引（无安装索引时用）");
        return 1;
    }

    private static string DefaultInstalledIndexPath()
        => Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "FindX",
            "index.dat");

    /// <summary>
    /// 默认加载 FindX 与本机服务相同的 index.dat；<c>--synthetic</c> 时使用内存合成索引。
    /// </summary>
    private static int RunLocalSmokeTests(string[] args)
    {
        if (args.Contains("--synthetic", StringComparer.OrdinalIgnoreCase))
            return RunSyntheticSmokeTests(args);

        string? indexPath = null;
        for (var i = 1; i < args.Length; i++)
        {
            if (args[i].Equals("--index", StringComparison.OrdinalIgnoreCase) && i + 1 < args.Length)
            {
                indexPath = args[++i];
                continue;
            }

            if (args[i].Equals("--quick", StringComparison.OrdinalIgnoreCase)
                || args[i].Equals("--synthetic", StringComparison.OrdinalIgnoreCase))
                continue;

            if (!args[i].StartsWith('-') && File.Exists(args[i]))
                indexPath = args[i];
        }

        indexPath ??= DefaultInstalledIndexPath();

        if (!File.Exists(indexPath))
        {
            Console.Error.WriteLine($"fx test: 索引文件不存在: {indexPath}");
            Console.Error.WriteLine("  请先运行 FindX 完成索引，或指定: fx test --index <其它 index.dat 路径>");
            Console.Error.WriteLine("  开发机无安装索引时可用: fx test --synthetic [--quick] [N]");
            return 1;
        }

        Console.WriteLine($"fx test: 加载索引 {indexPath}");

        var index = new FileIndex();
        var usns = new Dictionary<char, ulong>();
        var swLoad = Stopwatch.StartNew();
        var loaded = IndexSerializer.TryLoadBinary(indexPath, index, usns);
        swLoad.Stop();

        if (loaded < 0)
        {
            Console.Error.WriteLine("fx test: 加载失败（非 FXBIN 或文件损坏）。");
            return 1;
        }

        if (index.Count == 0)
        {
            Console.Error.WriteLine("fx test: 索引条目数为 0。");
            return 1;
        }

        Console.WriteLine($"  加载: {swLoad.Elapsed.TotalMilliseconds:F1} ms, live={index.Count:N0}");

        var engine = new SearchEngine(index);

        static void BenchQuery(SearchEngine eng, string q, int max)
        {
            var sw = Stopwatch.StartNew();
            var r = eng.Search(q, max);
            sw.Stop();
            Console.WriteLine($"  Search \"{q}\" max={max} -> {r.Count} 条, {sw.Elapsed.TotalMilliseconds:F2} ms");
        }

        BenchQuery(engine, "yuebao", 200);
        BenchQuery(engine, "ybhz", 200);
        BenchQuery(engine, "bao", 200);
        BenchQuery(engine, "windows", 20);
        BenchQuery(engine, "clipboard", 20);
        BenchQuery(engine, "clip", 20);

        Console.WriteLine("fx test: 本机索引检查完成（未对命中内容做强断言；回归请 dotnet test src/FindX.Tests）");
        return 0;
    }

    /// <summary>内存合成索引 + 固定断言（仅 <c>--synthetic</c>）。</summary>
    private static int RunSyntheticSmokeTests(string[] args)
    {
        int count = 120_000;
        var quick = args.Contains("--quick", StringComparer.OrdinalIgnoreCase);
        if (quick)
            count = 25_000;

        foreach (var a in args)
        {
            if (a.StartsWith('-')) continue;
            if (int.TryParse(a, out var n) && n > 0)
                count = Math.Min(n, 2_000_000);
        }

        Console.WriteLine($"fx test --synthetic: 合成索引条目数={count:N0} …");

        var index = new FileIndex();
        var swBulk = Stopwatch.StartNew();
        index.BeginBulk();
        const int chunk = 8192;
        var batch = new List<FileEntry>(chunk);
        for (int i = 0; i < count; i++)
        {
            var month = i % 12 + 1;
            var name = i % 2048 == 0
                ? $"【彩石智能月报】马春天+{month}月.docx"
                : $"bench_{i % 256:x2}_{(uint)i:x6}.txt";
            batch.Add(new FileEntry
            {
                VolumeLetter = 'C',
                FileRef = (ulong)(i + 1),
                ParentRef = 0x1000UL,
                Name = name,
                Attributes = (i & 1) == 0 ? 0u : 0x10u,
                Size = i,
                LastWriteTimeTicks = i,
            });
            if (batch.Count >= chunk)
            {
                index.AddBulk(batch);
                batch.Clear();
            }
        }

        if (batch.Count > 0)
            index.AddBulk(batch);
        index.EndBulk();
        index.AddEntry(new FileEntry
        {
            VolumeLetter = 'C',
            FileRef = (ulong)count + 10_000_000UL,
            ParentRef = 0x1000UL,
            Name = "月报汇总.md",
            Attributes = 0x20,
            Size = 1,
            LastWriteTimeTicks = 0,
        });
        swBulk.Stop();
        Console.WriteLine($"  EndBulk+月报汇总: {swBulk.Elapsed.TotalMilliseconds:F1} ms");

        var engine = new SearchEngine(index);

        var failed = false;

        var sw1 = Stopwatch.StartNew();
        var ry = engine.Search("yuebao", 50);
        sw1.Stop();
        var ms1 = sw1.Elapsed.TotalMilliseconds;
        if (!ry.Exists(x => x.Name.Contains("月报", StringComparison.Ordinal)))
        {
            Console.Error.WriteLine("  失败: yuebao 应命中月报名称");
            failed = true;
        }
        else
            Console.WriteLine($"  yuebao: {ms1:F2} ms, 命中 {ry.Count} 条");

        var sw2 = Stopwatch.StartNew();
        var rb = engine.Search("ybhz", 50);
        sw2.Stop();
        var ms2 = sw2.Elapsed.TotalMilliseconds;
        if (rb.Count == 0 || !rb.Exists(x => x.Name.Contains("月报汇总", StringComparison.Ordinal)))
        {
            Console.Error.WriteLine("  失败: ybhz 应命中 月报汇总.md（首字母子串）");
            failed = true;
        }
        else
            Console.WriteLine($"  ybhz: {ms2:F2} ms, 命中 {rb.Count} 条");

        var sw3 = Stopwatch.StartNew();
        var rbao = engine.Search("bao", 50);
        sw3.Stop();
        var ms3 = sw3.Elapsed.TotalMilliseconds;
        if (rbao.Count == 0)
        {
            Console.Error.WriteLine("  失败: bao 应有结果");
            failed = true;
        }
        else
            Console.WriteLine($"  bao: {ms3:F2} ms, 命中 {rbao.Count} 条");

        if (failed)
        {
            Console.Error.WriteLine("fx test --synthetic: 未通过");
            return 1;
        }

        Console.WriteLine($"fx test --synthetic: 全部通过（yuebao {ms1:F1} ms / ybhz {ms2:F1} ms / bao {ms3:F1} ms）");
        Console.WriteLine("完整单元测试请执行: dotnet test src/FindX.Tests");
        return 0;
    }

    private static string FormatSize(long bytes)
    {
        if (bytes < 1024) return $"{bytes} B";
        if (bytes < 1024 * 1024) return $"{bytes / 1024.0:F1} KB";
        if (bytes < 1024L * 1024 * 1024) return $"{bytes / 1024.0 / 1024:F1} MB";
        return $"{bytes / 1024.0 / 1024 / 1024:F2} GB";
    }
}
