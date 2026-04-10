using System.Buffers;
using System.Text;
using FindX.Core.Interop;

namespace FindX.Core.Pinyin;

/// <summary>
/// Final pinyin matching is delegated to the Rust engine so search, filtering and
/// candidate scoring all share the same source of truth.
/// </summary>
public static class PinyinMatcher
{
    private const int StackUtf8Threshold = 512;

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

    public readonly struct PreparedQuery
    {
        internal readonly string Lower;
        internal readonly byte[] Utf8;

        internal PreparedQuery(string lower, byte[] utf8)
        {
            Lower = lower;
            Utf8 = utf8;
        }

        public bool IsEmpty => Utf8 == null || Utf8.Length == 0;
    }

    public static PreparedQuery Prepare(string query)
    {
        if (string.IsNullOrWhiteSpace(query))
            return default;

        var lower = query.ToLowerInvariant();
        return new PreparedQuery(lower, Encoding.UTF8.GetBytes(lower));
    }

    public static MatchResult Match(string queryLower, string candidate)
        => Match(Prepare(queryLower), candidate);

    public static MatchResult Match(PreparedQuery query, string candidate)
    {
        if (query.IsEmpty || string.IsNullOrEmpty(candidate))
            return MatchResult.NoMatch;

        var candidateByteCount = Encoding.UTF8.GetByteCount(candidate);
        if (candidateByteCount == 0)
            return MatchResult.NoMatch;

        if (candidateByteCount <= StackUtf8Threshold)
        {
            Span<byte> candidateUtf8 = stackalloc byte[candidateByteCount];
            Encoding.UTF8.GetBytes(candidate, candidateUtf8);
            return MatchCore(query, candidateUtf8);
        }

        var rented = ArrayPool<byte>.Shared.Rent(candidateByteCount);
        try
        {
            var written = Encoding.UTF8.GetBytes(candidate, rented);
            return MatchCore(query, rented.AsSpan(0, written));
        }
        finally
        {
            ArrayPool<byte>.Shared.Return(rented);
        }
    }

    private static unsafe MatchResult MatchCore(PreparedQuery query, ReadOnlySpan<byte> candidateUtf8)
    {
        fixed (byte* pq = query.Utf8)
        fixed (byte* pc = candidateUtf8)
        {
            var rc = RustIndexNative.findx_match_name_utf8(
                (IntPtr)pq,
                query.Utf8.Length,
                (IntPtr)pc,
                candidateUtf8.Length,
                out var matchType,
                out var score,
                out var matchedChars);

            if (rc <= 0)
                return MatchResult.NoMatch;

            var type = Enum.IsDefined(typeof(MatchType), matchType)
                ? (MatchType)matchType
                : MatchType.None;
            return new MatchResult(type, score, matchedChars);
        }
    }
}
