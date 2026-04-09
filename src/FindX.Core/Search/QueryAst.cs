using System.Globalization;
using System.Text.RegularExpressions;
using FindX.Core.Index;
using FindX.Core.Pinyin;

namespace FindX.Core.Search;

/// <summary>
/// 表达式求值上下文：封装单条候选的所有可评估数据，避免重复计算。
/// </summary>
public sealed class EvalContext
{
    public FileEntry Entry = null!;
    public string FullPath = "";
    public string NameLower = "";
    public int PathDepth;
    public bool CaseSensitive;

    public void Reset(FileEntry entry, string fullPath)
    {
        Entry = entry;
        FullPath = fullPath;
        NameLower = entry.Name.ToLowerInvariant();
        PathDepth = CountSeparators(fullPath);
        CaseSensitive = false;
    }

    private static int CountSeparators(string path)
    {
        int count = 0;
        foreach (var ch in path)
            if (ch is '\\' or '/') count++;
        return count;
    }
}

// ─── AST 节点基类 ───

public abstract class QueryNode
{
    public abstract bool Match(EvalContext ctx);

    /// <summary>收集所有正向（非 NOT 子树中的）搜索关键词，用于索引前缀查找。</summary>
    public virtual void CollectPositiveTerms(List<string> terms) { }
}

// ─── 布尔节点 ───

public sealed class AndNode : QueryNode
{
    public readonly List<QueryNode> Children;
    public AndNode(List<QueryNode> children) => Children = children;

    public override bool Match(EvalContext ctx)
    {
        foreach (var child in Children)
            if (!child.Match(ctx)) return false;
        return true;
    }

    public override void CollectPositiveTerms(List<string> terms)
    {
        foreach (var child in Children)
            child.CollectPositiveTerms(terms);
    }
}

public sealed class OrNode : QueryNode
{
    public readonly List<QueryNode> Children;
    public OrNode(List<QueryNode> children) => Children = children;

    public override bool Match(EvalContext ctx)
    {
        foreach (var child in Children)
            if (child.Match(ctx)) return true;
        return false;
    }

    public override void CollectPositiveTerms(List<string> terms)
    {
        foreach (var child in Children)
            child.CollectPositiveTerms(terms);
    }
}

public sealed class NotNode : QueryNode
{
    public readonly QueryNode Child;
    public NotNode(QueryNode child) => Child = child;

    public override bool Match(EvalContext ctx) => !Child.Match(ctx);
    // NOT 子树中的关键词不参与正向索引搜索
}

// ─── 关键词匹配节点 ───

public sealed class TermNode : QueryNode
{
    public readonly string Pattern;
    public readonly bool IsExact;       // 引号精确匹配
    public readonly bool HasWildcard;   // 包含 * 或 ?
    public readonly bool WholeWord;
    public readonly bool CaseSensitive;
    private readonly Regex? _wildcardRegex;

    public TermNode(string pattern, bool isExact = false, bool wholeWord = false, bool caseSensitive = false)
    {
        Pattern = pattern;
        IsExact = isExact;
        WholeWord = wholeWord;
        CaseSensitive = caseSensitive;
        HasWildcard = pattern.Contains('*') || pattern.Contains('?');

        if (HasWildcard)
        {
            string escaped = Regex.Escape(pattern).Replace("\\*", ".*").Replace("\\?", ".");
            var options = RegexOptions.Compiled;
            if (!caseSensitive) options |= RegexOptions.IgnoreCase;
            _wildcardRegex = new Regex($"^{escaped}$", options);
        }
    }

    public override bool Match(EvalContext ctx)
    {
        string name = ctx.Entry.Name;

        if (HasWildcard && _wildcardRegex != null)
            return _wildcardRegex.IsMatch(name);

        var comparison = CaseSensitive ? StringComparison.Ordinal : StringComparison.OrdinalIgnoreCase;

        if (IsExact)
            return name.Contains(Pattern, comparison);

        if (WholeWord)
        {
            var regex = new Regex($@"\b{Regex.Escape(Pattern)}\b",
                CaseSensitive ? RegexOptions.None : RegexOptions.IgnoreCase);
            return regex.IsMatch(name);
        }

        if (name.Contains(Pattern, comparison))
            return true;

        // 大小写敏感模式下不走拼音 fallback（拼音匹配本质不区分大小写）
        if (CaseSensitive)
            return false;

        var result = PinyinMatcher.Match(Pattern.ToLowerInvariant(), name);
        return result.IsMatch;
    }

    public override void CollectPositiveTerms(List<string> terms)
    {
        if (!HasWildcard)
            terms.Add(Pattern);
    }
}

// ─── 正则节点 ───

public sealed class RegexNode : QueryNode
{
    public readonly Regex CompiledRegex;

    public RegexNode(Regex regex) => CompiledRegex = regex;

    public override bool Match(EvalContext ctx) => CompiledRegex.IsMatch(ctx.Entry.Name);
}

// ─── 过滤节点 ───

public enum FilterType
{
    FileOnly,
    FolderOnly,
    Extension,
    Size,
    DateModified,
    DateCreated,
    DateAccessed,
    Path,
    NoPath,
    NameLength,
    Depth,
    Root,
    Attributes,
    StartWith,
    EndWith,
}

/// <summary>范围比较类型</summary>
public enum CompareOp { Eq, Gt, Lt, Gte, Lte, Range }

public sealed class FilterNode : QueryNode
{
    public readonly FilterType Type;

    // 通用条件存储
    private readonly CompareOp _op;
    private readonly long _longVal;
    private readonly long _longVal2;  // range 上界
    private readonly string _strVal;
    private readonly string[]? _strList;
    private readonly uint _uintVal;
    private readonly uint _uintMask;

    private FilterNode(FilterType type, CompareOp op = CompareOp.Eq,
        long longVal = 0, long longVal2 = 0,
        string strVal = "", string[]? strList = null,
        uint uintVal = 0, uint uintMask = 0)
    {
        Type = type;
        _op = op;
        _longVal = longVal;
        _longVal2 = longVal2;
        _strVal = strVal;
        _strList = strList;
        _uintVal = uintVal;
        _uintMask = uintMask;
    }

    public override bool Match(EvalContext ctx)
    {
        return Type switch
        {
            FilterType.FileOnly => !ctx.Entry.IsDirectory,
            FilterType.FolderOnly => ctx.Entry.IsDirectory,
            FilterType.Extension => MatchExtension(ctx),
            FilterType.Size => CompareValue(ctx.Entry.Size),
            FilterType.DateModified => CompareValue(ctx.Entry.LastWriteTimeTicks),
            FilterType.DateCreated => CompareValue(ctx.Entry.CreationTimeTicks),
            FilterType.DateAccessed => CompareValue(ctx.Entry.AccessTimeTicks),
            FilterType.Path => MatchPath(ctx.FullPath, false),
            FilterType.NoPath => !MatchPath(ctx.FullPath, false),
            FilterType.NameLength => CompareValue(ctx.Entry.Name.Length),
            FilterType.Depth => CompareValue(ctx.PathDepth),
            FilterType.Root => ctx.PathDepth <= 1,
            FilterType.Attributes => (ctx.Entry.Attributes & _uintMask) == _uintVal,
            FilterType.StartWith => ctx.Entry.Name.StartsWith(_strVal, StringComparison.OrdinalIgnoreCase),
            FilterType.EndWith => ctx.Entry.Name.EndsWith(_strVal, StringComparison.OrdinalIgnoreCase),
            _ => true,
        };
    }

    private bool MatchExtension(EvalContext ctx)
    {
        var ext = Path.GetExtension(ctx.Entry.Name).TrimStart('.');
        if (_strList != null)
        {
            foreach (var e in _strList)
                if (ext.Equals(e, StringComparison.OrdinalIgnoreCase)) return true;
            return false;
        }
        return ext.Equals(_strVal, StringComparison.OrdinalIgnoreCase);
    }

    private bool MatchPath(string fullPath, bool exact)
    {
        if (_strVal.Contains('*') || _strVal.Contains('?'))
        {
            string escaped = Regex.Escape(_strVal).Replace("\\*", ".*").Replace("\\?", ".");
            return Regex.IsMatch(fullPath, escaped, RegexOptions.IgnoreCase);
        }
        return fullPath.Contains(_strVal, StringComparison.OrdinalIgnoreCase);
    }

    private bool CompareValue(long actual)
    {
        return _op switch
        {
            CompareOp.Eq => actual == _longVal,
            CompareOp.Gt => actual > _longVal,
            CompareOp.Lt => actual < _longVal,
            CompareOp.Gte => actual >= _longVal,
            CompareOp.Lte => actual <= _longVal,
            CompareOp.Range => actual >= _longVal && actual <= _longVal2,
            _ => true,
        };
    }

    // ─── 静态工厂方法 ───

    public static FilterNode FileOnly() => new(FilterType.FileOnly);
    public static FilterNode FolderOnly() => new(FilterType.FolderOnly);
    public static FilterNode RootOnly() => new(FilterType.Root);

    public static FilterNode StartWith(string text) =>
        new(FilterType.StartWith, strVal: text);

    public static FilterNode EndWith(string text) =>
        new(FilterType.EndWith, strVal: text);

    public static FilterNode ParseExtension(string value)
    {
        value = value.TrimStart('.');
        if (value.Contains(';'))
        {
            var list = value.Split(';', StringSplitOptions.RemoveEmptyEntries);
            return new FilterNode(FilterType.Extension, strList: list);
        }
        return new FilterNode(FilterType.Extension, strVal: value);
    }

    public static FilterNode ParsePath(string value, bool negate = false) =>
        new(negate ? FilterType.NoPath : FilterType.Path, strVal: value.Trim('"'));

    public static FilterNode ParseSize(string value)
    {
        var (op, v1, v2) = ParseNumericRange(value, ParseSizeValue);
        return new FilterNode(FilterType.Size, op: op, longVal: v1, longVal2: v2);
    }

    public static FilterNode ParseDateModified(string value)
    {
        var (op, v1, v2) = ParseDateRange(value);
        return new FilterNode(FilterType.DateModified, op: op, longVal: v1, longVal2: v2);
    }

    public static FilterNode ParseDateCreated(string value)
    {
        var (op, v1, v2) = ParseDateRange(value);
        return new FilterNode(FilterType.DateCreated, op: op, longVal: v1, longVal2: v2);
    }

    public static FilterNode ParseDateAccessed(string value)
    {
        var (op, v1, v2) = ParseDateRange(value);
        return new FilterNode(FilterType.DateAccessed, op: op, longVal: v1, longVal2: v2);
    }

    public static FilterNode ParseNameLength(string value)
    {
        var (op, v1, v2) = ParseNumericRange(value, s => long.TryParse(s, out var n) ? n : -1);
        return new FilterNode(FilterType.NameLength, op: op, longVal: v1, longVal2: v2);
    }

    public static FilterNode ParseDepth(string value)
    {
        var (op, v1, v2) = ParseNumericRange(value, s => long.TryParse(s, out var n) ? n : -1);
        return new FilterNode(FilterType.Depth, op: op, longVal: v1, longVal2: v2);
    }

    public static FilterNode ParseAttributes(string value)
    {
        uint mask = 0;
        uint expected = 0;
        foreach (char c in value.ToUpperInvariant())
        {
            uint bit = c switch
            {
                'R' => 0x01,   // FILE_ATTRIBUTE_READONLY
                'H' => 0x02,   // FILE_ATTRIBUTE_HIDDEN
                'S' => 0x04,   // FILE_ATTRIBUTE_SYSTEM
                'D' => 0x10,   // FILE_ATTRIBUTE_DIRECTORY
                'A' => 0x20,   // FILE_ATTRIBUTE_ARCHIVE
                'N' => 0x80,   // FILE_ATTRIBUTE_NORMAL
                'T' => 0x100,  // FILE_ATTRIBUTE_TEMPORARY
                'C' => 0x800,  // FILE_ATTRIBUTE_COMPRESSED
                'O' => 0x1000, // FILE_ATTRIBUTE_OFFLINE
                'I' => 0x2000, // FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
                'E' => 0x4000, // FILE_ATTRIBUTE_ENCRYPTED
                _ => 0,
            };
            if (bit != 0)
            {
                mask |= bit;
                expected |= bit;
            }
        }
        return new FilterNode(FilterType.Attributes, uintMask: mask, uintVal: expected);
    }

    // ─── 解析辅助 ───

    private static (CompareOp op, long v1, long v2) ParseNumericRange(string value, Func<string, long> parser)
    {
        int dotdot = value.IndexOf("..", StringComparison.Ordinal);
        if (dotdot >= 0)
        {
            var lo = parser(value[..dotdot]);
            var hi = parser(value[(dotdot + 2)..]);
            return (CompareOp.Range, lo, hi);
        }

        if (value.StartsWith(">="))
            return (CompareOp.Gte, parser(value[2..]), 0);
        if (value.StartsWith("<="))
            return (CompareOp.Lte, parser(value[2..]), 0);
        if (value.StartsWith('>'))
            return (CompareOp.Gt, parser(value[1..]), 0);
        if (value.StartsWith('<'))
            return (CompareOp.Lt, parser(value[1..]), 0);

        var v = parser(value);
        return (CompareOp.Eq, v, 0);
    }

    private static long ParseSizeValue(string s)
    {
        s = s.Trim();
        long multiplier = 1;
        string numPart = s;

        if (s.EndsWith("tb", StringComparison.OrdinalIgnoreCase))
        { multiplier = 1L << 40; numPart = s[..^2]; }
        else if (s.EndsWith("gb", StringComparison.OrdinalIgnoreCase))
        { multiplier = 1L << 30; numPart = s[..^2]; }
        else if (s.EndsWith("mb", StringComparison.OrdinalIgnoreCase))
        { multiplier = 1L << 20; numPart = s[..^2]; }
        else if (s.EndsWith("kb", StringComparison.OrdinalIgnoreCase))
        { multiplier = 1L << 10; numPart = s[..^2]; }

        if (double.TryParse(numPart, NumberStyles.Float, CultureInfo.InvariantCulture, out var d))
            return (long)(d * multiplier);
        return 0;
    }

    private static (CompareOp op, long v1, long v2) ParseDateRange(string value)
    {
        int dotdot = value.IndexOf("..", StringComparison.Ordinal);
        if (dotdot >= 0)
        {
            var lo = ParseDateValue(value[..dotdot]);
            var hi = ParseDateEndValue(value[(dotdot + 2)..]);
            return (CompareOp.Range, lo, hi);
        }

        if (value.StartsWith(">="))
            return (CompareOp.Gte, ParseDateValue(value[2..]), 0);
        if (value.StartsWith("<="))
            return (CompareOp.Lte, ParseDateEndValue(value[2..]), 0);
        if (value.StartsWith('>'))
            return (CompareOp.Gt, ParseDateEndValue(value[1..]), 0);
        if (value.StartsWith('<'))
            return (CompareOp.Lt, ParseDateValue(value[1..]), 0);

        // 无操作符 = 整个时间段范围
        var start = ParseDateValue(value);
        var end = ParseDateEndValue(value);
        if (start != end)
            return (CompareOp.Range, start, end);
        return (CompareOp.Gte, start, 0);
    }

    private static long ParseDateValue(string s)
    {
        s = s.Trim().ToLowerInvariant();
        var now = DateTime.Now;

        return s switch
        {
            "today" => now.Date.Ticks,
            "yesterday" => now.Date.AddDays(-1).Ticks,
            "thisweek" => now.Date.AddDays(-(int)now.DayOfWeek).Ticks,
            "thismonth" => new DateTime(now.Year, now.Month, 1).Ticks,
            "thisyear" => new DateTime(now.Year, 1, 1).Ticks,
            "lastweek" => now.Date.AddDays(-(int)now.DayOfWeek - 7).Ticks,
            "lastmonth" => new DateTime(now.Year, now.Month, 1).AddMonths(-1).Ticks,
            "lastyear" => new DateTime(now.Year - 1, 1, 1).Ticks,
            _ when s.StartsWith("last") => ParseRelativeDate(s[4..]),
            _ => ParseAbsoluteDate(s),
        };
    }

    private static long ParseDateEndValue(string s)
    {
        s = s.Trim().ToLowerInvariant();
        var now = DateTime.Now;

        return s switch
        {
            "today" => now.Date.AddDays(1).Ticks - 1,
            "yesterday" => now.Date.Ticks - 1,
            "thisweek" => now.Date.AddDays(7 - (int)now.DayOfWeek).Ticks - 1,
            "thismonth" => new DateTime(now.Year, now.Month, 1).AddMonths(1).Ticks - 1,
            "thisyear" => new DateTime(now.Year + 1, 1, 1).Ticks - 1,
            "lastweek" => now.Date.AddDays(-(int)now.DayOfWeek).Ticks - 1,
            "lastmonth" => new DateTime(now.Year, now.Month, 1).Ticks - 1,
            "lastyear" => new DateTime(now.Year, 1, 1).Ticks - 1,
            _ when s.StartsWith("last") => DateTime.Now.Ticks,
            _ => ParseAbsoluteDateEnd(s),
        };
    }

    /// <summary>解析 last2weeks / last3months / last7days 等相对日期</summary>
    private static long ParseRelativeDate(string s)
    {
        int numEnd = 0;
        while (numEnd < s.Length && char.IsDigit(s[numEnd])) numEnd++;
        if (numEnd == 0) return 0;

        int num = int.Parse(s[..numEnd]);
        string unit = s[numEnd..].ToLowerInvariant();
        var now = DateTime.Now;

        return unit switch
        {
            "days" or "day" => now.AddDays(-num).Ticks,
            "weeks" or "week" => now.AddDays(-num * 7).Ticks,
            "months" or "month" => now.AddMonths(-num).Ticks,
            "years" or "year" => now.AddYears(-num).Ticks,
            "hours" or "hour" => now.AddHours(-num).Ticks,
            "minutes" or "minute" or "mins" or "min" => now.AddMinutes(-num).Ticks,
            "seconds" or "second" or "secs" or "sec" => now.AddSeconds(-num).Ticks,
            _ => 0,
        };
    }

    private static long ParseAbsoluteDate(string s)
    {
        // yyyy
        if (s.Length == 4 && int.TryParse(s, out var year))
            return new DateTime(year, 1, 1).Ticks;

        // yyyy-MM or yyyy/MM
        if (s.Length is 6 or 7)
        {
            if (DateTime.TryParseExact(s, new[] { "yyyy-M", "yyyy/M", "yyyy-MM", "yyyy/MM" },
                    CultureInfo.InvariantCulture, DateTimeStyles.None, out var ym))
                return ym.Ticks;
        }

        string[] formats = { "yyyy-MM-dd", "yyyy/MM/dd", "yyyy-M-d", "yyyy/M/d",
                             "yyyy-MM-dd HH:mm:ss", "yyyy/MM/dd HH:mm:ss" };
        if (DateTime.TryParseExact(s, formats, CultureInfo.InvariantCulture, DateTimeStyles.None, out var dt))
            return dt.Ticks;

        if (DateTime.TryParse(s, CultureInfo.InvariantCulture, DateTimeStyles.None, out dt))
            return dt.Ticks;

        return 0;
    }

    private static long ParseAbsoluteDateEnd(string s)
    {
        if (s.Length == 4 && int.TryParse(s, out var year))
            return new DateTime(year + 1, 1, 1).Ticks - 1;

        if (s.Length is 6 or 7)
        {
            if (DateTime.TryParseExact(s, new[] { "yyyy-M", "yyyy/M", "yyyy-MM", "yyyy/MM" },
                    CultureInfo.InvariantCulture, DateTimeStyles.None, out var ym))
                return ym.AddMonths(1).Ticks - 1;
        }

        string[] formats = { "yyyy-MM-dd", "yyyy/MM/dd", "yyyy-M-d", "yyyy/M/d" };
        if (DateTime.TryParseExact(s, formats, CultureInfo.InvariantCulture, DateTimeStyles.None, out var dt))
            return dt.AddDays(1).Ticks - 1;

        if (DateTime.TryParse(s, CultureInfo.InvariantCulture, DateTimeStyles.None, out dt))
            return dt.AddDays(1).Ticks - 1;

        return long.MaxValue;
    }
}
