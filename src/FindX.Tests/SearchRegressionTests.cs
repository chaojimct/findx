using FindX.Core.Index;
using FindX.Core.Search;

namespace FindX.Tests;

public class SearchRegressionTests
{
    [Fact]
    public void Pcha_PrefersPuChaDocumentsOverAsciiNoise()
    {
        var engine = BuildEngine();

        var results = engine.Search("pcha", 10);

        Assert.NotEmpty(results);
        Assert.Contains(results, r => r.Name == "普查通知.docx");
        Assert.True(results.FindIndex(r => r.Name == "普查通知.docx")
                    < results.FindIndex(r => r.Name == "pchannel-demo.txt"));
    }

    [Fact]
    public void Pucha_FindsPuChaDocuments()
    {
        var engine = BuildEngine();

        var results = engine.Search("pucha", 10);

        Assert.Contains(results, r => r.Name == "普查通知.docx");
        Assert.Contains(results, r => r.Name == "退场申请-普查版.txt");
    }

    [Fact]
    public void MultiKeyword_PuChaTongZhi_FindsExpectedDocument()
    {
        var engine = BuildEngine();

        var results = engine.Search("普查 通知", 10);

        Assert.Contains(results, r => r.Name == "普查通知.docx");
    }

    [Fact]
    public void Tchang_FindsRetreatDocuments()
    {
        var engine = BuildEngine();

        var results = engine.Search("tchang", 10);

        Assert.Contains(results, r => r.Name == "工人退场确认书.docx");
        Assert.Contains(results, r => r.Name == "退场申请-普查版.txt");
    }

    [Fact]
    public void MultiKeyword_GongRenTuiChang_FindsRetreatDocument()
    {
        var engine = BuildEngine();

        var results = engine.Search("工人 退场", 10);

        Assert.Contains(results, r => r.Name == "工人退场确认书.docx");
    }

    [Fact]
    public void Yuebao_FindsMonthlyReportDocuments()
    {
        var engine = BuildEngine();

        var results = engine.Search("yuebao", 10);

        Assert.Contains(results, r => r.Name == "月报汇总.md");
    }

    [Fact]
    public void ShortAscii_Bao_FindsMonthlyReportViaFullPinyinSubstring()
    {
        var engine = BuildEngine();

        var results = engine.Search("bao", 10);

        Assert.Contains(results, r => r.Name == "月报汇总.md");
    }

    [Fact]
    public void MixedAsciiAndChineseNames_RemainSearchable()
    {
        var engine = BuildEngine();

        Assert.Contains(engine.Search("aaa", 10), r => r.Name == "aaa你好.txt");
        Assert.Contains(engine.Search("bbb", 10), r => r.Name == "你知道吗bbb.md");
        Assert.Contains(engine.Search("12312312", 10), r => r.Name == "12312312$$$111.aa");
    }

    [Fact]
    public void ParentPath_SingleAsciiChar_FindsFileUnderDeepFolder()
    {
        var index = new FileIndex();
        index.BeginBulk();
        const char vol = 'Z';
        const uint dirAttr = 0x10;
        const uint fileAttr = 0x20;
        ulong p = 0;
        p = AddDir(index, vol, 1, p, "Users", dirAttr);
        p = AddDir(index, vol, 2, p, "chaoj", dirAttr);
        p = AddDir(index, vol, 3, p, "dev", dirAttr);
        p = AddDir(index, vol, 4, p, "tools", dirAttr);
        p = AddDir(index, vol, 5, p, "findx", dirAttr);
        p = AddDir(index, vol, 6, p, "src", dirAttr);
        p = AddDir(index, vol, 7, p, "FindX.Service", dirAttr);
        p = AddDir(index, vol, 8, p, "obj", dirAttr);
        p = AddDir(index, vol, 9, p, "Debug", dirAttr);
        p = AddDir(index, vol, 10, p, "net8.0-windows", dirAttr);
        index.AddEntry(new FileEntry
        {
            VolumeLetter = vol,
            FileRef = 11,
            ParentRef = p,
            Name = "apphost.exe",
            Attributes = fileAttr,
            Size = 1,
        });
        // 根目录下另有一条「a」前缀，全局前缀桶易排在 apphost 之前；路径感知扫描仍应命中目标目录。
        index.AddEntry(new FileEntry
        {
            VolumeLetter = vol,
            FileRef = 12,
            ParentRef = 0,
            Name = "alphabet-root-noise.txt",
            Attributes = fileAttr,
            Size = 1,
        });
        index.EndBulk();

        var engine = new SearchEngine(index);
        const string parent =
            @"Z:\Users\chaoj\dev\tools\findx\src\FindX.Service\obj\Debug\net8.0-windows";
        var results = engine.Search($"parent:{parent} a", 20);

        Assert.Contains(results, r => r.Name == "apphost.exe");
        Assert.DoesNotContain(results, r => r.Name == "alphabet-root-noise.txt");

        // 多字符纯 ASCII 关键词同样应先走子树路径索引，而非仅单字符
        var resultsApp = engine.Search($"parent:{parent} app", 20);
        Assert.Contains(resultsApp, r => r.Name == "apphost.exe");
    }

    /// <summary>
    /// <c>parent:</c> 路径短于 20 字符时旧逻辑不会走路径针扫描；单字符前缀在全局序下又受 512 帽限制，
    /// 子树解析 + 子树前缀遍历应仍能命中深层文件。
    /// </summary>
    [Fact]
    public void ParentPath_ShortFilter_SingleAscii_SubtreeFindsBeyondGlobalPrefixCap()
    {
        var index = new FileIndex();
        index.BeginBulk();
        const char vol = 'Z';
        const uint dirAttr = 0x10;
        const uint fileAttr = 0x20;
        ulong p = 0;
        p = AddDir(index, vol, 1, p, "p", dirAttr);
        p = AddDir(index, vol, 2, p, "q", dirAttr);
        for (int i = 0; i < 600; i++)
        {
            index.AddEntry(new FileEntry
            {
                VolumeLetter = vol,
                FileRef = (ulong)(10 + i),
                ParentRef = 0,
                Name = $"a{i:D3}-noise.txt",
                Attributes = fileAttr,
                Size = 1,
            });
        }

        index.AddEntry(new FileEntry
        {
            VolumeLetter = vol,
            FileRef = 2000,
            ParentRef = p,
            Name = "azzz-target.txt",
            Attributes = fileAttr,
            Size = 1,
        });
        index.EndBulk();

        var engine = new SearchEngine(index);
        var results = engine.Search(@"parent:Z:\p\q a", 20);
        Assert.Contains(results, r => r.Name == "azzz-target.txt");

        var resultsAz = engine.Search(@"parent:Z:\p\q az", 20);
        Assert.Contains(resultsAz, r => r.Name == "azzz-target.txt");
    }

    private static ulong AddDir(FileIndex index, char vol, ulong fileRef, ulong parentRef, string name, uint attributes)
    {
        index.AddEntry(new FileEntry
        {
            VolumeLetter = vol,
            FileRef = fileRef,
            ParentRef = parentRef,
            Name = name,
            Attributes = attributes,
            Size = 0,
        });
        return fileRef;
    }

    private static SearchEngine BuildEngine()
    {
        var index = new FileIndex();
        AddEntry(index, 1, "普查通知.docx");
        AddEntry(index, 2, "排查清单.xlsx");
        AddEntry(index, 3, "退场申请-普查版.txt");
        AddEntry(index, 4, "pchannel-demo.txt");
        AddEntry(index, 5, "月报汇总.md");
        AddEntry(index, 6, "工人退场确认书.docx");
        AddEntry(index, 7, "aaa你好.txt");
        AddEntry(index, 8, "你知道吗bbb.md");
        AddEntry(index, 9, "12312312$$$111.aa");
        AddEntry(index, 10, "呵呵呵asdfasdf123123h哈哈哈.doc");
        return new SearchEngine(index);
    }

    private static void AddEntry(FileIndex index, ulong fileRef, string name, uint attributes = 0x20)
    {
        index.AddEntry(new FileEntry
        {
            VolumeLetter = 'Z',
            FileRef = fileRef,
            ParentRef = 0,
            Name = name,
            Attributes = attributes,
            Size = 1,
        });
    }
}
