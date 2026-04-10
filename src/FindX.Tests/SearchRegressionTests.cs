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
    public void Tchang_FindsRetreatDocuments()
    {
        var engine = BuildEngine();

        var results = engine.Search("tchang", 10);

        Assert.Contains(results, r => r.Name == "工人退场确认书.docx");
        Assert.Contains(results, r => r.Name == "退场申请-普查版.txt");
    }

    [Fact]
    public void Yuebao_FindsMonthlyReportDocuments()
    {
        var engine = BuildEngine();

        var results = engine.Search("yuebao", 10);

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
