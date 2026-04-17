using FindX.Core.Index;
using FindX.Core.Storage;

namespace FindX.Tests;

public class IndexPersistenceTests
{
    [Fact]
    public void BinaryIndex_RoundTripsVolumeUsnsAndEntries()
    {
        var tempPath = Path.Combine(Path.GetTempPath(), $"findx-{Guid.NewGuid():N}.dat");
        try
        {
            var source = new FileIndex();
            source.AddEntry(new FileEntry
            {
                VolumeLetter = 'C',
                FileRef = 1,
                ParentRef = 0,
                Name = "普查通知.docx",
                Attributes = 0x20,
                Size = 123,
                LastWriteTimeTicks = new DateTime(2026, 4, 10, 6, 0, 0, DateTimeKind.Utc).Ticks,
                CreationTimeTicks = new DateTime(2026, 4, 10, 6, 0, 0, DateTimeKind.Utc).Ticks,
                AccessTimeTicks = new DateTime(2026, 4, 10, 6, 0, 0, DateTimeKind.Utc).Ticks,
            });

            var savedUsns = new Dictionary<char, ulong> { ['C'] = 123456789UL, ['D'] = 987654321UL };
            IndexSerializer.Save(tempPath, source, savedUsns);

            var loaded = new FileIndex();
            var loadedUsns = new Dictionary<char, ulong>();
            var count = IndexSerializer.TryLoadBinary(tempPath, loaded, loadedUsns);

            Assert.Equal(1, count);
            Assert.Equal(savedUsns, loadedUsns);

            var result = new FindX.Core.Search.SearchEngine(loaded).Search("pcha", 10);
            Assert.Contains(result, r => r.Name == "普查通知.docx");
        }
        finally
        {
            if (File.Exists(tempPath))
                File.Delete(tempPath);
        }
    }
}
