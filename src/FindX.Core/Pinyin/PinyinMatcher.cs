namespace FindX.Core.Pinyin;

/// <summary>
/// 拼音混合匹配器：使用 DP 同时支持全拼、首字母、混合模式。
/// 输入 "zhongw" 可匹配 "中文"（zhong=全拼 + w=首字母）。
/// </summary>
public static class PinyinMatcher
{
    public enum MatchType
    {
        None = 0,
        Initials = 1,
        FullPinyin = 2,
        Mixed = 3,
        Exact = 4,
        Prefix = 5,
    }

    public readonly struct MatchResult
    {
        public readonly MatchType Type;
        public readonly int Score;
        public readonly int MatchedChars;

        public MatchResult(MatchType type, int score, int matchedChars)
        {
            Type = type;
            Score = score;
            MatchedChars = matchedChars;
        }

        public bool IsMatch => Type != MatchType.None;
        public static readonly MatchResult NoMatch = new(MatchType.None, 0, 0);
    }

    /// <summary>
    /// 匹配输入 query 与候选文件名 candidate。
    /// 同时尝试直接字符串匹配和拼音匹配，返回最优结果。
    /// </summary>
    public static MatchResult Match(string query, string candidate)
    {
        if (string.IsNullOrEmpty(query) || string.IsNullOrEmpty(candidate))
            return MatchResult.NoMatch;

        var qLower = query.ToLowerInvariant();
        var cLower = candidate.ToLowerInvariant();

        if (cLower == qLower)
            return new MatchResult(MatchType.Exact, 1000, candidate.Length);

        if (cLower.StartsWith(qLower))
            return new MatchResult(MatchType.Prefix, 800, query.Length);

        if (cLower.Contains(qLower))
            return new MatchResult(MatchType.Prefix, 600, query.Length);

        bool hasCjk = false;
        foreach (var ch in candidate)
        {
            if (PinyinTable.IsCjk(ch)) { hasCjk = true; break; }
        }

        if (!hasCjk)
        {
            var fuzzy = FuzzyMatch(qLower, cLower);
            return fuzzy > 0 ? new MatchResult(MatchType.Mixed, fuzzy, query.Length) : MatchResult.NoMatch;
        }

        bool allAscii = true;
        foreach (var ch in query)
        {
            if (!char.IsAsciiLetterOrDigit(ch)) { allAscii = false; break; }
        }

        if (!allAscii)
        {
            if (cLower.Contains(qLower))
                return new MatchResult(MatchType.Prefix, 700, query.Length);
            return MatchResult.NoMatch;
        }

        return MatchPinyin(qLower, candidate);
    }

    /// <summary>
    /// DP 拼音匹配核心：同时尝试全拼匹配、首字母匹配、混合模式。
    /// dp[charPos, inputPos] = 该状态可达时的最高分。
    /// </summary>
    private static MatchResult MatchPinyin(string query, string candidate)
    {
        PinyinTable.EnsureInitialized();

        var cjkChars = new List<(char ch, string[][] readings)>();
        foreach (var ch in candidate)
        {
            if (PinyinTable.IsCjk(ch))
            {
                var readings = PinyinTable.GetReadings(ch);
                if (readings != null)
                    cjkChars.Add((ch, readings.Select(r => new[] { r }).ToArray()));
                else
                    cjkChars.Add((ch, [[ch.ToString().ToLowerInvariant()]]));
            }
            else
            {
                cjkChars.Add((ch, [[char.ToLowerInvariant(ch).ToString()]]));
            }
        }

        if (cjkChars.Count == 0) return MatchResult.NoMatch;

        int n = cjkChars.Count;
        int m = query.Length;
        var dp = new int[n + 1, m + 1];
        for (int i = 0; i <= n; i++)
        for (int j = 0; j <= m; j++)
            dp[i, j] = -1;
        dp[0, 0] = 0;

        for (int i = 0; i < n; i++)
        {
            for (int j = 0; j <= m; j++)
            {
                if (dp[i, j] < 0) continue;

                dp[i + 1, j] = Math.Max(dp[i + 1, j], dp[i, j]);

                if (j >= m) continue;

                foreach (var readingSet in cjkChars[i].readings)
                {
                    var py = readingSet[0];
                    if (string.IsNullOrEmpty(py)) continue;

                    if (py[0] == query[j])
                    {
                        int newScore = dp[i, j] + 10;
                        dp[i + 1, j + 1] = Math.Max(dp[i + 1, j + 1], newScore);
                    }

                    int maxPrefix = Math.Min(py.Length, m - j);
                    for (int len = 1; len <= maxPrefix; len++)
                    {
                        bool match = true;
                        for (int k = 0; k < len; k++)
                        {
                            if (py[k] != query[j + k]) { match = false; break; }
                        }
                        if (!match) continue;

                        int bonus = len == py.Length ? 50 : len * 8;
                        int newScore2 = dp[i, j] + bonus;
                        dp[i + 1, j + len] = Math.Max(dp[i + 1, j + len], newScore2);
                    }
                }
            }
        }

        int bestScore = dp[n, m];
        if (bestScore > 0)
        {
            bool allFull = bestScore >= n * 40;
            var type = allFull ? MatchType.FullPinyin : MatchType.Mixed;
            return new MatchResult(type, 200 + bestScore, m);
        }

        var initials = PinyinTable.GetInitials(candidate);
        if (initials.StartsWith(query))
            return new MatchResult(MatchType.Initials, 400, query.Length);
        if (initials.Contains(query))
            return new MatchResult(MatchType.Initials, 300, query.Length);

        return MatchResult.NoMatch;
    }

    private static int FuzzyMatch(string query, string candidate)
    {
        int qi = 0;
        int score = 0;
        for (int ci = 0; ci < candidate.Length && qi < query.Length; ci++)
        {
            if (candidate[ci] == query[qi])
            {
                score += 10;
                qi++;
            }
        }
        return qi == query.Length ? score : 0;
    }
}
