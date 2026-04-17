namespace FindX.Core.Search;

public sealed class SearchPreferences
{
    public bool PreferPinyinForAsciiQueries { get; set; } = true;

    /// <summary>
    /// 为 true 时，在展示/返回搜索结果前按需从磁盘补全 Size、最后写入时间等（由索引判定是否需要）。
    /// 关闭可减少对命中路径的 FileInfo 访问，但 CLI/搜索窗口可能显示「未知」大小或日期。
    /// </summary>
    public bool HydrateSearchResultMetadata { get; set; } = true;

    public SearchPreferences Clone() => new()
    {
        PreferPinyinForAsciiQueries = PreferPinyinForAsciiQueries,
        HydrateSearchResultMetadata = HydrateSearchResultMetadata,
    };
}
