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

        var candidates = GatherCandidates(parsed, maxResults * 10);
        var results = new List<SearchResult>();

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
                var combined = string.Join("", parsed.Keywords);
                matchResult = PinyinMatcher.Match(combined, entry.Name);
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

    private HashSet<int> GatherCandidates(ParsedQuery parsed, int limit)
    {
        var candidates = new HashSet<int>();

        if (parsed.IsRegex)
        {
            var pattern = parsed.RegexPattern!;
            _index.ForEachLiveEntry((entry, i) =>
            {
                if (candidates.Count >= limit) return false;
                if (pattern.IsMatch(entry.Name)) candidates.Add(i);
                return true;
            });
            return candidates;
        }

        foreach (var kw in parsed.Keywords)
        {
            var lower = kw.ToLowerInvariant();

            var nameHits = _index.SearchNamePrefix(lower, limit);
            foreach (var h in nameHits) candidates.Add(h);

            bool hasAscii = lower.All(c => char.IsAsciiLetterOrDigit(c));
            if (hasAscii)
            {
                var pinyinHits = _index.SearchPinyinInitialsPrefix(lower, limit);
                foreach (var h in pinyinHits) candidates.Add(h);
                var fullPyHits = _index.SearchFullPinyinCompactPrefix(lower, limit);
                foreach (var h in fullPyHits) candidates.Add(h);
            }
        }

        return candidates;
    }
}
