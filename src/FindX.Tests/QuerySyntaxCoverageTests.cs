using FindX.Core.Search;
using static FindX.Tests.TestHelpers;

namespace FindX.Tests;

public class QuerySyntaxCoverageTests
{
    [Theory]
    [InlineData("audio:", "track.mp3", true)]
    [InlineData("audio:", "clip.mp4", false)]
    [InlineData("video:", "clip.mp4", true)]
    [InlineData("video:", "track.mp3", false)]
    [InlineData("doc:", "report.docx", true)]
    [InlineData("doc:", "photo.png", false)]
    [InlineData("exe:", "setup.exe", true)]
    [InlineData("exe:", "archive.zip", false)]
    [InlineData("zip:", "archive.7z", true)]
    [InlineData("zip:", "setup.exe", false)]
    [InlineData("pic:", "photo.webp", true)]
    [InlineData("pic:", "report.docx", false)]
    [InlineData("type:audio", "track.flac", true)]
    [InlineData("type:image", "poster.png", true)]
    [InlineData("type:image", "track.flac", false)]
    public void TypeAndMacroFilters_MatchExpectedExtensions(string query, string name, bool expected)
    {
        var parsed = QueryParser.Parse(query);
        Assert.Equal(expected, parsed.Root!.Match(MakeCtx(name, @$"C:\sample\{name}")));
    }

    [Theory]
    [InlineData("datemodified:2024", 2024, true)]
    [InlineData("datemodified:2024", 2025, false)]
    [InlineData("datecreated:2024", 2024, true)]
    [InlineData("datecreated:2024", 2025, false)]
    [InlineData("dateaccessed:2024", 2024, true)]
    [InlineData("dateaccessed:2024", 2025, false)]
    public void DateAliases_AreParsedAndMatched(string query, int year, bool expected)
    {
        var parsed = QueryParser.Parse(query);
        var ticks = new DateTime(year, 6, 15, 12, 0, 0, DateTimeKind.Utc).Ticks;
        var ctx = query switch
        {
            var q when q.StartsWith("datecreated:", StringComparison.OrdinalIgnoreCase)
                => MakeCtx(new FindX.Core.Index.FileEntry
                {
                    Name = "sample.txt",
                    Attributes = 0x20,
                    CreationTimeTicks = ticks,
                }, @"C:\sample.txt"),
            var q when q.StartsWith("dateaccessed:", StringComparison.OrdinalIgnoreCase)
                => MakeCtx(new FindX.Core.Index.FileEntry
                {
                    Name = "sample.txt",
                    Attributes = 0x20,
                    AccessTimeTicks = ticks,
                }, @"C:\sample.txt"),
            _
                => MakeCtx("sample.txt", @"C:\sample.txt", mtime: ticks),
        };

        Assert.Equal(expected, parsed.Root!.Match(ctx));
    }

    [Fact]
    public void ParentsAlias_MapsToDepthFilter()
    {
        var parsed = QueryParser.Parse("parents:<=2");

        Assert.True(parsed.Root!.Match(MakeCtx("a.txt", @"C:\a.txt")));
        Assert.True(parsed.Root!.Match(MakeCtx("a.txt", @"C:\top\a.txt")));
        Assert.False(parsed.Root!.Match(MakeCtx("a.txt", @"C:\a\b\c\d\a.txt")));
    }

    [Fact]
    public void WholeWordAlias_ParsesAndMatches()
    {
        var parsed = QueryParser.Parse("wholeword: report");

        Assert.True(parsed.Root!.Match(MakeCtx("monthly report.docx", @"C:\monthly report.docx")));
        Assert.False(parsed.Root!.Match(MakeCtx("reporting.docx", @"C:\reporting.docx")));
    }

    [Fact]
    public void ShellDownloads_ResolvesToUserDownloadsPath()
    {
        var parsed = QueryParser.Parse("shell:downloads");
        var downloads = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
            "Downloads",
            "demo.txt");

        Assert.True(parsed.Root!.Match(MakeCtx("demo.txt", downloads)));
        Assert.False(parsed.Root!.Match(MakeCtx("demo.txt", @"D:\Downloads\demo.txt")));
    }

    [Fact]
    public void ShellDesktop_ResolvesToDesktopPath()
    {
        var parsed = QueryParser.Parse("shell:desktop");
        var desktop = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.DesktopDirectory),
            "demo.txt");

        Assert.True(parsed.Root!.Match(MakeCtx("demo.txt", desktop)));
    }

    [Fact]
    public void UnknownFilter_FallsBackToNormalTerm()
    {
        var parsed = QueryParser.Parse("unknown:token");

        var term = Assert.IsType<TermNode>(parsed.Root);
        Assert.Equal("unknown:token", term.Pattern);
        Assert.Equal(["unknown:token"], parsed.Keywords);
    }

    [Fact]
    public void PathWildcardFilter_MatchesFullPath()
    {
        var parsed = QueryParser.Parse(@"path:*project*\src\*.cs");

        Assert.True(parsed.Root!.Match(MakeCtx("app.cs", @"C:\work\project-a\src\app.cs")));
        Assert.False(parsed.Root!.Match(MakeCtx("app.cs", @"C:\work\project-a\tests\app.cs")));
    }

    [Fact]
    public void CombinedSyntax_GroupFilterModifierAndNot_AreAllParsed()
    {
        var parsed = QueryParser.Parse("case: README <type:doc | ext:md> !temp");
        var and = Assert.IsType<AndNode>(parsed.Root);

        Assert.Equal(3, and.Children.Count);
        Assert.IsType<TermNode>(and.Children[0]);
        Assert.IsType<OrNode>(and.Children[1]);
        Assert.IsType<NotNode>(and.Children[2]);
    }
}
