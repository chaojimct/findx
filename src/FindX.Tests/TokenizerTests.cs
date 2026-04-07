using FindX.Core.Search;

namespace FindX.Tests;

public class TokenizerTests
{
    [Fact]
    public void SimpleWords_SplitBySpace()
    {
        var tokens = QueryTokenizer.Tokenize("hello world");
        Assert.Equal(2, tokens.Count);
        Assert.All(tokens, t => Assert.Equal(TokenType.Term, t.Type));
    }

    [Fact]
    public void QuotedString_PreservesSpaces()
    {
        var tokens = QueryTokenizer.Tokenize("\"hello world\"");
        Assert.Single(tokens);
        Assert.Equal(TokenType.QuotedString, tokens[0].Type);
        Assert.Equal("hello world", tokens[0].Value);
    }

    [Fact]
    public void Pipe_IsOrOp()
    {
        var tokens = QueryTokenizer.Tokenize("a | b");
        Assert.Equal(3, tokens.Count);
        Assert.Equal(TokenType.OrOp, tokens[1].Type);
    }

    [Fact]
    public void Exclamation_IsNotOp()
    {
        var tokens = QueryTokenizer.Tokenize("!hidden");
        Assert.Equal(2, tokens.Count);
        Assert.Equal(TokenType.NotOp, tokens[0].Type);
        Assert.Equal(TokenType.Term, tokens[1].Type);
    }

    [Fact]
    public void AngleBrackets_AreGroupDelimiters()
    {
        var tokens = QueryTokenizer.Tokenize("<a | b> c");
        Assert.Equal(TokenType.OpenGroup, tokens[0].Type);
        Assert.Equal(TokenType.CloseGroup, tokens[4].Type);
        Assert.Equal(6, tokens.Count);
    }

    [Theory]
    [InlineData("ext:cs", "ext", "cs")]
    [InlineData("size:>1mb", "size", ">1mb")]
    [InlineData("dm:today", "dm", "today")]
    [InlineData("len:<10", "len", "<10")]
    [InlineData("depth:<=3", "depth", "<=3")]
    [InlineData("dm:>2023-01-01", "dm", ">2023-01-01")]
    [InlineData("size:100kb..2mb", "size", "100kb..2mb")]
    public void KnownFilterPrefix_ParsedAsFilter(string input, string prefix, string value)
    {
        var tokens = QueryTokenizer.Tokenize(input);
        Assert.Single(tokens);
        Assert.Equal(TokenType.Filter, tokens[0].Type);
        Assert.Equal(prefix, tokens[0].FilterPrefix);
        Assert.Equal(value, tokens[0].Value);
    }

    [Fact]
    public void UnknownPrefix_TreatedAsTerm()
    {
        var tokens = QueryTokenizer.Tokenize("unknown:value");
        Assert.Single(tokens);
        Assert.Equal(TokenType.Term, tokens[0].Type);
        Assert.Equal("unknown:value", tokens[0].Value);
    }

    [Fact]
    public void Wildcard_StaysInTerm()
    {
        var tokens = QueryTokenizer.Tokenize("*.txt");
        Assert.Single(tokens);
        Assert.Equal(TokenType.Term, tokens[0].Type);
        Assert.Equal("*.txt", tokens[0].Value);
    }

    [Fact]
    public void FilterValueWithGtLt_NotBrokenByDelimiters()
    {
        var tokens = QueryTokenizer.Tokenize("size:>=1mb size:<500kb");
        Assert.Equal(2, tokens.Count);
        Assert.Equal(">=1mb", tokens[0].Value);
        Assert.Equal("<500kb", tokens[1].Value);
    }
}
