using System.Linq;
using System.Text;
using FindX.Core.Pinyin;

namespace FindX.Core.Search;

/// <summary>用于结果列表：将文件名拆成普通片段与高亮片段。</summary>
public readonly struct HighlightPart
{
    public string Text { get; init; }
    public bool IsHighlight { get; init; }
}

/// <summary>
/// 根据用户输入的原始关键词（不含侧栏拼上的 filter）计算文件名高亮区间。
/// 含：字面子串、中文简拼/首字母串、与 Rust <c>compute_full_py_stack</c> 一致的全拼压缩串子串（对齐 <c>SearchFullPinyinContains</c>）、纯英文子序列。
/// </summary>
public static class SearchHighlightBuilder
{
    public static IReadOnlyList<HighlightPart> BuildNameParts(string fileName, string? rawQuery)
    {
        if (string.IsNullOrEmpty(fileName))
            return [new HighlightPart { Text = "", IsHighlight = false }];

        var terms = SplitHighlightTerms(rawQuery);
        if (terms.Count == 0)
            return [new HighlightPart { Text = fileName, IsHighlight = false }];

        var ranges = new List<(int Start, int Len)>();
        foreach (var term in terms)
        {
            var t = term.Trim();
            if (t.Length == 0 || LooksLikeFilterToken(t)) continue;
            AddLiteralRanges(fileName, t, ranges);

            bool asciiTerm = IsAsciiLetterDigitOnly(t);
            if (asciiTerm)
            {
                var tl = t.ToLowerInvariant();
                // 无连续子串但整词仍能命中时，与 Matcher 的英文子序列一致
                if (!PinyinTable.NameContainsCjk(fileName)
                    && PinyinMatcher.Match(tl, fileName).IsMatch
                    && !NameContainsLiteralIgnoreCase(fileName, tl))
                    AddSubsequenceRanges(fileName, tl, ranges);

                if (PinyinTable.NameContainsCjk(fileName)
                    && PinyinMatcher.Match(tl, fileName).IsMatch)
                {
                    var initials = PinyinTable.GetInitials(fileName);
                    if (initials.StartsWith(tl, StringComparison.OrdinalIgnoreCase))
                        AddInitialsPrefixRanges(fileName, tl, ranges);
                    else if (initials.Contains(tl, StringComparison.OrdinalIgnoreCase))
                        AddInitialsSubstringRanges(fileName, tl, ranges);
                }

                // 与索引侧 SearchFullPinyinContains / Rust compute_full_py_stack 对齐
                if (PinyinTable.NameContainsCjk(fileName))
                    AddFullPinyinCompactRanges(fileName, tl, ranges);
            }
        }

        MergeRanges(ranges);
        return ToParts(fileName, ranges);
    }

    private static List<string> SplitHighlightTerms(string? raw)
    {
        var list = new List<string>();
        if (string.IsNullOrWhiteSpace(raw)) return list;
        foreach (var part in raw.Trim().Split([' ', '\t'], StringSplitOptions.RemoveEmptyEntries))
            list.Add(part);
        return list;
    }

    private static bool LooksLikeFilterToken(string t)
    {
        var i = t.IndexOf(':');
        if (i <= 0 || i > 20) return false;
        for (int j = 0; j < i; j++)
        {
            if (!char.IsAsciiLetter(t[j])) return false;
        }
        return true;
    }

    private static bool IsAsciiLetterDigitOnly(string t)
    {
        foreach (var c in t)
        {
            if (!char.IsAsciiLetterOrDigit(c)) return false;
        }
        return true;
    }

    private static bool NameContainsLiteralIgnoreCase(string name, string needleLower)
    {
        return name.AsSpan().Contains(needleLower, StringComparison.OrdinalIgnoreCase);
    }

    private static void AddLiteralRanges(string name, string term, List<(int Start, int Len)> ranges)
    {
        if (term.Length == 0) return;
        int idx = 0;
        while ((idx = name.IndexOf(term, idx, StringComparison.OrdinalIgnoreCase)) >= 0)
        {
            ranges.Add((idx, term.Length));
            idx += term.Length;
        }
    }

    private static void AddSubsequenceRanges(string name, string queryLower, List<(int Start, int Len)> ranges)
    {
        int qi = 0;
        for (int i = 0; i < name.Length && qi < queryLower.Length; i++)
        {
            if (char.ToLowerInvariant(name[i]) == queryLower[qi])
            {
                ranges.Add((i, 1));
                qi++;
            }
        }
    }

    /// <summary>按从左到右首字母/数字与 query 逐字对齐（跳过标点），匹配简拼前缀场景。</summary>
    private static void AddInitialsPrefixRanges(string name, string queryLower, List<(int Start, int Len)> ranges)
    {
        PinyinTable.EnsureInitialized();
        int q = 0;
        for (int i = 0; i < name.Length && q < queryLower.Length; i++)
        {
            char ch = name[i];
            char? ic = null;
            if (PinyinTable.IsCjk(ch))
            {
                var py = PinyinTable.GetPrimaryReading(ch);
                if (py is { Length: > 0 })
                    ic = py[0];
            }
            else if (char.IsAsciiLetterOrDigit(ch))
                ic = char.ToLowerInvariant(ch);

            if (ic == null)
                continue;

            if (ic == queryLower[q])
            {
                ranges.Add((i, 1));
                q++;
            }
            else
                break;
        }
    }

    /// <summary>
    /// 构建与 native/findx-engine compute_full_py_stack 相同的压缩串（ASCII 数字字母逐字小写 + 汉字主读音拼音仅 a-z），
    /// 找出 needle 的每次出现，将命中的字节映射回文件名 UTF-16 索引并合并为连续区间。
    /// </summary>
    private static void AddFullPinyinCompactRanges(string name, string needleLower, List<(int Start, int Len)> ranges)
    {
        if (needleLower.Length == 0) return;
        PinyinTable.EnsureInitialized();

        const int MaxCompact = 1024;
        var compact = new List<byte>(MaxCompact);
        var owner = new List<int>(MaxCompact);

        for (int i = 0; i < name.Length; i++)
        {
            if (compact.Count >= MaxCompact) break;
            char ch = name[i];

            if (char.IsAsciiLetterOrDigit(ch))
            {
                compact.Add((byte)char.ToLowerInvariant(ch));
                owner.Add(i);
                continue;
            }

            if (PinyinTable.IsCjk(ch))
            {
                var py = PinyinTable.GetPrimaryReading(ch);
                if (py is not { Length: > 0 }) continue;
                foreach (var c in py.ToLowerInvariant())
                {
                    if (compact.Count >= MaxCompact) break;
                    if (c is >= 'a' and <= 'z')
                    {
                        compact.Add((byte)c);
                        owner.Add(i);
                    }
                }
            }
        }

        if (compact.Count == 0) return;
        var nb = Encoding.ASCII.GetBytes(needleLower);
        if (nb.Length == 0 || compact.Count < nb.Length) return;

        for (int s = 0; s <= compact.Count - nb.Length; s++)
        {
            if (!CompactMatchAt(compact, s, nb)) continue;
            AddMergedCharRangesFromOwners(owner, s, nb.Length, ranges);
        }
    }

    private static bool CompactMatchAt(List<byte> hay, int start, byte[] needle)
    {
        for (int j = 0; j < needle.Length; j++)
        {
            if (hay[start + j] != needle[j])
                return false;
        }
        return true;
    }

    private static void AddMergedCharRangesFromOwners(List<int> owner, int byteStart, int needleLen,
        List<(int Start, int Len)> ranges)
    {
        var sorted = new SortedSet<int>();
        for (int k = 0; k < needleLen; k++)
            sorted.Add(owner[byteStart + k]);
        if (sorted.Count == 0) return;

        int? runStart = null;
        int prev = 0;
        foreach (var c in sorted)
        {
            if (runStart == null)
            {
                runStart = c;
                prev = c;
                continue;
            }
            if (c == prev + 1)
            {
                prev = c;
                continue;
            }
            ranges.Add((runStart.Value, prev - runStart.Value + 1));
            runStart = c;
            prev = c;
        }
        if (runStart != null)
            ranges.Add((runStart.Value, prev - runStart.Value + 1));
    }

    private static void AddInitialsSubstringRanges(string name, string queryLower, List<(int Start, int Len)> ranges)
    {
        PinyinTable.EnsureInitialized();
        var initials = PinyinTable.GetInitials(name);
        int idx = initials.IndexOf(queryLower, StringComparison.OrdinalIgnoreCase);
        if (idx < 0) return;
        int endIni = idx + queryLower.Length;

        int iniPos = 0;
        for (int i = 0; i < name.Length && iniPos < endIni; i++)
        {
            char ch = name[i];
            bool contributes = false;
            if (PinyinTable.IsCjk(ch))
            {
                var py = PinyinTable.GetPrimaryReading(ch);
                if (py is { Length: > 0 }) contributes = true;
            }
            else if (char.IsAsciiLetterOrDigit(ch))
                contributes = true;

            if (!contributes)
                continue;

            if (iniPos >= idx && iniPos < endIni)
                ranges.Add((i, 1));
            iniPos++;
        }
    }

    private static void MergeRanges(List<(int Start, int Len)> ranges)
    {
        if (ranges.Count <= 1) return;
        ranges.Sort((a, b) => a.Start.CompareTo(b.Start));
        var merged = new List<(int Start, int Len)> { ranges[0] };
        foreach (var cur in ranges.Skip(1))
        {
            var last = merged[^1];
            int lastEnd = last.Start + last.Len;
            int curEnd = cur.Start + cur.Len;
            if (cur.Start <= lastEnd)
                merged[^1] = (last.Start, Math.Max(lastEnd, curEnd) - last.Start);
            else
                merged.Add(cur);
        }
        ranges.Clear();
        ranges.AddRange(merged);
    }

    private static IReadOnlyList<HighlightPart> ToParts(string name, List<(int Start, int Len)> ranges)
    {
        if (ranges.Count == 0)
            return [new HighlightPart { Text = name, IsHighlight = false }];

        var parts = new List<HighlightPart>();
        int pos = 0;
        foreach (var (start, len) in ranges)
        {
            if (start > pos)
                parts.Add(new HighlightPart { Text = name[pos..start], IsHighlight = false });
            if (len > 0 && start + len <= name.Length)
                parts.Add(new HighlightPart { Text = name.Substring(start, len), IsHighlight = true });
            pos = start + len;
        }
        if (pos < name.Length)
            parts.Add(new HighlightPart { Text = name[pos..], IsHighlight = false });

        return parts.Count > 0 ? parts : [new HighlightPart { Text = name, IsHighlight = false }];
    }
}
