using FindX.Core.Search;
using static FindX.Tests.TestHelpers;

namespace FindX.Tests;

/// <summary>
/// Everything 兼容语义 + FindX 扩展（volroot、引号路径值）及组合场景。
/// </summary>
public class EverythingSyntaxCompatTests
{
    // ─── root: / volroot: ───

    [Theory]
    [InlineData(@"root:C:\Users", @"C:\Users\dev\app.cs", true)]
    [InlineData(@"root:C:\Users", @"C:\Users\app.cs", true)]
    [InlineData(@"root:C:\Users", @"C:\Users2\app.cs", false)]
    [InlineData(@"root:C:\Users\", @"C:\Users\dev\app.cs", true)]
    [InlineData(@"root:C:/Users", @"C:\Users\dev\app.cs", true)]
    public void RootPath_EverythingStyle_Prefix(string query, string fullPath, bool expected)
    {
        var q = QueryParser.Parse(query);
        Assert.Equal(expected, q.Root!.Match(MakeCtx("app.cs", fullPath)));
        Assert.NotNull(q.RootPathFilter);
    }

    [Fact]
    public void RootPath_WildcardPrefix()
    {
        var q = QueryParser.Parse(@"root:C:\Users\*\src");
        Assert.True(q.Root!.Match(MakeCtx("a.cs", @"C:\Users\john\src\a.cs")));
        Assert.False(q.Root!.Match(MakeCtx("a.cs", @"C:\Users\john\lib\a.cs")));
    }

    [Fact]
    public void RootEmpty_StillMeansDepthOne()
    {
        var q = QueryParser.Parse("root:");
        Assert.Null(q.RootPathFilter);
        Assert.True(q.Root!.Match(MakeCtx("a.cs", @"C:\a.cs")));
        Assert.False(q.Root!.Match(MakeCtx("a.cs", @"C:\a\b\a.cs")));
    }

    [Fact]
    public void VolRoot_FindXExtension_AlwaysDepthOne()
    {
        var q = QueryParser.Parse("volroot:");
        Assert.Null(q.RootPathFilter);
        Assert.True(q.Root!.Match(MakeCtx("a.cs", @"C:\a.cs")));
        Assert.False(q.Root!.Match(MakeCtx("a.cs", @"C:\a\b\a.cs")));
    }

    [Fact]
    public void RootPath_QuotedValue_WithSpaces()
    {
        var q = QueryParser.Parse(@"root:""C:\Program Files""");
        Assert.True(q.Root!.Match(MakeCtx("x.exe", @"C:\Program Files\app\x.exe")));
        Assert.False(q.Root!.Match(MakeCtx("x.exe", @"C:\Program Files (x86)\app\x.exe")));
    }

    // ─── regex: 值内含 | < > ───

    [Fact]
    public void Tokenizer_RegexValue_KeepsPipeAndAngles()
    {
        var t = QueryTokenizer.Tokenize(@"regex:^(a|b)\.cs$");
        Assert.Single(t);
        Assert.Equal(TokenType.Filter, t[0].Type);
        Assert.Equal("regex", t[0].FilterPrefix);
        Assert.Equal(@"^(a|b)\.cs$", t[0].Value);
    }

    [Fact]
    public void Regex_Alternation_MatchesEitherName()
    {
        var q = QueryParser.Parse(@"regex:^(a|b)\.cs$");
        Assert.True(q.IsRegex);
        Assert.True(q.Root!.Match(MakeCtx("a.cs", @"C:\a.cs")));
        Assert.True(q.Root!.Match(MakeCtx("b.cs", @"C:\b.cs")));
        Assert.False(q.Root!.Match(MakeCtx("c.cs", @"C:\c.cs")));
    }

    [Fact]
    public void Regex_Then_Or_StillSplitsOutsideRegex()
    {
        var t = QueryTokenizer.Tokenize(@"regex:^a\.cs$ | readme");
        Assert.Contains(t, x => x.Type == TokenType.OrOp);
        var q = QueryParser.Parse(@"regex:^a\.cs$ | readme");
        Assert.IsType<OrNode>(q.Root);
    }

    // ─── case: / nocase: / ww: 紧贴值 ───

    [Fact]
    public void CaseModifier_CompactForm_EmitsCaseSensitiveTerm()
    {
        var q = QueryParser.Parse("case:README");
        Assert.Contains("README", q.Keywords);
        Assert.True(q.Root!.Match(MakeCtx("README.md", @"C:\README.md")));
        Assert.False(q.Root!.Match(MakeCtx("readme.md", @"C:\readme.md")));
    }

    [Fact]
    public void NoCase_CompactForm_Term()
    {
        var q = QueryParser.Parse("nocase:ReadMe");
        Assert.True(q.Root!.Match(MakeCtx("readme.md", @"C:\readme.md")));
    }

    [Fact]
    public void WholeWord_CompactForm()
    {
        var q = QueryParser.Parse("wholeword:report");
        Assert.True(q.Root!.Match(MakeCtx("monthly report.doc", @"C:\monthly report.doc")));
        Assert.False(q.Root!.Match(MakeCtx("reporting.doc", @"C:\reporting.doc")));
    }

    // ─── path: 引号值 ───

    [Fact]
    public void Path_Quoted_WithSpaces()
    {
        var q = QueryParser.Parse(@"path:""C:\Program Files""");
        Assert.True(q.Root!.Match(MakeCtx("e.exe", @"C:\Program Files\Microsoft\e.exe")));
    }

    // ─── 组合 ───

    [Fact]
    public void Combined_RootPath_And_Ext_And_Term()
    {
        var q = QueryParser.Parse(@"root:C:\work ext:cs main");
        var ctx = MakeCtx("main.cs", @"C:\work\proj\main.cs");
        Assert.True(q.Root!.Match(ctx));
        Assert.Contains("main", q.Keywords);
    }

    [Fact]
    public void Combined_OrOfRootFilters_And_Extensions()
    {
        var q = QueryParser.Parse(@"<root:C:\a | root:C:\b> ext:txt");
        Assert.True(q.Root!.Match(MakeCtx("t.txt", @"C:\a\x\t.txt")));
        Assert.True(q.Root!.Match(MakeCtx("t.txt", @"C:\b\y\t.txt")));
        Assert.False(q.Root!.Match(MakeCtx("t.txt", @"C:\c\t.txt")));
    }

    [Fact]
    public void Combined_CaseCompact_Ext_Not_Group()
    {
        var q = QueryParser.Parse(@"case:TODO ext:cs !bak");
        var and = Assert.IsType<AndNode>(q.Root);
        Assert.True(and.Children.Count >= 3);
    }

    [Fact]
    public void Combined_VolRoot_WithExt()
    {
        var q = QueryParser.Parse("volroot: ext:txt");
        Assert.True(q.Root!.Match(MakeCtx("a.txt", @"D:\a.txt")));
        Assert.False(q.Root!.Match(MakeCtx("a.txt", @"D:\sub\a.txt")));
    }

    [Fact]
    public void Combined_RegexAlternation_WithFileFilter()
    {
        var q = QueryParser.Parse(@"regex:^(foo|bar)\.dll$ file:");
        Assert.True(q.Root!.Match(MakeCtx("foo.dll", @"C:\foo.dll")));
        Assert.False(q.Root!.Match(MakeCtx("foo.dll", @"C:\x\", isDir: true)));
    }

    [Fact]
    public void Combined_Dm_Size_Depth_Path()
    {
        var q = QueryParser.Parse(@"dm:today size:>0 depth:<=5 path:src");
        var ticks = DateTime.Now.Date.AddHours(12).Ticks;
        Assert.True(q.Root!.Match(MakeCtx("a.cs", @"C:\repo\src\a.cs", size: 100, mtime: ticks)));
    }
}
