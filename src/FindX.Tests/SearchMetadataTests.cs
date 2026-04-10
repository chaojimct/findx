using FindX.Core.Index;
using FindX.Core.Search;

namespace FindX.Tests;

public class SearchMetadataTests
{
    [Fact]
    public void SearchResult_携带来自索引的大小与最后写入时间_ticks()
    {
        var expectedUtc = new DateTime(2024, 6, 15, 12, 0, 0, DateTimeKind.Utc);
        long expectedTicks = expectedUtc.Ticks;
        const long expectedSize = 123_456;

        var index = new FileIndex();
        var entry = new FileEntry
        {
            VolumeLetter = 'Z',
            FileRef = 100,
            ParentRef = 0,
            Name = "MetadataSmoke.txt",
            Attributes = 0x20,
            Size = expectedSize,
            LastWriteTimeTicks = expectedTicks,
            CreationTimeTicks = expectedTicks,
            AccessTimeTicks = expectedTicks,
        };
        index.AddEntry(entry);

        var engine = new SearchEngine(index);
        var results = engine.Search("MetadataSmoke", 10);

        var r = Assert.Single(results);
        Assert.Equal(expectedSize, r.Size);
        Assert.Equal(expectedTicks, r.LastWriteUtcTicks);
        Assert.Equal(expectedTicks, r.LastModified.ToUniversalTime().Ticks);
    }

    [Fact]
    public void SearchResult_UnknownMtime_RemainsZeroInsteadOfYear2000()
    {
        var index = new FileIndex();
        index.AddEntry(new FileEntry
        {
            VolumeLetter = 'Z',
            FileRef = 101,
            ParentRef = 0,
            Name = "UnknownMtime.txt",
            Attributes = 0x20,
            Size = 0,
            LastWriteTimeTicks = 0,
            CreationTimeTicks = 0,
            AccessTimeTicks = 0,
        });

        var engine = new SearchEngine(index);
        var results = engine.Search("UnknownMtime", 10);

        var r = Assert.Single(results);
        Assert.Equal(0, r.LastWriteUtcTicks);
        Assert.Equal(default, r.LastModified);
    }

    [Fact]
    public void SearchResult_MissingMetadata_CanHydrateFromRealFile()
    {
        var root = Path.Combine(Path.GetTempPath(), "FindX.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);

        try
        {
            var filePath = Path.Combine(root, "metadata-hydrate.txt");
            File.WriteAllText(filePath, "hydration works");
            var info = new FileInfo(filePath);
            var volume = char.ToUpperInvariant(Path.GetPathRoot(root)![0]);

            var index = new FileIndex();
            AddPathChain(index, root);
            const ulong fileRef = 9000;
            index.AddEntry(new FileEntry
            {
                VolumeLetter = volume,
                FileRef = fileRef,
                ParentRef = GetPathRef(root),
                Name = Path.GetFileName(filePath),
                Attributes = 0x20,
                Size = 0,
                LastWriteTimeTicks = 0,
                CreationTimeTicks = 0,
                AccessTimeTicks = 0,
            });
            index.RebuildNameIndex();

            var engine = new SearchEngine(index);
            var results = engine.Search("metadata-hydrate", 10);

            var r = Assert.Single(results);
            Assert.Equal(info.Length, r.Size);
            Assert.Equal(info.LastWriteTimeUtc.Ticks, r.LastWriteUtcTicks);
        }
        finally
        {
            if (Directory.Exists(root))
                Directory.Delete(root, true);
        }
    }

    private static void AddPathChain(FileIndex index, string directoryPath)
    {
        var root = Path.GetPathRoot(directoryPath)!;
        var volume = char.ToUpperInvariant(root[0]);
        var relative = directoryPath[root.Length..];
        if (string.IsNullOrEmpty(relative))
            return;

        ulong parentRef = 0;
        var current = root;
        foreach (var part in relative.Split(Path.DirectorySeparatorChar, StringSplitOptions.RemoveEmptyEntries))
        {
            current = Path.Combine(current, part);
            index.AddEntry(new FileEntry
            {
                VolumeLetter = volume,
                FileRef = GetPathRef(current),
                ParentRef = parentRef,
                Name = part,
                Attributes = 0x10,
                Size = 0,
                LastWriteTimeTicks = 0,
                CreationTimeTicks = 0,
                AccessTimeTicks = 0,
            });
            parentRef = GetPathRef(current);
        }
    }

    private static ulong GetPathRef(string path)
        => (ulong)StringComparer.OrdinalIgnoreCase.GetHashCode(path);
}
