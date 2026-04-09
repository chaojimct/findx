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
        entry.ComputePinyin();
        index.AddEntry(entry);

        var engine = new SearchEngine(index);
        var results = engine.Search("MetadataSmoke", 10);

        var r = Assert.Single(results);
        Assert.Equal(expectedSize, r.Size);
        Assert.Equal(expectedTicks, r.LastWriteUtcTicks);
        Assert.Equal(expectedTicks, r.LastModified.ToUniversalTime().Ticks);
    }
}
