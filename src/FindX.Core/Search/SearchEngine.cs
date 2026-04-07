using FindX.Core.Index;
using FindX.Core.Pinyin;

namespace FindX.Core.Search;

/// <summary>
/// 统一搜索引擎入口。组合排序表二分前缀、拼音匹配、正则、路径过滤等策略。
/// </summary>
public sealed class SearchEngine
{
    private readonly FileIndex _index;

    public SearchEngine(FileIndex index) => _index = index;

    public List<SearchResult> Search(string query, int maxResults = 50, string? pathFilter = null)
    {
        var parsed = QueryParser.Parse(query);
        if (parsed.PathFilter == null && pathFilter != null)
            parsed.PathFilter = pathFilter;

        if (parsed.Keywords.Count == 0 && !parsed.IsRegex)
            return new List<SearchResult>();

        var candidates = GatherCandidates(parsed, maxResults);
        var results = new List<SearchResult>();

        var combinedLower = parsed.IsRegex ? null : string.Join("", parsed.Keywords).ToLowerInvariant();

        foreach (var idx in candidates)
        {
            var entry = _index.GetByIndex(idx);
            if (entry == null) continue;

            if (parsed.ExtFilter != null)
            {
                var ext = Path.GetExtension(entry.Name).TrimStart('.');
                if (!ext.Equals(parsed.ExtFilter, StringComparison.OrdinalIgnoreCase))
                    continue;
            }

            string? fullPath = null;

            if (parsed.PathFilter != null)
            {
                fullPath = _index.BuildFullPath(idx);
                if (!fullPath.StartsWith(parsed.PathFilter, StringComparison.OrdinalIgnoreCase))
                    continue;
            }

            PinyinMatcher.MatchResult matchResult;
            if (parsed.IsRegex && parsed.RegexPattern != null)
            {
                matchResult = parsed.RegexPattern.IsMatch(entry.Name)
                    ? new PinyinMatcher.MatchResult(PinyinMatcher.MatchType.Exact, 500, entry.Name.Length)
                    : PinyinMatcher.MatchResult.NoMatch;
            }
            else
            {
                matchResult = PinyinMatcher.Match(combinedLower!, entry.Name);
            }

            if (!matchResult.IsMatch) continue;

            fullPath ??= _index.BuildFullPath(idx);
            var score = Scorer.Score(entry, fullPath, matchResult);

            results.Add(new SearchResult
            {
                FullPath = fullPath,
                Name = entry.Name,
                IsDirectory = entry.IsDirectory,
                Size = entry.Size,
                Score = score,
                MatchType = matchResult.Type,
                EntryIndex = idx,
            });
        }

        results.Sort((a, b) => b.Score.CompareTo(a.Score));
        if (results.Count > maxResults)
            results.RemoveRange(maxResults, results.Count - maxResults);

        return results;
    }

    private HashSet<int> GatherCandidates(ParsedQuery parsed, int maxResults)
    {
        var candidates = new HashSet<int>();

        if (parsed.IsRegex)
        {
            var pattern = parsed.RegexPattern!;
            var regexCap = maxResults * 10;
            _index.ForEachLiveEntry((entry, i) =>
            {
                if (candidates.Count >= regexCap) return false;
                if (pattern.IsMatch(entry.Name)) candidates.Add(i);
                return true;
            });
            return candidates;
        }

        var cap = FileIndex.PrefixSearchHitCap;
        const int mixCap = 512;

        foreach (var kw in parsed.Keywords)
        {
            var lower = kw.ToLowerInvariant();

            foreach (var h in _index.SearchNamePrefix(lower, cap))
                candidates.Add(h);

            if (!lower.All(c => char.IsAsciiLetterOrDigit(c)))
                continue;

            foreach (var h in _index.SearchPinyinInitialsPrefix(lower, cap))
                candidates.Add(h);
            foreach (var h in _index.SearchFullPinyinCompactPrefix(lower, cap))
                candidates.Add(h);

            // 递进首字母前缀：2..min(len-1, 4)
            // mchunt → "mc","mch","mchu"；其中 "mc" 命中 initials="mct"（马春天）
            int pyInitMax = Math.Min(lower.Length - 1, 4);
            for (int plen = pyInitMax; plen >= 2; plen--)
            {
                foreach (var h in _index.SearchPinyinInitialsPrefix(lower[..plen], mixCap))
                    candidates.Add(h);
            }

            // 递进全拼前缀：2..min(len-1, 6)
            int fullPyMax = Math.Min(lower.Length - 1, 6);
            for (int plen = fullPyMax; plen >= 2; plen--)
            {
                foreach (var h in _index.SearchFullPinyinCompactPrefix(lower[..plen], mixCap))
                    candidates.Add(h);
            }

            // 跨音节三字首字母组合：遍历查询中所有可能的 (P, Q) 位置对，
            // P = 第二个 CJK 字起始位置，Q = 第三个 CJK 字起始位置，
            // 组成三字首字母前缀搜索。三字前缀足够精确（如 "mct" 几乎仅命中 CJK），
            // 用小 cap 避免 "min"/"mac" 等高频 ASCII 前缀带来的候选集爆炸。
            // mact → (P=2,Q=3): "mct" → 命中 initials="mct"（马春天）
            // mchunt → (P=1,Q=5): "mct" → 命中 initials="mct"（马春天）
            if (lower.Length >= 3)
            {
                char first = lower[0];
                int pMax = Math.Min(6, lower.Length - 2);
                for (int p = 1; p <= pMax; p++)
                {
                    if (lower[p] is < 'a' or > 'z') continue;
                    int qMax = Math.Min(p + 6, lower.Length - 1);
                    for (int q = p + 1; q <= qMax; q++)
                    {
                        if (lower[q] is < 'a' or > 'z') continue;
                        foreach (var h in _index.SearchPinyinInitialsPrefix(
                            $"{first}{lower[p]}{lower[q]}", mixCap))
                            candidates.Add(h);
                    }
                }
            }
        }

        return candidates;
    }
}
