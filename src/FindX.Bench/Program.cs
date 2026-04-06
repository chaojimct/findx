using System.Diagnostics;
using System.Runtime;
using FindX.Core.Index;
using FindX.Core.Pinyin;
using FindX.Core.Search;

namespace FindX.Bench;

/// <summary>
/// 内存索引基准：纯托管驱动 + Rust findx_engine，不依赖 MFT/USN。
/// 用法: FindX.Bench [entryCount] [可选: 跳过高成本搜索轮询 1]
/// </summary>
internal static class Program
{
    private static void Main(string[] args)
    {
        int count = 120_000;
        if (args.Length > 0 && int.TryParse(args[0], out var c) && c > 0)
            count = Math.Min(c, 2_000_000);

        bool skipHeavyPoll = args.Contains("--quick", StringComparer.OrdinalIgnoreCase)
                             || (args.Length > 1 && args[1] == "1");

        PinyinTable.EnsureInitialized();
        GCSettings.LargeObjectHeapCompactionMode = GCLargeObjectHeapCompactionMode.CompactOnce;
        GC.Collect(GC.MaxGeneration, GCCollectionMode.Forced, true, true);
        GC.WaitForPendingFinalizers();

        var proc = Process.GetCurrentProcess();
        proc.Refresh();
        var ws0 = proc.WorkingSet64;

        Console.WriteLine($"FindX.Bench  synthetic entries={count:N0}  PID={proc.Id}");

        var index = new FileIndex();
        var swBulk = Stopwatch.StartNew();
        index.BeginBulk();
        BuildSyntheticIndex(index, count);
        index.EndBulk();
        swBulk.Stop();

        proc.Refresh();
        var ws1 = proc.WorkingSet64;

        Console.WriteLine($"  批量入库 + EndBulk(排序/拼音): {swBulk.Elapsed.TotalSeconds:F2}s");
        Console.WriteLine($"  工作集: 前 {ws0 / 1024.0 / 1024:F1} MB → 后 {ws1 / 1024.0 / 1024:F1} MB (Δ {(ws1 - ws0) / 1024.0 / 1024:F1} MB)");
        Console.WriteLine($"  活条目数: {index.Count:N0}");

        var engine = new SearchEngine(index);
        RunSearch(engine, "bench_a0", 20);
        RunSearch(engine, "bench_ff", 50);
        RunSearch(engine, "000100", 20);

        if (!skipHeavyPoll)
        {
            Console.WriteLine("  轮询 status 风格（SearchNamePrefix 冷/热）10 次 …");
            var swP = Stopwatch.StartNew();
            for (int i = 0; i < 10; i++)
                _ = index.SearchNamePrefix("bench_a0", 30);
            swP.Stop();
            Console.WriteLine($"    10×SearchNamePrefix 总耗时: {swP.Elapsed.TotalMilliseconds:F1}ms (均 {swP.Elapsed.TotalMilliseconds / 10:F2}ms)");
        }

        Console.WriteLine("完成。");
    }

    private static void BuildSyntheticIndex(FileIndex index, int count)
    {
        // 模拟多文件名字分布：前缀 + 数字，便于前缀检索
        const int chunk = 8192;
        var batch = new List<FileEntry>(chunk);
        for (int i = 0; i < count; i++)
        {
            batch.Add(new FileEntry
            {
                VolumeLetter = 'C',
                FileRef = (ulong)(i + 1),
                ParentRef = 0x1000UL,
                Name = $"bench_{i % 256:x2}_{(uint)i:x6}.txt",
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
        {
            index.AddBulk(batch);
            batch.Clear();
        }
    }

    private static void RunSearch(SearchEngine engine, string query, int max)
    {
        var sw = Stopwatch.StartNew();
        var r = engine.Search(query, max);
        sw.Stop();
        Console.WriteLine(
            $"  Search \"{query}\" max={max} → {r.Count} 条, {sw.Elapsed.TotalMilliseconds:F2}ms (引擎上报约路径评分逻辑)");
    }
}
