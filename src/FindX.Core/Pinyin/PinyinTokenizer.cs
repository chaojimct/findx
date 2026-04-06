namespace FindX.Core.Pinyin;

/// <summary>
/// 将连续拼音字符串切分为可能的音节组合。
/// 支持标准声母+韵母组合，用于混合匹配中的最优路径搜索。
/// </summary>
public static class PinyinTokenizer
{
    private static readonly HashSet<string> ValidSyllables = new(StringComparer.OrdinalIgnoreCase);

    static PinyinTokenizer()
    {
        var initials = new[] { "b","p","m","f","d","t","n","l","g","k","h","j","q","x","zh","ch","sh","r","z","c","s","y","w","" };
        var finals = new[] { "a","o","e","i","u","v","ai","ei","ao","ou","an","en","ang","eng","ong","ia","ie","iao","iu","ian","in","iang","ing","iong","ua","uo","uai","ui","uan","un","uang","ueng","ve","van","vn" };

        foreach (var ini in initials)
        foreach (var fin in finals)
        {
            var s = ini + fin;
            if (s.Length > 0 && IsValidCombination(ini, fin))
                ValidSyllables.Add(s);
        }

        foreach (var extra in new[] { "a","o","e","ai","ei","ao","ou","an","en","ang","eng","er","yi","wu","yu","ye","yue","yuan","yin","yun","ying" })
            ValidSyllables.Add(extra);
    }

    private static bool IsValidCombination(string ini, string fin)
    {
        if (ini.Length == 0) return fin.Length > 0;
        if (ini is "j" or "q" or "x")
            return fin.StartsWith('i') || fin.StartsWith('v') || fin == "u" || fin == "ue" || fin == "uan" || fin == "un";
        if (ini is "zh" or "ch" or "sh" or "r" or "z" or "c" or "s")
            return !fin.StartsWith('v');
        return true;
    }

    public static bool IsValidSyllable(string s) => ValidSyllables.Contains(s);

    /// <summary>
    /// 找出输入字符串的所有可能拼音前缀（完整音节或音节前缀）。
    /// 返回 (前缀长度, 是否完整音节) 的列表。
    /// </summary>
    public static List<(int length, bool complete)> FindPrefixes(ReadOnlySpan<char> input)
    {
        var results = new List<(int, bool)>();
        if (input.IsEmpty) return results;

        for (int len = 1; len <= Math.Min(input.Length, 6); len++)
        {
            var sub = input[..len].ToString();
            if (ValidSyllables.Contains(sub))
                results.Add((len, true));
        }

        if (results.Count == 0 && input.Length > 0 && char.IsAsciiLetter(input[0]))
        {
            foreach (var syl in ValidSyllables)
            {
                if (syl.Length > 0 && syl[0] == char.ToLowerInvariant(input[0]))
                {
                    results.Add((1, false));
                    break;
                }
            }
        }

        return results;
    }

    /// <summary>
    /// 尝试将输入拆分为完整的拼音音节序列。
    /// 返回所有可能的拆分结果。
    /// </summary>
    public static List<string[]> Tokenize(string input, int maxResults = 8)
    {
        var results = new List<string[]>();
        Backtrack(input, 0, new List<string>(), results, maxResults);
        return results;
    }

    private static void Backtrack(string input, int pos, List<string> current, List<string[]> results, int max)
    {
        if (results.Count >= max) return;
        if (pos >= input.Length)
        {
            results.Add(current.ToArray());
            return;
        }

        for (int len = Math.Min(6, input.Length - pos); len >= 1; len--)
        {
            var sub = input.Substring(pos, len);
            if (ValidSyllables.Contains(sub))
            {
                current.Add(sub);
                Backtrack(input, pos + len, current, results, max);
                current.RemoveAt(current.Count - 1);
            }
        }
    }
}
