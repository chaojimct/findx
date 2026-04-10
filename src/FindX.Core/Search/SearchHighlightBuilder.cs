using System.Buffers;
using System.Linq;
using System.Text;
using FindX.Core.Interop;
using FindX.Core.Pinyin;

namespace FindX.Core.Search;

public readonly struct HighlightPart
{
    public string Text { get; init; }
    public bool IsHighlight { get; init; }
}

public static class SearchHighlightBuilder
{
    private const int StackUtf8Threshold = 512;
    private const int StackRangePairs = 16;

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
            var text = term.Trim();
            if (text.Length == 0 || LooksLikeFilterToken(text))
                continue;

            AddRustRanges(text, fileName, ranges);
        }

        MergeRanges(ranges);
        return ToParts(fileName, ranges);
    }

    private static List<string> SplitHighlightTerms(string? raw)
    {
        var list = new List<string>();
        if (string.IsNullOrWhiteSpace(raw))
            return list;

        foreach (var part in raw.Trim().Split([' ', '\t'], StringSplitOptions.RemoveEmptyEntries))
            list.Add(part);
        return list;
    }

    private static bool LooksLikeFilterToken(string text)
    {
        var colon = text.IndexOf(':');
        if (colon <= 0 || colon > 20)
            return false;

        for (int i = 0; i < colon; i++)
        {
            if (!char.IsAsciiLetter(text[i]))
                return false;
        }
        return true;
    }

    private static void AddRustRanges(string query, string fileName, List<(int Start, int Len)> ranges)
    {
        var prepared = PinyinMatcher.Prepare(query);
        if (prepared.IsEmpty)
            return;

        var candidateByteCount = Encoding.UTF8.GetByteCount(fileName);
        if (candidateByteCount == 0)
            return;

        if (candidateByteCount <= StackUtf8Threshold)
        {
            Span<byte> candidateUtf8 = stackalloc byte[candidateByteCount];
            Encoding.UTF8.GetBytes(fileName, candidateUtf8);
            AddRustRangesCore(prepared, candidateUtf8, ranges);
            return;
        }

        var rented = ArrayPool<byte>.Shared.Rent(candidateByteCount);
        try
        {
            var written = Encoding.UTF8.GetBytes(fileName, rented);
            AddRustRangesCore(prepared, rented.AsSpan(0, written), ranges);
        }
        finally
        {
            ArrayPool<byte>.Shared.Return(rented);
        }
    }

    private static unsafe void AddRustRangesCore(PinyinMatcher.PreparedQuery query, ReadOnlySpan<byte> candidateUtf8,
        List<(int Start, int Len)> ranges)
    {
        Span<int> stackBuffer = stackalloc int[StackRangePairs * 2];
        fixed (byte* pq = query.Utf8)
        fixed (byte* pc = candidateUtf8)
        fixed (int* pr = stackBuffer)
        {
            var pairCount = RustIndexNative.findx_highlight_name_utf8(
                (IntPtr)pq,
                query.Utf8.Length,
                (IntPtr)pc,
                candidateUtf8.Length,
                (IntPtr)pr,
                StackRangePairs);

            if (pairCount <= 0)
                return;

            for (int i = 0; i < pairCount; i++)
            {
                var len = stackBuffer[i * 2 + 1];
                if (len > 0)
                    ranges.Add((stackBuffer[i * 2], len));
            }
        }
    }

    private static void MergeRanges(List<(int Start, int Len)> ranges)
    {
        if (ranges.Count <= 1)
            return;

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

        return parts.Count > 0
            ? parts
            : [new HighlightPart { Text = name, IsHighlight = false }];
    }
}
