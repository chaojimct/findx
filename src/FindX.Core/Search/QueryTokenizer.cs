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
    private static readonly HashSet<string> KnownFilters = new(StringComparer.OrdinalIgnoreCase)
    {
        "ext", "path", "nopath", "parent",
        "file", "folder",
        "size",
        "dm", "datemodified",
        // TODO: "dc", "datecreated" — 创建时间，需 Rust 引擎扩展 FileEntry 存储 CreationTimeTicks
        // TODO: "da", "dateaccessed" — 访问时间，需 Rust 引擎扩展 FileEntry 存储 AccessTimeTicks
        "len",
        "depth", "parents",
        "root",
        // TODO: "empty" — 空文件夹判断，需遍历子树检查是否有子条目
        "attrib", "attributes",
        "case", "nocase",
        "wholeword", "ww",
        "startwith", "endwith",
        "regex",
        "count",
        // TODO: "content" — 文件内容搜索，需读取文件 IO（性能敏感）
        // TODO: "type" — Windows 文件类型名（如 "Text Document"），需查询注册表
        // TODO: "dupe", "sizedupe", "namedupe" — 重复文件检测，需全索引扫描+分组
        // TODO: "child", "childcount", "childfile", "childfolder" — 子项计数，需遍历子树
        // TODO: "runcount", "daterun" — 运行历史，需实现运行历史跟踪
        // TODO: "shell" — Shell 已知文件夹（桌面/文档等），需 KNOWNFOLDERID 映射
        // TODO: "audio", "video", "doc", "exe", "zip", "pic" 等宏 — ext 列表展开，低难度可快速添加
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
            while (i < len)
            {
                char ch = input[i];
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
                        inFilterValue = true;
                }
                else if (inFilterValue && ch is not (':' or '<' or '>' or '='))
                {
                    valueHasContent = true;
                }
                i++;
            }

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
