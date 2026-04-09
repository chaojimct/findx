using FindX.Core.Index;
using FindX.Core.Pinyin;

namespace FindX.Core.Search;

/// <summary>
/// 统一搜索引擎入口。组合排序表二分前缀、拼音匹配、正则、路径过滤等策略。
/// 支持 Everything 兼容的 AST 表达式树求值。
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

        bool hasTerms = parsed.Keywords.Count > 0;
        bool hasRegex = parsed.IsRegex;
        bool hasFilters = parsed.HasFilters;

        // 纯 filter 查询（如 "ext:cs size:>1mb"）没有搜索词，需全量扫描
        bool filterOnlyQuery = !hasTerms && !hasRegex && hasFilters;

        if (!hasTerms && !hasRegex && !hasFilters)
            return new List<SearchResult>();

        int effectiveMax = parsed.MaxCount.HasValue
            ? Math.Min(parsed.MaxCount.Value, maxResults)
            : maxResults;

        var candidates = GatherCandidates(parsed, effectiveMax, filterOnlyQuery);
        var results = new List<SearchResult>();
        var seenPaths = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var evalCtx = new EvalContext();
        int scoreBudget = Math.Max(effectiveMax * 5, 2000);

        foreach (var idx in candidates)
        {
            if (results.Count >= scoreBudget) break;

            var entry = _index.GetByIndex(idx);
            if (entry == null) continue;

            string fullPath = _index.BuildFullPath(idx);
            if (!seenPaths.Add(fullPath)) continue;
            evalCtx.Reset(entry, fullPath);

            if (parsed.Root != null)
            {
                if (!parsed.Root.Match(evalCtx))
                    continue;
            }
            else
            {
                if (!LegacyFilter(parsed, entry, fullPath))
                    continue;
            }

            PinyinMatcher.MatchResult matchResult;
            if (hasTerms)
            {
                if (parsed.Root != null)
                {
                    int totalScore = 0;
                    int totalLen = 0;
                    var bestType = PinyinMatcher.MatchType.None;

                    foreach (var kw in parsed.Keywords)
                    {
                        var mr = PinyinMatcher.Match(kw.ToLowerInvariant(), entry.Name);
                        if (mr.IsMatch)
                        {
                            totalScore += mr.Score;
                            totalLen += mr.MatchedChars;
                            if (mr.Type > bestType) bestType = mr.Type;
                        }
                    }

                    matchResult = totalScore > 0
                        ? new PinyinMatcher.MatchResult(bestType, totalScore, totalLen)
                        : new PinyinMatcher.MatchResult(PinyinMatcher.MatchType.None, 10, 0);
                }
                else
                {
                    var combinedLower = string.Join("", parsed.Keywords).ToLowerInvariant();
                    matchResult = PinyinMatcher.Match(combinedLower, entry.Name);
                    if (!matchResult.IsMatch && !hasFilters)
                        continue;
                    if (!matchResult.IsMatch)
                        matchResult = new PinyinMatcher.MatchResult(PinyinMatcher.MatchType.None, 0, 0);
                }
            }
            else if (hasRegex && parsed.RegexPattern != null)
            {
                matchResult = parsed.RegexPattern.IsMatch(entry.Name)
                    ? new PinyinMatcher.MatchResult(PinyinMatcher.MatchType.Exact, 500, entry.Name.Length)
                    : PinyinMatcher.MatchResult.NoMatch;
                if (!matchResult.IsMatch)
                    continue;
            }
            else
            {
                matchResult = new PinyinMatcher.MatchResult(PinyinMatcher.MatchType.Exact, 100, 0);
            }

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
                LastWriteUtcTicks = entry.LastWriteTimeTicks,
                LastModified = entry.LastWriteTimeTicks > 0
                    ? new DateTime(entry.LastWriteTimeTicks, DateTimeKind.Utc).ToLocalTime()
                    : default,
            });
        }

        results.Sort((a, b) => b.Score.CompareTo(a.Score));
        if (results.Count > effectiveMax)
            results.RemoveRange(effectiveMax, results.Count - effectiveMax);

        return results;
    }

    /// <summary>向后兼容：无 AST 时的旧过滤逻辑</summary>
    private static bool LegacyFilter(ParsedQuery parsed, FileEntry entry, string fullPath)
    {
        if (parsed.ExtFilter != null)
        {
            var ext = Path.GetExtension(entry.Name).TrimStart('.');
            if (!ext.Equals(parsed.ExtFilter, StringComparison.OrdinalIgnoreCase))
                return false;
        }

        if (parsed.PathFilter != null)
        {
            if (!fullPath.StartsWith(parsed.PathFilter, StringComparison.OrdinalIgnoreCase))
                return false;
        }

        return true;
    }

    private HashSet<int> GatherCandidates(ParsedQuery parsed, int maxResults, bool filterOnly)
    {
        var candidates = new HashSet<int>();

        if (_index.IsInBulkLoad)
            return candidates;

        if (filterOnly)
        {
            var scanCap = maxResults * 20;
            _index.ForEachLiveEntry((entry, i) =>
            {
                if (candidates.Count >= scanCap) return false;
                candidates.Add(i);
                return true;
            });
            return candidates;
        }

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

        var fullCap = FileIndex.PrefixSearchHitCap;
        const int mixCap = 512;

        int minAsciiKwLen = int.MaxValue;
        foreach (var kw in parsed.Keywords)
        {
            bool ascii = true;
            foreach (var c in kw)
                if (!char.IsAsciiLetterOrDigit(c)) { ascii = false; break; }
            if (ascii && kw.Length < minAsciiKwLen)
                minAsciiKwLen = kw.Length;
        }

        bool shortAscii = minAsciiKwLen <= 1;
        var cap = shortAscii ? Math.Min(512, fullCap) : fullCap;

        foreach (var kw in parsed.Keywords)
        {
            var lower = kw.ToLowerInvariant();

            foreach (var h in _index.SearchNamePrefix(lower, cap))
                candidates.Add(h);

            if (!lower.All(c => char.IsAsciiLetterOrDigit(c)))
                continue;

            if (shortAscii && lower.Length <= 1) continue;

            foreach (var h in _index.SearchPinyinInitialsPrefix(lower, cap))
                candidates.Add(h);
            foreach (var h in _index.SearchFullPinyinCompactPrefix(lower, cap))
                candidates.Add(h);

            int pyInitMax = Math.Min(lower.Length - 1, 4);
            for (int plen = pyInitMax; plen >= 2; plen--)
            {
                foreach (var h in _index.SearchPinyinInitialsPrefix(lower[..plen], mixCap))
                    candidates.Add(h);
            }

            int fullPyMax = Math.Min(lower.Length - 1, 6);
            for (int plen = fullPyMax; plen >= 2; plen--)
            {
                foreach (var h in _index.SearchFullPinyinCompactPrefix(lower[..plen], mixCap))
                    candidates.Add(h);
            }

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

        // ── 拼音子串补充扫描 ──
        bool hasAsciiKw = false;
        foreach (var kw in parsed.Keywords)
        {
            if (kw.Length < 2) continue;
            bool ok = true;
            foreach (var c in kw)
                if (!char.IsAsciiLetterOrDigit(c)) { ok = false; break; }
            if (ok) { hasAsciiKw = true; break; }
        }

        if (hasAsciiKw)
            GatherPinyinSubstringCandidates(parsed.Keywords, candidates, cap);

        // ── CJK 子串补充扫描 ──
        // SearchNamePrefix 只能命中文件名以关键词开头的条目；
        // 搜索 "退场" 找不到 "工人退场确认书.docx"，需要子串匹配补充。
        bool hasCjkKw = false;
        foreach (var kw in parsed.Keywords)
        {
            foreach (var c in kw)
                if (c >= '\u4E00' && c <= '\u9FFF') { hasCjkKw = true; break; }
            if (hasCjkKw) break;
        }

        if (hasCjkKw)
            GatherCjkSubstringCandidates(parsed.Keywords, candidates, cap);

        return candidates;
    }

    private void GatherCjkSubstringCandidates(
        IReadOnlyList<string> keywords, HashSet<int> candidates, int addCap)
    {
        if (_index.IsInBulkLoad) return;

        foreach (var kw in keywords)
        {
            bool hasCjk = false;
            foreach (var c in kw)
                if (c >= '\u4E00' && c <= '\u9FFF') { hasCjk = true; break; }
            if (!hasCjk) continue;

            foreach (var h in _index.SearchNameContains(kw, addCap))
                candidates.Add(h);
        }
    }

    private void GatherPinyinSubstringCandidates(
        IReadOnlyList<string> keywords, HashSet<int> candidates, int addCap)
    {
        if (_index.IsInBulkLoad) return;

        foreach (var kw in keywords)
        {
            if (kw.Length < 2) continue;
            bool allAscii = true;
            foreach (var c in kw)
                if (!char.IsAsciiLetterOrDigit(c)) { allAscii = false; break; }
            if (!allAscii) continue;

            var kwLower = kw.ToLowerInvariant();

            foreach (var h in _index.SearchFullPinyinContains(kwLower, addCap))
                candidates.Add(h);

            if (kwLower.Length <= 5)
            {
                foreach (var h in _index.SearchInitialsContains(kwLower, addCap))
                    candidates.Add(h);
            }
        }
    }
}
