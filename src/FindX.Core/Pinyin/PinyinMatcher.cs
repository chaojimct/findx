using System.Buffers;

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
    /// 匹配输入 queryLower（必须由调用方预先 ToLowerInvariant）与候选文件名 candidate。
    /// 同时尝试直接字符串匹配和拼音匹配，返回最优结果。
    /// </summary>
    public static MatchResult Match(string queryLower, string candidate)
    {
        if (string.IsNullOrEmpty(queryLower) || string.IsNullOrEmpty(candidate))
            return MatchResult.NoMatch;

        if (candidate.Equals(queryLower, StringComparison.OrdinalIgnoreCase))
            return new MatchResult(MatchType.Exact, 1000, candidate.Length);

        if (candidate.StartsWith(queryLower, StringComparison.OrdinalIgnoreCase))
            return new MatchResult(MatchType.Prefix, 800, queryLower.Length);

        if (candidate.Contains(queryLower, StringComparison.OrdinalIgnoreCase))
            return new MatchResult(MatchType.Prefix, 600, queryLower.Length);

        bool hasCjk = false;
        foreach (var ch in candidate)
        {
            if (PinyinTable.IsCjk(ch)) { hasCjk = true; break; }
        }

        if (!hasCjk)
        {
            int fuzzy = FuzzyMatch(queryLower, candidate);
            return fuzzy > 0 ? new MatchResult(MatchType.Mixed, fuzzy, queryLower.Length) : MatchResult.NoMatch;
        }

        bool allAscii = true;
        foreach (var ch in queryLower)
        {
            if (!char.IsAsciiLetterOrDigit(ch)) { allAscii = false; break; }
        }

        if (!allAscii)
        {
            if (candidate.Contains(queryLower, StringComparison.OrdinalIgnoreCase))
                return new MatchResult(MatchType.Prefix, 700, queryLower.Length);
            return MatchResult.NoMatch;
        }

        return MatchPinyin(queryLower, candidate);
    }

    /// <summary>
    /// 零分配 DP 拼音匹配：用 stackalloc 一维数组代替 new int[,]，
    /// 直接遍历 candidate 字符并从 PinyinTable 获取读音，不再构建 List。
    /// </summary>
    private static MatchResult MatchPinyin(string queryLower, string candidate)
    {
        PinyinTable.EnsureInitialized();

        int n = candidate.Length;
        int m = queryLower.Length;
        int stride = m + 1;
        int dpSize = (n + 1) * stride;

        int[]? rented = null;
        Span<int> dp = dpSize <= 2048
            ? stackalloc int[dpSize]
            : (rented = ArrayPool<int>.Shared.Rent(dpSize)).AsSpan(0, dpSize);
        dp.Fill(-1);
        dp[0] = 0;

        for (int i = 0; i < n; i++)
        {
            char ch = candidate[i];
            int rowCur = i * stride;
            int rowNext = rowCur + stride;

            for (int j = 0; j <= m; j++)
            {
                int cur = dp[rowCur + j];
                if (cur < 0) continue;

                ref int skipSlot = ref dp[rowNext + j];
                if (cur > skipSlot) skipSlot = cur;

                if (j >= m) continue;

                if (PinyinTable.IsCjk(ch))
                {
                    var readings = PinyinTable.GetReadings(ch);
                    if (readings != null)
                    {
                        for (int ri = 0; ri < readings.Length; ri++)
                        {
                            var py = readings[ri];
                            if (py.Length == 0) continue;

                            if (py[0] == queryLower[j])
                            {
                                ref int r = ref dp[rowNext + j + 1];
                                int s = cur + 10;
                                if (s > r) r = s;
                            }

                            int maxPre = Math.Min(py.Length, m - j);
                            for (int len = 1; len <= maxPre; len++)
                            {
                                if (py[len - 1] != queryLower[j + len - 1]) break;
                                int bonus = len == py.Length ? 50 : len * 8;
                                ref int r = ref dp[rowNext + j + len];
                                int s = cur + bonus;
                                if (s > r) r = s;
                            }
                        }
                    }
                    else
                    {
                        if (char.ToLowerInvariant(ch) == queryLower[j])
                        {
                            ref int r = ref dp[rowNext + j + 1];
                            int s = cur + 10;
                            if (s > r) r = s;
                        }
                    }
                }
                else
                {
                    if (char.ToLowerInvariant(ch) == queryLower[j])
                    {
                        ref int r = ref dp[rowNext + j + 1];
                        int s = cur + 10;
                        if (s > r) r = s;
                    }
                }
            }
        }

        int bestScore = dp[n * stride + m];
        if (rented != null) ArrayPool<int>.Shared.Return(rented);

        if (bestScore > 0)
        {
            bool allFull = bestScore >= n * 40;
            var type = allFull ? MatchType.FullPinyin : MatchType.Mixed;
            return new MatchResult(type, 200 + bestScore, m);
        }

        var initials = PinyinTable.GetInitials(candidate);
        if (initials.StartsWith(queryLower))
            return new MatchResult(MatchType.Initials, 400, queryLower.Length);
        if (initials.Contains(queryLower))
            return new MatchResult(MatchType.Initials, 300, queryLower.Length);

        return MatchResult.NoMatch;
    }

    private static int FuzzyMatch(string queryLower, string candidate)
    {
        int qi = 0;
        int score = 0;
        for (int ci = 0; ci < candidate.Length && qi < queryLower.Length; ci++)
        {
            if (char.ToLowerInvariant(candidate[ci]) == queryLower[qi])
            {
                score += 10;
                qi++;
            }
        }
        return qi == queryLower.Length ? score : 0;
    }
}
