using FindX.Core.Index;
using FindX.Core.Pinyin;

namespace FindX.Core.Search;

/// <summary>
/// 搜索结果评分器。综合匹配类型、路径深度、名称长度、时间权重等因素。
/// </summary>
public static class Scorer
{
    public static int Score(FileEntry entry, string fullPath, PinyinMatcher.MatchResult match)
    {
        int score = match.Score;

        int depth = 0;
        foreach (var ch in fullPath)
            if (ch is '\\' or '/') depth++;
        score -= depth * 2;

        score -= entry.Name.Length;

        if (entry.IsDirectory)
            score += 5;

        return score;
    }
}

public sealed class SearchResult
{
    public required string FullPath { get; init; }
    public required string Name { get; init; }
    public required bool IsDirectory { get; init; }
    public required long Size { get; init; }
    public required int Score { get; init; }
    public required PinyinMatcher.MatchType MatchType { get; init; }
    public int EntryIndex { get; init; }
    /// <summary>索引中的最后写入时间（UTC ticks，0 表示未知）</summary>
    public long LastWriteUtcTicks { get; init; }
    public DateTime LastModified { get; init; }
}
