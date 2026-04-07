using FindX.Core.Search;
using static FindX.Tests.TestHelpers;

namespace FindX.Tests;

public class MatchingTests
{
    // ─── 引号精确匹配 ───

    [Fact]
    public void QuotedPhrase_MatchesExact()
    {
        var q = QueryParser.Parse("\"hello world\"");
        Assert.True(q.Root!.Match(MakeCtx("hello world.txt", @"C:\hello world.txt")));
        Assert.False(q.Root!.Match(MakeCtx("helloworld.txt", @"C:\helloworld.txt")));
    }

    // ─── 通配符 ───

    [Theory]
    [InlineData("*.txt", "readme.txt", true)]
    [InlineData("*.txt", "Program.cs", false)]
    [InlineData("test?.cs", "test1.cs", true)]
    [InlineData("test?.cs", "testAB.cs", false)]
    [InlineData("doc*.*", "document.pdf", true)]
    [InlineData("doc*.*", "readme.txt", false)]
    public void Wildcard_PatternMatching(string query, string name, bool expected)
    {
        var q = QueryParser.Parse(query);
        Assert.Equal(expected, q.Root!.Match(MakeCtx(name, @$"C:\{name}")));
    }

    // ─── case: / nocase: ───

    [Fact]
    public void CaseModifier_EnforcesCaseSensitivity()
    {
        var q = QueryParser.Parse("case: README");
        Assert.True(q.Root!.Match(MakeCtx("README.md", @"C:\README.md")));
        Assert.False(q.Root!.Match(MakeCtx("readme.md", @"C:\readme.md")));
    }

    [Fact]
    public void NoCaseModifier_IgnoresCase()
    {
        var q = QueryParser.Parse("nocase: readme");
        Assert.True(q.Root!.Match(MakeCtx("README.md", @"C:\README.md")));
        Assert.True(q.Root!.Match(MakeCtx("readme.md", @"C:\readme.md")));
    }

    // ─── wholeword: ───

    [Fact]
    public void WholeWord_MatchesWordBoundary()
    {
        var q = QueryParser.Parse("ww: test");
        Assert.True(q.Root!.Match(MakeCtx("my-test-file.txt", @"C:\my-test-file.txt")));
        Assert.False(q.Root!.Match(MakeCtx("testing.txt", @"C:\testing.txt")));
    }

    // ─── regex: ───

    [Fact]
    public void Regex_MatchesPattern()
    {
        var q = QueryParser.Parse("regex:^test.*\\.cs$");
        Assert.True(q.IsRegex);
        Assert.True(q.Root!.Match(MakeCtx("test1.cs", @"C:\test1.cs")));
        Assert.True(q.Root!.Match(MakeCtx("testHelper.cs", @"C:\testHelper.cs")));
        Assert.False(q.Root!.Match(MakeCtx("readme.txt", @"C:\readme.txt")));
    }

    [Fact]
    public void Regex_InvalidPattern_FallsBackToTerm()
    {
        var q = QueryParser.Parse("regex:[invalid");
        Assert.False(q.IsRegex);
        Assert.IsType<TermNode>(q.Root);
    }

    // ─── count: ───

    [Fact]
    public void Count_SetsMaxCount()
    {
        var q = QueryParser.Parse("count:5 test");
        Assert.Equal(5, q.MaxCount);
        Assert.IsType<TermNode>(q.Root);
    }

    [Fact]
    public void Count_InvalidValue_Ignored()
    {
        var q = QueryParser.Parse("count:abc test");
        Assert.Null(q.MaxCount);
    }
}
