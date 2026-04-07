using FindX.Core.Search;

namespace FindX.Tests;

public class ParserTests
{
    [Fact]
    public void SingleWord_ProducesTermNode()
    {
        var q = QueryParser.Parse("hello");
        Assert.IsType<TermNode>(q.Root);
        Assert.Single(q.Keywords);
        Assert.Equal("hello", q.Keywords[0]);
    }

    [Fact]
    public void TwoWords_ProducesAndNode()
    {
        var q = QueryParser.Parse("hello world");
        var and = Assert.IsType<AndNode>(q.Root);
        Assert.Equal(2, and.Children.Count);
    }

    [Fact]
    public void PipeOp_ProducesOrNode()
    {
        var q = QueryParser.Parse("a | b");
        var or = Assert.IsType<OrNode>(q.Root);
        Assert.Equal(2, or.Children.Count);
    }

    [Fact]
    public void ExclamationOp_ProducesNotNode()
    {
        var q = QueryParser.Parse("!hidden");
        Assert.IsType<NotNode>(q.Root);
    }

    [Fact]
    public void GroupedOrWithAnd_ProducesAndOfOrAndTerm()
    {
        var q = QueryParser.Parse("<a | b> c");
        var and = Assert.IsType<AndNode>(q.Root);
        Assert.Equal(2, and.Children.Count);
        Assert.IsType<OrNode>(and.Children[0]);
        Assert.IsType<TermNode>(and.Children[1]);
    }

    [Fact]
    public void QuotedString_ProducesExactTermNode()
    {
        var q = QueryParser.Parse("\"exact phrase\"");
        var term = Assert.IsType<TermNode>(q.Root);
        Assert.True(term.IsExact);
        Assert.Equal("exact phrase", term.Pattern);
    }

    [Fact]
    public void WildcardTerm_HasWildcardFlag()
    {
        var q = QueryParser.Parse("*.dll");
        var term = Assert.IsType<TermNode>(q.Root);
        Assert.True(term.HasWildcard);
    }

    [Fact]
    public void FilterWithTerm_ProducesAndNode()
    {
        var q = QueryParser.Parse("ext:cs hello");
        var and = Assert.IsType<AndNode>(q.Root);
        Assert.IsType<FilterNode>(and.Children[0]);
        Assert.IsType<TermNode>(and.Children[1]);
    }

    [Fact]
    public void Regex_ProducesRegexNode()
    {
        var q = QueryParser.Parse("regex:^test.*\\.cs$");
        Assert.IsType<RegexNode>(q.Root);
        Assert.True(q.IsRegex);
        Assert.NotNull(q.RegexPattern);
    }

    [Fact]
    public void Count_SetsMaxCount()
    {
        var q = QueryParser.Parse("count:10 hello");
        Assert.Equal(10, q.MaxCount);
        Assert.IsType<TermNode>(q.Root);
    }

    [Fact]
    public void NotSubtree_ExcludedFromKeywords()
    {
        var q = QueryParser.Parse("abc !xyz def");
        Assert.Contains("abc", q.Keywords);
        Assert.Contains("def", q.Keywords);
        Assert.DoesNotContain("xyz", q.Keywords);
    }

    [Fact]
    public void FilterTokens_NotInKeywords()
    {
        var q = QueryParser.Parse("ext:cs hello");
        Assert.Single(q.Keywords);
        Assert.Equal("hello", q.Keywords[0]);
    }

    [Fact]
    public void ModifierFilter_SkippedAndNextTokenParsed()
    {
        var q = QueryParser.Parse("case: README");
        Assert.IsType<TermNode>(q.Root);
        var term = (TermNode)q.Root!;
        Assert.True(term.CaseSensitive);
    }

    [Fact]
    public void EmptyQuery_ReturnsNullRoot()
    {
        var q = QueryParser.Parse("");
        Assert.Null(q.Root);
        Assert.Empty(q.Keywords);
    }

    [Fact]
    public void ComplexNested_ParsesCorrectly()
    {
        // <ext:cs | ext:txt> !hidden size:>1mb
        var q = QueryParser.Parse("<ext:cs | ext:txt> !hidden size:>1mb");
        var and = Assert.IsType<AndNode>(q.Root);
        Assert.Equal(3, and.Children.Count);
        Assert.IsType<OrNode>(and.Children[0]);
        Assert.IsType<NotNode>(and.Children[1]);
        Assert.IsType<FilterNode>(and.Children[2]);
    }
}
