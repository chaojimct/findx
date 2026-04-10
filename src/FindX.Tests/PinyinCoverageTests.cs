using System.Reflection;
using FindX.Core.Index;
using FindX.Core.Pinyin;
using FindX.Core.Search;

namespace FindX.Tests;

public class PinyinCoverageTests
{
    [Theory]
    [InlineData("yuebao", "【彩石智能月报】马春天+3月.docx", PinyinMatcher.MatchType.FullPinyin)]
    [InlineData("bao", "【彩石智能月报】马春天+3月.docx", PinyinMatcher.MatchType.FullPinyin)]
    [InlineData("yb", "【彩石智能月报】马春天+3月.docx", PinyinMatcher.MatchType.Initials)]
    [InlineData("grtc", "工人退场确认书.docx", PinyinMatcher.MatchType.Initials)]
    [InlineData("gongren", "工人退场确认书.docx", PinyinMatcher.MatchType.FullPinyin)]
    [InlineData("tuichang", "工人退场确认书.docx", PinyinMatcher.MatchType.FullPinyin)]
    [InlineData("tchang", "工人退场确认书.docx", PinyinMatcher.MatchType.Mixed)]
    [InlineData("shanghai", "上海总结.docx", PinyinMatcher.MatchType.FullPinyin)]
    public void PinyinMatcher_CoversStableFullInitialAndMixedQueries(string query, string name, PinyinMatcher.MatchType expectedType)
    {
        var match = PinyinMatcher.Match(query, name);

        Assert.True(match.IsMatch);
        Assert.Equal(expectedType, match.Type);
    }

    [Theory]
    [InlineData("gr", "工人退场确认书")]
    [InlineData("tc", "工人退场确认书")]
    [InlineData("sh", "上海总结")]
    public void PinyinMatcher_ShortQueriesStillMatchExpectedNames(string query, string namePart)
    {
        var match = PinyinMatcher.Match(query, $"{namePart}.docx");

        Assert.True(match.IsMatch);
    }

    [Theory]
    [InlineData("tchang", "工人退场确认书")]
    [InlineData("tuichang", "工人退场确认书")]
    [InlineData("tc", "工人退场确认书")]
    [InlineData("gr", "工人退场确认书")]
    [InlineData("grtc", "工人退场确认书")]
    [InlineData("gongren", "工人退场确认书")]
    [InlineData("yuebao", "月报")]
    [InlineData("bao", "月报")]
    [InlineData("yb", "月报")]
    [InlineData("shanghai", "上海")]
    [InlineData("sh", "上海")]
    public void SearchEngine_CanFindExpectedPinyinResults(string query, string expectedNamePart)
    {
        var engine = BuildCoverageEngine();
        var results = engine.Search(query, 20);

        Assert.Contains(results, r => r.Name.Contains(expectedNamePart, StringComparison.Ordinal));
    }

    [Theory]
    [InlineData("zhangsan", true, "zs")]
    [InlineData("changcheng", true, "cc")]
    [InlineData("shanghai", true, "sh")]
    [InlineData("tchang", true, "tc")]
    [InlineData("bao", false, "")]
    public void SearchEngine_CanBuildAnchorsForSpecialInitials(string keyword, bool expected, string anchor)
    {
        Assert.Equal(expected, InvokeBoolOutString("TryBuildAsciiPinyinInitialsAnchor", keyword, out var actual));
        Assert.Equal(anchor, actual);
    }

    [Theory]
    [InlineData("zhangsan", true, "san")]
    [InlineData("changcheng", true, "cheng")]
    [InlineData("shanghai", true, "hai")]
    [InlineData("tchang", true, "chang")]
    [InlineData("bao", false, "")]
    public void SearchEngine_CanBuildTailTokensForSpecialInitials(string keyword, bool expected, string tail)
    {
        Assert.Equal(expected, InvokeBoolOutString("TryBuildAsciiPinyinTailToken", keyword, out var actual));
        Assert.Equal(tail, actual);
    }

    [Fact]
    public void SearchEngine_TChangRanksRetreatDocumentWithinTopResults()
    {
        var engine = BuildCoverageEngine();
        var results = engine.Search("tchang", 10);

        Assert.NotEmpty(results);
        Assert.Contains(results, r => r.Name.Contains("退场", StringComparison.Ordinal));
    }

    [Fact]
    public void SearchHighlightBuilder_CanHighlightMixedPinyinQuery()
    {
        var parts = SearchHighlightBuilder.BuildNameParts("工人退场确认书.docx", "tchang");
        var highlighted = string.Concat(parts.Where(p => p.IsHighlight).Select(p => p.Text));

        Assert.Contains("退场", highlighted, StringComparison.Ordinal);
    }

    private static SearchEngine BuildCoverageEngine()
    {
        var index = new FileIndex();
        AddFile(index, 100, "工人退场确认书.docx");
        AddFile(index, 101, "【彩石智能月报】马春天+3月.docx");
        AddFile(index, 102, "上海总结.docx");
        AddFile(index, 103, "textChange.js");
        AddFile(index, 104, "ChtChangjieDS.DLL");
        return new SearchEngine(index);
    }

    private static void AddFile(FileIndex index, ulong fileRef, string name)
    {
        index.AddEntry(new FileEntry
        {
            VolumeLetter = 'Z',
            FileRef = fileRef,
            ParentRef = 0,
            Name = name,
            Attributes = 0x20,
            Size = 1024,
            LastWriteTimeTicks = new DateTime(2026, 4, 1, 12, 0, 0, DateTimeKind.Utc).Ticks,
        });
    }

    private static bool InvokeBoolOutString(string methodName, string keyword, out string value)
    {
        var method = typeof(SearchEngine).GetMethod(methodName, BindingFlags.NonPublic | BindingFlags.Static);
        Assert.NotNull(method);

        object?[] args = [keyword, null];
        var success = (bool)method!.Invoke(null, args)!;
        value = (string?)args[1] ?? string.Empty;
        return success;
    }
}
