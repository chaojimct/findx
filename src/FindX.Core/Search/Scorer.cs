using FindX.Core.Index;
using FindX.Core.Pinyin;

namespace FindX.Core.Search;

/// <summary>
/// 搜索结果评分器。综合匹配类型、路径深度、名称长度、时间权重等因素。
/// </summary>
public static class Scorer
{
    public static int Score(FileEntry entry, int pathDepth, PinyinMatcher.MatchResult match,
        bool preferPinyinForAsciiQuery = false)
    {
        int score = match.Score;

        if (preferPinyinForAsciiQuery && NameContainsCjk(entry.Name))
        {
            score += match.Type switch
            {
                PinyinMatcher.MatchType.FullPinyin => 340,
                PinyinMatcher.MatchType.Initials => 280,
                PinyinMatcher.MatchType.Mixed => 420,
                _ => 0,
            };

            var ext = Path.GetExtension(entry.Name);
            if (IsPreferredDocumentExtension(ext))
                score += 170;
            else if (IsLowValueAsciiPinyinExtension(ext))
                score -= 90;

            if (entry.IsDirectory)
                score -= 35;
        }

        score -= pathDepth * 2;

        score -= entry.Name.Length;

        if (entry.IsDirectory)
            score += 5;

        return score;
    }

    private static bool NameContainsCjk(string name)
    {
        foreach (var ch in name)
        {
            if (ch is >= '\u4E00' and <= '\u9FFF')
                return true;
        }
        return false;
    }

    private static bool IsPreferredDocumentExtension(string ext)
    {
        return ext.Equals(".doc", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".docx", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".pdf", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".xls", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".xlsx", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".ppt", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".pptx", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".txt", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".md", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".csv", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".rtf", StringComparison.OrdinalIgnoreCase);
    }

    private static bool IsLowValueAsciiPinyinExtension(string ext)
    {
        return ext.Equals(".lnk", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".png", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".jpg", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".jpeg", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".gif", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".webp", StringComparison.OrdinalIgnoreCase)
               || ext.Equals(".ico", StringComparison.OrdinalIgnoreCase);
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
