namespace FindX.Core.Search;

public enum TokenType
{
    Term,
    QuotedString,
    Filter,
    OrOp,
    NotOp,
    OpenGroup,
    CloseGroup,
}

public readonly struct QueryToken
{
    public readonly TokenType Type;
    public readonly string Value;
    public readonly string? FilterPrefix;

    public QueryToken(TokenType type, string value, string? filterPrefix = null)
    {
        Type = type;
        Value = value;
        FilterPrefix = filterPrefix;
    }
}

/// <summary>
/// Everything 兼容查询词法分析器。
/// 处理引号短语、| OR、! NOT、&lt; &gt; 分组、已知 filter: 前缀、通配符 */? 保留在 Term 内。
/// </summary>
public static class QueryTokenizer
{
    /// <summary>值可写为 path:&quot;... ...&quot;（含空格、冒号），与 Everything 一致。</summary>
    private static readonly HashSet<string> QuotedValueFilters = new(StringComparer.OrdinalIgnoreCase)
    {
        "path", "parent", "nopath", "root",
    };

    private static readonly HashSet<string> KnownFilters = new(StringComparer.OrdinalIgnoreCase)
    {
        "ext", "path", "nopath", "parent",
        "file", "folder",
        "size",
        "dm", "datemodified",
        "dc", "datecreated",
        "da", "dateaccessed",
        "len",
        "depth", "parents",
        "root", "volroot",
        "attrib", "attributes",
        "case", "nocase",
        "wholeword", "ww",
        "startwith", "endwith",
        "regex",
        "count",
        "type", "audio", "video", "doc", "exe", "zip", "pic",
        "shell",
    };

    public static List<QueryToken> Tokenize(string input)
    {
        var tokens = new List<QueryToken>();
        int i = 0;
        int len = input.Length;

        while (i < len)
        {
            char c = input[i];

            if (char.IsWhiteSpace(c)) { i++; continue; }

            if (c == '"')
            {
                i++;
                int start = i;
                while (i < len && input[i] != '"') i++;
                tokens.Add(new QueryToken(TokenType.QuotedString, input[start..i]));
                if (i < len) i++; // skip closing "
                continue;
            }

            if (c == '|') { tokens.Add(new QueryToken(TokenType.OrOp, "|")); i++; continue; }
            if (c == '<') { tokens.Add(new QueryToken(TokenType.OpenGroup, "<")); i++; continue; }
            if (c == '>') { tokens.Add(new QueryToken(TokenType.CloseGroup, ">")); i++; continue; }

            if (c == '!')
            {
                // ! at the start of a term is NOT; mid-term is literal
                tokens.Add(new QueryToken(TokenType.NotOp, "!"));
                i++;
                continue;
            }

            // Accumulate a word (until whitespace or operator)
            int wordStart = i;
            bool inFilterValue = false;
            bool valueHasContent = false;
            bool regexValueMode = false;
            bool emittedQuotedFilter = false;
            while (i < len)
            {
                char ch = input[i];
                if (regexValueMode)
                {
                    if (char.IsWhiteSpace(ch) || ch == '"')
                        break;
                    i++;
                    continue;
                }

                if (char.IsWhiteSpace(ch) || ch == '|' || ch == '"')
                    break;
                // < > 仅在 filter 值开头不断词（如 size:>1mb, dm:<=2024）
                // 值中已有内容后 > 仍为分组符（如 ext:txt> 的 >）
                if (ch is '<' or '>')
                {
                    if (!inFilterValue || valueHasContent)
                        break;
                }
                if (ch == ':' && !inFilterValue)
                {
                    string potentialPrefix = input[wordStart..i];
                    if (KnownFilters.Contains(potentialPrefix))
                    {
                        i++; // skip ':'
                        if (i < len && input[i] == '"' && QuotedValueFilters.Contains(potentialPrefix))
                        {
                            i++; // opening "
                            int v0 = i;
                            while (i < len && input[i] != '"') i++;
                            string qval = input[v0..i];
                            if (i < len) i++; // closing "
                            tokens.Add(new QueryToken(TokenType.Filter, qval, potentialPrefix));
                            emittedQuotedFilter = true;
                            break;
                        }

                        inFilterValue = true;
                        if (potentialPrefix.Equals("regex", StringComparison.OrdinalIgnoreCase))
                            regexValueMode = true;
                        continue;
                    }
                }
                else if (inFilterValue && ch is not (':' or '<' or '>' or '='))
                {
                    valueHasContent = true;
                }
                i++;
            }

            if (emittedQuotedFilter)
                continue;

            string word = input[wordStart..i];

            int colonIdx = word.IndexOf(':');
            if (colonIdx > 0)
            {
                string prefix = word[..colonIdx];
                if (KnownFilters.Contains(prefix))
                {
                    string value = word[(colonIdx + 1)..];
                    tokens.Add(new QueryToken(TokenType.Filter, value, prefix));
                    continue;
                }
            }

            tokens.Add(new QueryToken(TokenType.Term, word));
        }

        return tokens;
    }
}
