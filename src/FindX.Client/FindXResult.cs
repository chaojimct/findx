namespace FindX.Client;

public sealed class FindXSearchResult
{
    public List<FindXResultItem> Items { get; set; } = new();
    public int TotalCount { get; set; }
    public double ElapsedMs { get; set; }
}

public sealed class FindXResultItem
{
    public string Path { get; set; } = "";
    public string Name { get; set; } = "";
    public bool IsDir { get; set; }
    public long Size { get; set; }
    public int Score { get; set; }
}

public sealed class FindXStatus
{
    public int FileCount { get; set; }
    public double MemoryMb { get; set; }
    /// <summary>false 表示全量扫描/加载仍在进行，此时文件数会持续增加。</summary>
    public bool IndexReady { get; set; } = true;
}
