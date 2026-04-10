using System.Diagnostics;
using System.Runtime;
using FindX.Core.Index;
using FindX.Core.Search;
using FindX.Core.Storage;

namespace FindX.Bench;

internal static class Program
{
    private static void Main(string[] args)
    {
        string? indexPath = null;
        var queries = new List<string>();
        int count = 120_000;

        for (int i = 0; i < args.Length; i++)
        {
            if (args[i].Equals("--index", StringComparison.OrdinalIgnoreCase) && i + 1 < args.Length)
            {
                indexPath = args[++i];
                continue;
            }

            if (args[i].Equals("--query", StringComparison.OrdinalIgnoreCase) && i + 1 < args.Length)
            {
                queries.Add(args[++i]);
                continue;
            }

            if (int.TryParse(args[i], out var parsedCount) && parsedCount > 0)
            {
                count = Math.Min(parsedCount, 2_000_000);
            }
        }

        bool skipHeavyPoll = args.Contains("--quick", StringComparer.OrdinalIgnoreCase)
                             || args.Contains("1", StringComparer.Ordinal);

        GCSettings.LargeObjectHeapCompactionMode = GCLargeObjectHeapCompactionMode.CompactOnce;
        GC.Collect(GC.MaxGeneration, GCCollectionMode.Forced, true, true);
        GC.WaitForPendingFinalizers();

        var proc = Process.GetCurrentProcess();
        proc.Refresh();
        var ws0 = proc.WorkingSet64;

        var index = new FileIndex();
        if (!string.IsNullOrWhiteSpace(indexPath))
        {
            var usns = new Dictionary<char, ulong>();
            var swLoad = Stopwatch.StartNew();
            var loaded = IndexSerializer.TryLoadBinary(indexPath, index, usns);
            swLoad.Stop();
            Console.WriteLine($"FindX.Bench  binary index={indexPath}  loaded={loaded:N0}  PID={proc.Id}");
            Console.WriteLine($"  Load index: {swLoad.Elapsed.TotalSeconds:F2}s");
        }
        else
        {
            Console.WriteLine($"FindX.Bench  synthetic entries={count:N0}  PID={proc.Id}");
            var swBulk = Stopwatch.StartNew();
            index.BeginBulk();
            BuildSyntheticIndex(index, count);
            index.EndBulk();
            swBulk.Stop();
            Console.WriteLine($"  Bulk build + EndBulk: {swBulk.Elapsed.TotalSeconds:F2}s");
        }

        proc.Refresh();
        var ws1 = proc.WorkingSet64;

        Console.WriteLine($"  Working set: {ws0 / 1024.0 / 1024:F1} MB -> {ws1 / 1024.0 / 1024:F1} MB (delta {(ws1 - ws0) / 1024.0 / 1024:F1} MB)");
        Console.WriteLine($"  Live entries: {index.Count:N0}");

        if (queries.Count == 0)
        {
            queries.AddRange(indexPath is null
                ? ["bench_a0", "bench_ff", "000100", "yuebao", "bao"]
                : ["gr", "bao", "yuebao", "工人"]);
        }

        var engine = new SearchEngine(index);
        foreach (var query in queries)
            RunSearch(engine, query, 200);

        if (!skipHeavyPoll && indexPath is null)
        {
            Console.WriteLine("  Polling SearchNamePrefix 10x:");
            var swP = Stopwatch.StartNew();
            for (int i = 0; i < 10; i++)
                _ = index.SearchNamePrefix("bench_a0", 30);
            swP.Stop();
            Console.WriteLine($"    10x SearchNamePrefix: {swP.Elapsed.TotalMilliseconds:F1}ms (avg {swP.Elapsed.TotalMilliseconds / 10:F2}ms)");
        }

        Console.WriteLine("Done.");
    }

    private static void BuildSyntheticIndex(FileIndex index, int count)
    {
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
    }

    private static void RunSearch(SearchEngine engine, string query, int max)
    {
        var sw = Stopwatch.StartNew();
        var results = engine.Search(query, max);
        sw.Stop();
        Console.WriteLine($"  Search \"{query}\" max={max} -> {results.Count} results in {sw.Elapsed.TotalMilliseconds:F2}ms");
    }
}
