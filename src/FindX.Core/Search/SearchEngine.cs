using System.Linq;
using FindX.Core.FileSystem;
using FindX.Core.Index;
using FindX.Core.Pinyin;

namespace FindX.Core.Search;

/// <summary>
/// 统一搜索引擎入口。组合排序表二分前缀、拼音匹配、正则、路径过滤等策略。
/// 支持 Everything 兼容的 AST 表达式树求值。
/// </summary>
public sealed class SearchEngine
{
    private readonly record struct CandidateHit(
        int EntryIndex,
        FileEntry Entry,
        int Score,
        PinyinMatcher.MatchType MatchType);

    private static readonly string[] PinyinInitials =
    [
        "zh", "ch", "sh",
        "b", "p", "m", "f", "d", "t", "n", "l",
        "g", "k", "h", "j", "q", "x", "r", "z", "c", "s", "y", "w"
    ];

    private static readonly HashSet<string> PinyinFinals = new(StringComparer.Ordinal)
    {
        "a", "ai", "an", "ang", "ao",
        "e", "ei", "en", "eng", "er",
        "i", "ia", "ian", "iang", "iao", "ie", "in", "ing", "iong", "iu",
        "o", "ong", "ou",
        "u", "ua", "uai", "uan", "uang", "ue", "ui", "un", "uo",
        "v", "van", "ve", "vn"
    };

    private readonly FileIndex _index;
    private readonly Func<SearchPreferences>? _getPreferences;

    public SearchEngine(FileIndex index, Func<SearchPreferences>? getPreferences = null)
    {
        _index = index;
        _getPreferences = getPreferences;
    }

    public List<SearchResult> Search(string query, int maxResults = 50, string? pathFilter = null)
    {
        var parsed = QueryParser.Parse(query);
        if (parsed.PathFilter == null && pathFilter != null)
            parsed.PathFilter = pathFilter;

        bool hasTerms = parsed.Keywords.Count > 0;
        bool hasRegex = parsed.IsRegex;
        bool hasFilters = parsed.HasFilters;
        bool needsMetadataForFilters = parsed.HasMetadataFilters;
        bool needsFullPathForFiltering = QueryNeedsFullPath(parsed);
        bool needsPathDepthForFiltering = QueryNeedsPathDepth(parsed);

        // 纯 filter 查询（如 "ext:cs size:>1mb"）没有搜索词，需全量扫描
        bool filterOnlyQuery = !hasTerms && !hasRegex && hasFilters;

        if (!hasTerms && !hasRegex && !hasFilters)
            return new List<SearchResult>();

        int effectiveMax = parsed.MaxCount.HasValue
            ? Math.Min(parsed.MaxCount.Value, maxResults)
            : maxResults;
        var preferences = _getPreferences?.Invoke() ?? new SearchPreferences();
        var preparedKeywords = parsed.Keywords
            .Select(PinyinMatcher.Prepare)
            .ToArray();
        var preparedCombined = hasTerms
            ? PinyinMatcher.Prepare(string.Concat(parsed.Keywords))
            : default;
        bool preferPinyinForAsciiQuery = preferences.PreferPinyinForAsciiQueries
            && hasTerms
            && KeywordsAreAsciiAlnum(parsed.Keywords);

        var candidates = GatherCandidates(parsed, effectiveMax, filterOnlyQuery);
        var hits = new List<CandidateHit>();
        var seenIndices = new HashSet<int>();
        var evalCtx = new EvalContext();
        int scoreBudget = Math.Max(effectiveMax * 5, 2000);

        foreach (var idx in candidates)
        {
            if (hits.Count >= scoreBudget) break;

            var entry = _index.GetByIndex(idx);
            if (entry == null) continue;
            if (!seenIndices.Add(idx)) continue;

            if (TryEvaluateCandidate(
                    parsed,
                    idx,
                    entry,
                    hasTerms,
                    hasFilters,
                    hasRegex,
                    needsMetadataForFilters,
                    needsFullPathForFiltering,
                    needsPathDepthForFiltering,
                    preparedKeywords,
                    preparedCombined,
                    preferPinyinForAsciiQuery,
                    evalCtx,
                    out var hit))
            {
                hits.Add(hit);
            }
        }

        if (hits.Count == 0)
        {
            int fallbackPool = Math.Min(FileIndex.PrefixSearchHitCap, Math.Max(effectiveMax * 20, 256));
            foreach (var keyword in parsed.Keywords)
            {
                if (keyword.Length < 5 || !IsAsciiAlnum(keyword))
                    continue;

                foreach (var idx in _index.SearchMatchQuery(keyword.ToLowerInvariant(), fallbackPool))
                {
                    if (hits.Count >= scoreBudget) break;

                    var entry = _index.GetByIndex(idx);
                    if (entry == null) continue;
                    if (!seenIndices.Add(idx)) continue;

                    var preparedKeyword = PinyinMatcher.Prepare(keyword);
                    if (TryEvaluateCandidate(
                            parsed,
                            idx,
                            entry,
                            hasTerms,
                            hasFilters,
                            hasRegex,
                            needsMetadataForFilters,
                            needsFullPathForFiltering,
                            needsPathDepthForFiltering,
                            [preparedKeyword],
                            preparedKeyword,
                            preferPinyinForAsciiQuery,
                            evalCtx,
                            out var hit))
                    {
                        hits.Add(hit);
                    }
                }

                if (hits.Count > 0)
                    break;
            }
        }

        hits.Sort((a, b) => b.Score.CompareTo(a.Score));
        if (hits.Count > effectiveMax)
            hits.RemoveRange(effectiveMax, hits.Count - effectiveMax);

        return MaterializeResults(hits, needsMetadataForFilters);
    }

    /// <summary>向后兼容：无 AST 时的旧过滤逻辑</summary>
    private bool TryEvaluateCandidate(
        ParsedQuery parsed,
        int idx,
        FileEntry entry,
        bool hasTerms,
        bool hasFilters,
        bool hasRegex,
        bool needsMetadataForFilters,
        bool needsFullPathForFiltering,
        bool needsPathDepthForFiltering,
        IReadOnlyList<PinyinMatcher.PreparedQuery> preparedKeywords,
        PinyinMatcher.PreparedQuery preparedCombined,
        bool preferPinyinForAsciiQuery,
        EvalContext evalCtx,
        out CandidateHit hit)
    {
        int pathDepth = _index.GetPathDepth(idx);
        if (pathDepth < 0)
        {
            hit = default;
            return false;
        }

        string fullPath = string.Empty;
        if (needsFullPathForFiltering || needsMetadataForFilters)
        {
            fullPath = _index.BuildFullPath(idx);
            if (string.IsNullOrEmpty(fullPath))
            {
                hit = default;
                return false;
            }
        }

        if (needsMetadataForFilters)
            entry = MaybeHydrateMetadata(entry, fullPath);

        evalCtx.Reset(entry, fullPath, pathDepth);

        if (parsed.Root != null)
        {
            if (!parsed.Root.Match(evalCtx))
            {
                hit = default;
                return false;
            }
        }
        else if (!LegacyFilter(parsed, entry, fullPath))
        {
            hit = default;
            return false;
        }

        PinyinMatcher.MatchResult matchResult;
        if (hasTerms)
        {
            if (parsed.Root != null)
            {
                int totalScore = 0;
                int totalLen = 0;
                var bestType = PinyinMatcher.MatchType.None;

                foreach (var preparedKeyword in preparedKeywords)
                {
                    var mr = PinyinMatcher.Match(preparedKeyword, entry.Name);
                    if (!mr.IsMatch)
                        continue;

                    totalScore += mr.Score;
                    totalLen += mr.MatchedChars;
                    if (mr.Type > bestType) bestType = mr.Type;
                }

                matchResult = totalScore > 0
                    ? new PinyinMatcher.MatchResult(bestType, totalScore, totalLen)
                    : new PinyinMatcher.MatchResult(PinyinMatcher.MatchType.None, 10, 0);
            }
            else
            {
                matchResult = PinyinMatcher.Match(preparedCombined, entry.Name);
                if (!matchResult.IsMatch && !hasFilters)
                {
                    hit = default;
                    return false;
                }

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
            {
                hit = default;
                return false;
            }
        }
        else
        {
            matchResult = new PinyinMatcher.MatchResult(PinyinMatcher.MatchType.Exact, 100, 0);
        }

        hit = new CandidateHit(
            idx,
            entry,
            Scorer.Score(entry, pathDepth, matchResult, preferPinyinForAsciiQuery),
            matchResult.Type);
        return true;
    }

    private List<SearchResult> MaterializeResults(List<CandidateHit> hits, bool alreadyHydrated)
    {
        var results = new List<SearchResult>(hits.Count);
        foreach (var hit in hits)
        {
            var entry = hit.Entry;
            string fullPath = _index.BuildFullPath(hit.EntryIndex);
            if (string.IsNullOrEmpty(fullPath))
                continue;

            if (!alreadyHydrated)
                entry = MaybeHydrateMetadata(entry, fullPath);

            results.Add(new SearchResult
            {
                FullPath = fullPath,
                Name = entry.Name,
                IsDirectory = entry.IsDirectory,
                Size = entry.Size,
                Score = hit.Score,
                MatchType = hit.MatchType,
                EntryIndex = hit.EntryIndex,
                LastWriteUtcTicks = entry.LastWriteTimeTicks,
                LastModified = entry.LastWriteTimeTicks > 0
                    ? new DateTime(entry.LastWriteTimeTicks, DateTimeKind.Utc).ToLocalTime()
                    : default,
            });
        }

        return results;
    }

    private static bool QueryNeedsFullPath(ParsedQuery parsed)
        => parsed.PathFilter != null || QueryNodeNeedsFullPath(parsed.Root);

    private static bool QueryNeedsPathDepth(ParsedQuery parsed)
        => QueryNodeNeedsPathDepth(parsed.Root);

    private static bool QueryNodeNeedsFullPath(QueryNode? node)
    {
        return node switch
        {
            null => false,
            FilterNode filter => filter.Type is FilterType.Path or FilterType.NoPath,
            AndNode andNode => andNode.Children.Any(QueryNodeNeedsFullPath),
            OrNode orNode => orNode.Children.Any(QueryNodeNeedsFullPath),
            NotNode notNode => QueryNodeNeedsFullPath(notNode.Child),
            _ => false,
        };
    }

    private static bool QueryNodeNeedsPathDepth(QueryNode? node)
    {
        return node switch
        {
            null => false,
            FilterNode filter => filter.Type is FilterType.Path or FilterType.NoPath or FilterType.Depth or FilterType.Root,
            AndNode andNode => andNode.Children.Any(QueryNodeNeedsPathDepth),
            OrNode orNode => orNode.Children.Any(QueryNodeNeedsPathDepth),
            NotNode notNode => QueryNodeNeedsPathDepth(notNode.Child),
            _ => false,
        };
    }

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

    private static bool KeywordsAreAsciiAlnum(IReadOnlyList<string> keywords)
    {
        if (keywords.Count == 0)
            return false;

        foreach (var keyword in keywords)
        {
            if (keyword.Length == 0)
                return false;
            foreach (var ch in keyword)
            {
                if (!char.IsAsciiLetterOrDigit(ch))
                    return false;
            }
        }

        return true;
    }

    internal static bool IsAsciiAlnum(string value)
    {
        if (value.Length == 0)
            return false;

        foreach (var ch in value)
        {
            if (!char.IsAsciiLetterOrDigit(ch))
                return false;
        }

        return true;
    }

    internal static bool ShouldUsePinyinSubstringExpansion(IReadOnlyList<string> keywords)
    {
        foreach (var keyword in keywords)
        {
            if (keyword.Length < 3)
                continue;
            if (IsAsciiAlnum(keyword))
                return true;
        }

        return false;
    }

    internal static bool ShouldUseShortAsciiInitialsExpansion(IReadOnlyList<string> keywords)
    {
        foreach (var keyword in keywords)
        {
            if (keyword.Length != 2)
                continue;
            if (IsAsciiAlnum(keyword))
                return true;
        }

        return false;
    }

    internal static bool TryBuildAsciiPinyinInitialsAnchor(string keyword, out string anchor)
    {
        anchor = string.Empty;
        if (keyword.Length < 4 || !IsAsciiAlnum(keyword))
            return false;

        Span<char> buffer = stackalloc char[Math.Min(keyword.Length, 12)];
        int count = 0;
        var lower = keyword.ToLowerInvariant().AsSpan();
        int pos = 0;
        while (pos < lower.Length && count < buffer.Length)
        {
            int len = TryConsumePinyinTokenLength(lower[pos..]);
            if (len <= 0)
                break;

            buffer[count++] = lower[pos];
            pos += len;
        }

        if (count < 2)
            return false;

        anchor = new string(buffer[..count]);
        return anchor.Length >= 2 && anchor.Length < keyword.Length;
    }

    internal static bool TryBuildAsciiPinyinTailToken(string keyword, out string tail)
    {
        tail = string.Empty;
        if (keyword.Length < 4 || !IsAsciiAlnum(keyword))
            return false;

        var lower = keyword.ToLowerInvariant().AsSpan();
        int pos = 0;
        string? lastFullToken = null;
        while (pos < lower.Length)
        {
            int len = TryConsumePinyinTokenLength(lower[pos..]);
            if (len <= 0)
                break;

            var token = lower[pos..(pos + len)];
            if (len >= 2 && IsValidPinyinSyllable(token))
                lastFullToken = token.ToString();
            pos += len;
        }

        if (string.IsNullOrEmpty(lastFullToken))
            return false;

        tail = lastFullToken;
        return tail.Length >= 2 && tail.Length < keyword.Length;
    }

    private static bool IsAsciiVowel(char ch)
        => ch is 'a' or 'e' or 'i' or 'o' or 'u' or 'v';

    private static int TryConsumePinyinTokenLength(ReadOnlySpan<char> text)
    {
        int max = Math.Min(text.Length, 6);
        for (int len = max; len >= 1; len--)
        {
            var token = text[..len];
            if (IsValidPinyinSyllable(token))
                return len;
        }

        if (text.Length >= 2 && text[0] is 'z' or 'c' or 's' && text[1] == 'h')
            return 2;
        return char.IsAsciiLetter(text[0]) ? 1 : 0;
    }

    private static bool IsValidPinyinSyllable(ReadOnlySpan<char> token)
    {
        if (token.IsEmpty)
            return false;

        var syllable = token.ToString();
        if (PinyinFinals.Contains(syllable))
            return true;

        foreach (var initial in PinyinInitials)
        {
            if (!syllable.StartsWith(initial, StringComparison.Ordinal))
                continue;
            if (syllable.Length == initial.Length)
                continue;

            var final = syllable[initial.Length..];
            if (PinyinFinals.Contains(final))
                return true;
        }

        return false;
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
            if (IsAsciiAlnum(kw) && kw.Length < minAsciiKwLen)
                minAsciiKwLen = kw.Length;
        }

        bool shortAscii = minAsciiKwLen <= 1;
        var cap = shortAscii ? Math.Min(512, fullCap) : fullCap;

        foreach (var kw in parsed.Keywords)
        {
            var lower = kw.ToLowerInvariant();

            foreach (var h in _index.SearchNamePrefix(lower, cap))
                candidates.Add(h);

            if (!IsAsciiAlnum(lower))
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
        if (ShouldUsePinyinSubstringExpansion(parsed.Keywords))
        {
            int substringCap = Math.Min(cap, 1024);
            GatherPinyinSubstringCandidates(parsed.Keywords, candidates, substringCap);
        }
        else if (ShouldUseShortAsciiInitialsExpansion(parsed.Keywords))
        {
            int target = Math.Max(maxResults * 2, 64);
            if (candidates.Count < target)
            {
                int shortfall = target - candidates.Count;
                int addCap = Math.Clamp(shortfall, 64, 256);
                GatherShortAsciiInitialsCandidates(parsed.Keywords, candidates, addCap);
            }
        }

        int mixedTarget = Math.Max(maxResults, 32);
        if (candidates.Count < mixedTarget)
        {
            int shortfall = mixedTarget - candidates.Count;
            int addCap = Math.Clamp(shortfall * 8, 256, 2048);
            GatherMixedAsciiAnchorCandidates(parsed.Keywords, candidates, addCap);
        }

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
            if (kw.Length < 3 || !IsAsciiAlnum(kw))
                continue;

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

    private void GatherShortAsciiInitialsCandidates(
        IReadOnlyList<string> keywords, HashSet<int> candidates, int addCap)
    {
        if (_index.IsInBulkLoad) return;

        foreach (var kw in keywords)
        {
            if (kw.Length != 2 || !IsAsciiAlnum(kw))
                continue;

            var kwLower = kw.ToLowerInvariant();
            foreach (var h in _index.SearchInitialsContains(kwLower, addCap))
                candidates.Add(h);
        }
    }

    private void GatherMixedAsciiAnchorCandidates(
        IReadOnlyList<string> keywords, HashSet<int> candidates, int addCap)
    {
        if (_index.IsInBulkLoad) return;

        foreach (var kw in keywords)
        {
            if (!TryBuildAsciiPinyinInitialsAnchor(kw, out var anchor))
                continue;

            int beforeCount = candidates.Count;
            if (TryBuildAsciiPinyinTailToken(kw, out var tail))
            {
                var initialsHits = _index.SearchInitialsContains(anchor, addCap * 2);
                var tailHits = _index.SearchFullPinyinContains(tail, addCap * 2);
                if (initialsHits.Count > 0 && tailHits.Count > 0)
                {
                    var tailSet = new HashSet<int>(tailHits);
                    int added = 0;
                    foreach (var h in initialsHits)
                    {
                        if (!tailSet.Contains(h))
                            continue;
                        candidates.Add(h);
                        added++;
                        if (added >= addCap)
                            break;
                    }
                    if (added > 0)
                        continue;
                }
            }

            if (kw.Length >= 5 && candidates.Count == beforeCount)
            {
                foreach (var h in _index.SearchFullPinyinFuzzy(kw.ToLowerInvariant(), addCap))
                    candidates.Add(h);
                if (candidates.Count > beforeCount)
                    continue;
            }

            foreach (var h in _index.SearchInitialsContains(anchor, addCap))
                candidates.Add(h);
        }
    }

    private FileEntry MaybeHydrateMetadata(FileEntry entry, string fullPath)
    {
        if (!FileMetadataReader.NeedsHydration(entry))
            return entry;
        if (!FileMetadataReader.TryHydrate(fullPath, entry, out var hydrated))
            return entry;

        if (hydrated.Size == entry.Size
            && hydrated.LastWriteTimeTicks == entry.LastWriteTimeTicks
            && hydrated.CreationTimeTicks == entry.CreationTimeTicks
            && hydrated.AccessTimeTicks == entry.AccessTimeTicks
            && hydrated.Attributes == entry.Attributes)
            return hydrated;

        _index.UpsertEntry(hydrated);
        return hydrated;
    }

    private void HydrateFinalResults(List<SearchResult> results)
    {
        for (int i = 0; i < results.Count; i++)
        {
            var result = results[i];
            if (result.Size > 0 && result.LastWriteUtcTicks > 0)
                continue;

            var entry = _index.GetByIndex(result.EntryIndex);
            if (entry == null)
                continue;

            var hydrated = MaybeHydrateMetadata(entry, result.FullPath);
            if (hydrated.Size == result.Size && hydrated.LastWriteTimeTicks == result.LastWriteUtcTicks)
                continue;

            results[i] = new SearchResult
            {
                FullPath = result.FullPath,
                Name = hydrated.Name,
                IsDirectory = hydrated.IsDirectory,
                Size = hydrated.Size,
                Score = result.Score,
                MatchType = result.MatchType,
                EntryIndex = result.EntryIndex,
                LastWriteUtcTicks = hydrated.LastWriteTimeTicks,
                LastModified = hydrated.LastWriteTimeTicks > 0
                    ? new DateTime(hydrated.LastWriteTimeTicks, DateTimeKind.Utc).ToLocalTime()
                    : default,
            };
        }
    }
}
