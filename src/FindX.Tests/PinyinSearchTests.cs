using System.Reflection;
using FindX.Core.Index;
using FindX.Core.Pinyin;
using FindX.Core.Search;

namespace FindX.Tests;

public class PinyinSearchTests
{
    private const string MonthlyReport = "【彩石智能月报】马春天+3月.docx";
    private const string RetreatNotice = "工人退场确认书.docx";

    [Theory]
    [InlineData("bao")]
    [InlineData("yuebao")]
    public void PinyinMatcher_CanMatchYueBao(string query)
    {
        var match = PinyinMatcher.Match(query, MonthlyReport);
        Assert.True(match.IsMatch);
    }

    [Theory]
    [InlineData("bao")]
    [InlineData("yuebao")]
    [InlineData("月报")]
    public void SearchEngine_CanFindYueBao(string query)
    {
        var index = new FileIndex();
        index.AddEntry(new FileEntry
        {
            VolumeLetter = 'Z',
            FileRef = 200,
            ParentRef = 0,
            Name = MonthlyReport,
            Attributes = 0x20,
            Size = 1024,
            LastWriteTimeTicks = new DateTime(2026, 3, 31, 12, 0, 0, DateTimeKind.Utc).Ticks,
            CreationTimeTicks = new DateTime(2026, 3, 31, 12, 0, 0, DateTimeKind.Utc).Ticks,
            AccessTimeTicks = new DateTime(2026, 3, 31, 12, 0, 0, DateTimeKind.Utc).Ticks,
        });

        var engine = new SearchEngine(index);
        var results = engine.Search(query, 10);

        Assert.Contains(results, r => r.Name.Contains("月报", StringComparison.Ordinal));
    }

    [Theory]
    [InlineData("yuebao", "月报")]
    [InlineData("bao", "报")]
    public void HighlightBuilder_UsesRustHighlightRanges(string query, string expectedHighlight)
    {
        var parts = SearchHighlightBuilder.BuildNameParts(MonthlyReport, query);
        var highlighted = string.Concat(parts.Where(p => p.IsHighlight).Select(p => p.Text));

        Assert.Contains(expectedHighlight, highlighted, StringComparison.Ordinal);
    }

    [Fact]
    public void SearchEngine_DefaultPrefersPinyinMatchesForAsciiQueries()
    {
        var index = new FileIndex();
        AddFolder(index, 10, "ascii");
        AddFolder(index, 11, "pinyin");
        index.AddEntry(new FileEntry
        {
            VolumeLetter = 'Z',
            FileRef = 300,
            ParentRef = 10,
            Name = "yuanbao.png",
            Attributes = 0x20,
            Size = 10,
        });
        index.AddEntry(new FileEntry
        {
            VolumeLetter = 'Z',
            FileRef = 301,
            ParentRef = 11,
            Name = MonthlyReport,
            Attributes = 0x20,
            Size = 10,
        });

        var engine = new SearchEngine(index);
        var results = engine.Search("bao", 10);

        Assert.Equal(MonthlyReport, results[0].Name);
    }

    [Fact]
    public void Scorer_CanDisablePinyinPreferredRankingBonus()
    {
        var entry = new FileEntry
        {
            VolumeLetter = 'Z',
            FileRef = 401,
            ParentRef = 21,
            Name = MonthlyReport,
            Attributes = 0x20,
            Size = 10,
        };
        var match = new PinyinMatcher.MatchResult(PinyinMatcher.MatchType.FullPinyin, 420, 3);

        var boosted = Scorer.Score(entry, 3, match, true);
        var plain = Scorer.Score(entry, 3, match, false);

        Assert.True(boosted > plain);
    }

    [Fact]
    public void Scorer_PrefersDocumentsOverLowValueAssetsForAsciiPinyinQueries()
    {
        var report = new FileEntry
        {
            VolumeLetter = 'Z',
            FileRef = 500,
            ParentRef = 0,
            Name = MonthlyReport,
            Attributes = 0x20,
            Size = 10,
        };
        var icon = new FileEntry
        {
            VolumeLetter = 'Z',
            FileRef = 501,
            ParentRef = 0,
            Name = "驾考宝典.png",
            Attributes = 0x20,
            Size = 10,
        };

        var reportScore = Scorer.Score(
            report,
            3,
            new PinyinMatcher.MatchResult(PinyinMatcher.MatchType.FullPinyin, 420, 3),
            true);
        var iconScore = Scorer.Score(
            icon,
            3,
            new PinyinMatcher.MatchResult(PinyinMatcher.MatchType.FullPinyin, 420, 3),
            true);

        Assert.True(reportScore > iconScore);
    }

    [Theory]
    [InlineData("gr", false)]
    [InlineData("g1", false)]
    [InlineData("bao", true)]
    [InlineData("yuebao", true)]
    public void SearchEngine_PinyinSubstringExpansion_SkipsVeryShortAsciiKeywords(string keyword, bool expected)
    {
        Assert.Equal(expected, InvokeSearchEngineGate("ShouldUsePinyinSubstringExpansion", keyword));
    }

    [Theory]
    [InlineData("gr", true)]
    [InlineData("zy", true)]
    [InlineData("bao", false)]
    [InlineData("月报", false)]
    public void SearchEngine_ShortAsciiInitialsExpansion_IsOnlyForTwoLetterAsciiKeywords(string keyword, bool expected)
    {
        Assert.Equal(expected, InvokeSearchEngineGate("ShouldUseShortAsciiInitialsExpansion", keyword));
    }

    [Theory]
    [InlineData("tchang", true, "tc")]
    [InlineData("tuichang", true, "tc")]
    [InlineData("yuebao", true, "yb")]
    [InlineData("bao", false, "")]
    public void SearchEngine_CanBuildAsciiPinyinInitialsAnchor(string keyword, bool expected, string anchor)
    {
        var method = typeof(SearchEngine).GetMethod("TryBuildAsciiPinyinInitialsAnchor", BindingFlags.NonPublic | BindingFlags.Static);
        Assert.NotNull(method);

        object?[] args = [keyword, null];
        var success = (bool)method!.Invoke(null, args)!;

        Assert.Equal(expected, success);
        Assert.Equal(anchor, (string?)args[1] ?? string.Empty);
    }

    [Theory]
    [InlineData("tchang", true, "chang")]
    [InlineData("tuichang", true, "chang")]
    [InlineData("yuebao", true, "bao")]
    [InlineData("bao", false, "")]
    public void SearchEngine_CanBuildAsciiPinyinTailToken(string keyword, bool expected, string tail)
    {
        var method = typeof(SearchEngine).GetMethod("TryBuildAsciiPinyinTailToken", BindingFlags.NonPublic | BindingFlags.Static);
        Assert.NotNull(method);

        object?[] args = [keyword, null];
        var success = (bool)method!.Invoke(null, args)!;

        Assert.Equal(expected, success);
        Assert.Equal(tail, (string?)args[1] ?? string.Empty);
    }

    [Fact]
    public void SearchEngine_CanFindTChangMixedPinyin()
    {
        var index = new FileIndex();
        index.AddEntry(new FileEntry
        {
            VolumeLetter = 'Z',
            FileRef = 600,
            ParentRef = 0,
            Name = RetreatNotice,
            Attributes = 0x20,
            Size = 1024,
        });

        var engine = new SearchEngine(index);
        var results = engine.Search("tchang", 10);

        Assert.Contains(results, r => r.Name.Contains("退场", StringComparison.Ordinal));
    }

    private static bool InvokeSearchEngineGate(string methodName, string keyword)
    {
        var method = typeof(SearchEngine).GetMethod(methodName, BindingFlags.NonPublic | BindingFlags.Static);
        Assert.NotNull(method);
        return (bool)method!.Invoke(null, [new[] { keyword }])!;
    }

    private static void AddFolder(FileIndex index, ulong fileRef, string name)
    {
        index.AddEntry(new FileEntry
        {
            VolumeLetter = 'Z',
            FileRef = fileRef,
            ParentRef = 0,
            Name = name,
            Attributes = 0x10,
        });
    }
}
