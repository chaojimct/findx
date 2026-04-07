using FindX.Core.Search;
using static FindX.Tests.TestHelpers;

namespace FindX.Tests;

public class BooleanTests
{
    private readonly EvalContext _abc = MakeCtx("abc_def.txt", @"C:\abc_def.txt");
    private readonly EvalContext _xyz = MakeCtx("xyz.txt", @"C:\xyz.txt");
    private readonly EvalContext _other = MakeCtx("other.pdf", @"C:\other.pdf");

    // ─── AND ───

    [Fact]
    public void And_AllTermsMustMatch()
    {
        var q = QueryParser.Parse("abc def");
        Assert.IsType<AndNode>(q.Root);
        Assert.True(q.Root!.Match(_abc));
        Assert.False(q.Root!.Match(_xyz));
    }

    // ─── OR ───

    [Fact]
    public void Or_AnyTermCanMatch()
    {
        var q = QueryParser.Parse("abc | xyz");
        Assert.IsType<OrNode>(q.Root);
        Assert.True(q.Root!.Match(_abc));
        Assert.True(q.Root!.Match(_xyz));
        Assert.False(q.Root!.Match(_other));
    }

    // ─── NOT ───

    [Fact]
    public void Not_InvertsMatch()
    {
        var q = QueryParser.Parse("!abc");
        Assert.False(q.Root!.Match(_abc));
        Assert.True(q.Root!.Match(_xyz));
    }

    // ─── 组合 ───

    [Fact]
    public void FilterAndNot_Combination()
    {
        var q = QueryParser.Parse("ext:txt !abc");
        Assert.True(q.Root!.Match(_xyz));
        Assert.False(q.Root!.Match(_abc));   // abc 被 NOT 排除
        Assert.False(q.Root!.Match(_other)); // ext 不匹配
    }

    [Fact]
    public void GroupedOr_WithFilter()
    {
        var q = QueryParser.Parse("<abc | xyz> ext:txt");
        Assert.IsType<AndNode>(q.Root);
        Assert.True(q.Root!.Match(_abc));
        Assert.True(q.Root!.Match(_xyz));
        Assert.False(q.Root!.Match(_other));
    }

    [Fact]
    public void NestedNot_DoubleNegation()
    {
        var q = QueryParser.Parse("!!abc");
        Assert.True(q.Root!.Match(_abc));
        Assert.False(q.Root!.Match(_xyz));
    }

    [Fact]
    public void ComplexExpression()
    {
        // (abc OR xyz) AND NOT other AND ext:txt
        var q = QueryParser.Parse("<abc | xyz> !other ext:txt");
        Assert.True(q.Root!.Match(_abc));
        Assert.True(q.Root!.Match(_xyz));
        Assert.False(q.Root!.Match(_other));
    }
}
