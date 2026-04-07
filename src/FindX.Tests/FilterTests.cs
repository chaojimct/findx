using FindX.Core.Search;
using static FindX.Tests.TestHelpers;

namespace FindX.Tests;

public class FilterTests
{
    // ─── file: / folder: ───

    [Fact]
    public void FileFilter_MatchesFileOnly()
    {
        var q = QueryParser.Parse("file:");
        var file = MakeCtx("test.cs", @"C:\test.cs");
        var dir = MakeCtx("src", @"C:\src", isDir: true);

        Assert.True(q.Root!.Match(file));
        Assert.False(q.Root!.Match(dir));
    }

    [Fact]
    public void FolderFilter_MatchesFolderOnly()
    {
        var q = QueryParser.Parse("folder:");
        var file = MakeCtx("test.cs", @"C:\test.cs");
        var dir = MakeCtx("src", @"C:\src", isDir: true);

        Assert.True(q.Root!.Match(dir));
        Assert.False(q.Root!.Match(file));
    }

    // ─── ext: ───

    [Theory]
    [InlineData("ext:cs", "Program.cs", true)]
    [InlineData("ext:cs", "readme.txt", false)]
    [InlineData("ext:CS", "Program.cs", true)]  // 大小写不敏感
    public void ExtFilter_MatchesExtension(string query, string name, bool expected)
    {
        var q = QueryParser.Parse(query);
        var ctx = MakeCtx(name, @$"C:\{name}");
        Assert.Equal(expected, q.Root!.Match(ctx));
    }

    [Fact]
    public void ExtFilter_MultipleExtensions()
    {
        var q = QueryParser.Parse("ext:cs;txt");
        Assert.True(q.Root!.Match(MakeCtx("a.cs", @"C:\a.cs")));
        Assert.True(q.Root!.Match(MakeCtx("b.txt", @"C:\b.txt")));
        Assert.False(q.Root!.Match(MakeCtx("c.pdf", @"C:\c.pdf")));
    }

    // ─── size: ───

    [Theory]
    [InlineData("size:>1mb", 500, false)]           // 500B < 1MB
    [InlineData("size:>1mb", 1048576, false)]        // 1MB not > 1MB
    [InlineData("size:>1mb", 2147483648, true)]      // 2GB > 1MB
    [InlineData("size:<1kb", 500, true)]             // 500B < 1KB
    [InlineData("size:<1kb", 1048576, false)]        // 1MB >= 1KB
    [InlineData("size:>=1mb", 1048576, true)]        // 1MB >= 1MB
    [InlineData("size:<=1kb", 1024, true)]           // 1KB <= 1KB
    public void SizeFilter_CompareOps(string query, long size, bool expected)
    {
        var q = QueryParser.Parse(query);
        var ctx = MakeCtx("file.bin", @"C:\file.bin", size: size);
        Assert.Equal(expected, q.Root!.Match(ctx));
    }

    [Fact]
    public void SizeFilter_Range()
    {
        var q = QueryParser.Parse("size:100kb..2mb");
        Assert.False(q.Root!.Match(MakeCtx("s.bin", @"C:\s.bin", size: 500)));
        Assert.True(q.Root!.Match(MakeCtx("m.bin", @"C:\m.bin", size: 1048576)));
        Assert.False(q.Root!.Match(MakeCtx("b.bin", @"C:\b.bin", size: 2147483648)));
    }

    // ─── dm: (date modified) ───

    [Fact]
    public void DmFilter_Today()
    {
        var q = QueryParser.Parse("dm:today");
        var today = MakeCtx("new.txt", @"C:\new.txt", mtime: DateTime.Now.Ticks);
        var old = MakeCtx("old.txt", @"C:\old.txt", mtime: new DateTime(2020, 1, 1).Ticks);

        Assert.True(q.Root!.Match(today));
        Assert.False(q.Root!.Match(old));
    }

    [Fact]
    public void DmFilter_Year()
    {
        var q = QueryParser.Parse("dm:2020");
        var match = MakeCtx("a.txt", @"C:\a.txt", mtime: new DateTime(2020, 6, 15).Ticks);
        var noMatch = MakeCtx("b.txt", @"C:\b.txt", mtime: DateTime.Now.Ticks);

        Assert.True(q.Root!.Match(match));
        Assert.False(q.Root!.Match(noMatch));
    }

    [Fact]
    public void DmFilter_GreaterThan()
    {
        var q = QueryParser.Parse("dm:>2023-01-01");
        var recent = MakeCtx("a.txt", @"C:\a.txt", mtime: DateTime.Now.Ticks);
        var old = MakeCtx("b.txt", @"C:\b.txt", mtime: new DateTime(2020, 1, 1).Ticks);

        Assert.True(q.Root!.Match(recent));
        Assert.False(q.Root!.Match(old));
    }

    [Fact]
    public void DmFilter_Range()
    {
        var q = QueryParser.Parse("dm:2020-01-01..2020-12-31");
        var inRange = MakeCtx("a.txt", @"C:\a.txt", mtime: new DateTime(2020, 6, 15).Ticks);
        var outRange = MakeCtx("b.txt", @"C:\b.txt", mtime: new DateTime(2019, 6, 15).Ticks);

        Assert.True(q.Root!.Match(inRange));
        Assert.False(q.Root!.Match(outRange));
    }

    // ─── path: / nopath: / parent: ───

    [Fact]
    public void PathFilter_Substring()
    {
        var q = QueryParser.Parse(@"path:project\src");
        Assert.True(q.Root!.Match(MakeCtx("a.cs", @"C:\project\src\a.cs")));
        Assert.False(q.Root!.Match(MakeCtx("a.cs", @"D:\backup\a.cs")));
    }

    [Fact]
    public void NoPathFilter_Excludes()
    {
        var q = QueryParser.Parse("nopath:backup");
        Assert.True(q.Root!.Match(MakeCtx("a.cs", @"C:\project\a.cs")));
        Assert.False(q.Root!.Match(MakeCtx("a.cs", @"D:\backup\a.cs")));
    }

    [Fact]
    public void ParentFilter_PrefixMatch()
    {
        var q = QueryParser.Parse(@"parent:C:\Users");
        Assert.True(q.Root!.Match(MakeCtx("a.cs", @"C:\Users\dev\a.cs")));
        Assert.False(q.Root!.Match(MakeCtx("a.cs", @"D:\data\a.cs")));
    }

    // ─── len: ───

    [Theory]
    [InlineData("len:<10", "a.cs", true)]       // len=4
    [InlineData("len:<10", "very_long_filename.txt", false)]  // len=22
    [InlineData("len:3..5", "a.cs", true)]      // len=4
    [InlineData("len:3..5", "ab.cs", true)]     // len=5
    [InlineData("len:3..5", "abcdef.cs", false)] // len=9
    public void LenFilter(string query, string name, bool expected)
    {
        var q = QueryParser.Parse(query);
        Assert.Equal(expected, q.Root!.Match(MakeCtx(name, @$"C:\{name}")));
    }

    // ─── depth: ───

    [Fact]
    public void DepthFilter()
    {
        var q = QueryParser.Parse("depth:<=2");
        Assert.True(q.Root!.Match(MakeCtx("a.cs", @"C:\a.cs")));         // depth=1
        Assert.False(q.Root!.Match(MakeCtx("a.cs", @"C:\a\b\c\d\a.cs"))); // depth=5
    }

    // ─── root: ───

    [Fact]
    public void RootFilter()
    {
        var q = QueryParser.Parse("root:");
        Assert.True(q.Root!.Match(MakeCtx("a.cs", @"C:\a.cs")));
        Assert.False(q.Root!.Match(MakeCtx("a.cs", @"C:\a\b\c\a.cs")));
    }

    // ─── attrib: ───

    [Fact]
    public void AttribFilter_Hidden()
    {
        var q = QueryParser.Parse("attrib:H");
        Assert.True(q.Root!.Match(MakeCtx("h.txt", @"C:\h.txt", attr: 0x22)));   // HIDDEN+ARCHIVE
        Assert.False(q.Root!.Match(MakeCtx("n.txt", @"C:\n.txt", attr: 0x20)));   // ARCHIVE only
    }

    [Fact]
    public void AttribFilter_ReadonlySystem()
    {
        var q = QueryParser.Parse("attrib:RS");
        var rs = MakeCtx("sys.dll", @"C:\sys.dll", attr: 0x05);  // READONLY+SYSTEM
        var normal = MakeCtx("n.txt", @"C:\n.txt", attr: 0x20);

        Assert.True(q.Root!.Match(rs));
        Assert.False(q.Root!.Match(normal));
    }

    // ─── startwith: / endwith: ───

    [Theory]
    [InlineData("startwith:Test", "TestReport.pdf", true)]
    [InlineData("startwith:Test", "myTest.pdf", false)]
    [InlineData("endwith:.pdf", "TestReport.pdf", true)]
    [InlineData("endwith:.pdf", "TestReport.doc", false)]
    public void StartEndWith(string query, string name, bool expected)
    {
        var q = QueryParser.Parse(query);
        Assert.Equal(expected, q.Root!.Match(MakeCtx(name, @$"C:\{name}")));
    }
}
