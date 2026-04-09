using System.Text.RegularExpressions;

namespace FindX.Core.Search;

/// <summary>
/// 解析结果。Root 为 AST 表达式树；Keywords 为从 AST 提取的正向搜索词（供索引前缀查找）。
/// 保留 IsRegex/RegexPattern 以兼容纯 regex: 查询的快速路径。
/// </summary>
public sealed class ParsedQuery
{
    public string RawQuery = "";
    public List<string> Keywords = new();
    public string? PathFilter;
    public string? ExtFilter;
    public Regex? RegexPattern;
    public bool IsRegex;
    public MatchMode Mode = MatchMode.All;

    public QueryNode? Root;
    public int? MaxCount;
    public bool HasFilters;
}

/// <summary>
/// Everything 兼容查询解析器。递归下降构建 AST 表达式树。
/// <para>语法：</para>
/// <code>
/// expr     = or_expr
/// or_expr  = and_expr ("|" and_expr)*
/// and_expr = unary (unary)*
/// unary    = "!" unary | atom
/// atom     = "&lt;" expr "&gt;" | QUOTED_STRING | FILTER | TERM
/// </code>
/// </summary>
// TODO(IbEverythingExt 可借): 显式匹配模式（仅拼音/仅字面/自动，如 ;py|;en 或全局设置）、小写/大写与字面匹配的约定、路径级通配 / 与 // 及文档与实现对齐。参见 Chaoses-Ib/IbEverythingExt README「搜索增强」。
public static class QueryParser
{
    public static ParsedQuery Parse(string input)
    {
        var q = new ParsedQuery { RawQuery = input.Trim() };
        if (string.IsNullOrEmpty(q.RawQuery)) return q;

        var tokens = QueryTokenizer.Tokenize(q.RawQuery);
        if (tokens.Count == 0) return q;

        // 解析器状态
        var state = new ParserState(tokens);
        var modifiers = new Modifiers();

        var root = ParseExpr(state, modifiers, q);

        if (root != null)
        {
            q.Root = root;
            var terms = new List<string>();
            root.CollectPositiveTerms(terms);
            q.Keywords = terms;
        }

        // 如果只有单个 regex: 没有其他节点，保留快速路径兼容
        if (q.Root is RegexNode rn)
        {
            q.IsRegex = true;
            q.RegexPattern = rn.CompiledRegex;
            q.Mode = MatchMode.Regex;
        }

        return q;
    }

    private sealed class ParserState
    {
        public readonly List<QueryToken> Tokens;
        public int Pos;
        public ParserState(List<QueryToken> tokens) => Tokens = tokens;
        public bool HasMore => Pos < Tokens.Count;
        public QueryToken Peek() => Tokens[Pos];
        public QueryToken Advance() => Tokens[Pos++];
    }

    private sealed class Modifiers
    {
        public bool CaseSensitive;
        public bool WholeWord;
    }

    // expr = or_expr
    private static QueryNode? ParseExpr(ParserState s, Modifiers m, ParsedQuery q) => ParseOrExpr(s, m, q);

    // or_expr = and_expr ("|" and_expr)*
    private static QueryNode? ParseOrExpr(ParserState s, Modifiers m, ParsedQuery q)
    {
        var first = ParseAndExpr(s, m, q);
        if (first == null) return null;

        List<QueryNode>? children = null;
        while (s.HasMore && s.Peek().Type == TokenType.OrOp)
        {
            s.Advance(); // consume |
            var next = ParseAndExpr(s, m, q);
            if (next == null) break;
            children ??= new List<QueryNode> { first };
            children.Add(next);
        }

        if (children != null)
            return new OrNode(children);
        return first;
    }

    // and_expr = unary (unary)*
    private static QueryNode? ParseAndExpr(ParserState s, Modifiers m, ParsedQuery q)
    {
        var first = ParseUnary(s, m, q);
        if (first == null) return null;

        List<QueryNode>? children = null;
        while (s.HasMore)
        {
            var tok = s.Peek();
            // Stop at | or > (group end)
            if (tok.Type == TokenType.OrOp || tok.Type == TokenType.CloseGroup)
                break;

            var next = ParseUnary(s, m, q);
            if (next == null) break;
            children ??= new List<QueryNode> { first };
            children.Add(next);
        }

        if (children != null)
            return new AndNode(children);
        return first;
    }

    // unary = "!" unary | atom
    private static QueryNode? ParseUnary(ParserState s, Modifiers m, ParsedQuery q)
    {
        if (!s.HasMore) return null;
        if (s.Peek().Type == TokenType.NotOp)
        {
            s.Advance();
            var child = ParseUnary(s, m, q);
            return child != null ? new NotNode(child) : null;
        }
        return ParseAtom(s, m, q);
    }

    // atom = "<" expr ">" | QUOTED | FILTER | TERM
    private static QueryNode? ParseAtom(ParserState s, Modifiers m, ParsedQuery q)
    {
        while (s.HasMore)
        {
            var tok = s.Peek();

            if (tok.Type == TokenType.OpenGroup)
            {
                s.Advance();
                var inner = ParseExpr(s, m, q);
                if (s.HasMore && s.Peek().Type == TokenType.CloseGroup)
                    s.Advance();
                return inner;
            }

            if (tok.Type == TokenType.QuotedString)
            {
                s.Advance();
                return new TermNode(tok.Value, isExact: true, caseSensitive: m.CaseSensitive);
            }

            if (tok.Type == TokenType.Filter)
            {
                s.Advance();
                var node = ParseFilter(tok, m, q);
                if (node != null) return node;
                // 修饰符 filter（case:/nocase:/ww:/count:）返回 null，跳过继续
                continue;
            }

            if (tok.Type == TokenType.Term)
            {
                s.Advance();
                return new TermNode(tok.Value, wholeWord: m.WholeWord, caseSensitive: m.CaseSensitive);
            }

            // 遇到无法处理的 token（如孤立的 CloseGroup/OrOp），退出
            break;
        }
        return null;
    }

    private static QueryNode? ParseFilter(QueryToken tok, Modifiers m, ParsedQuery q)
    {
        string prefix = tok.FilterPrefix!.ToLowerInvariant();
        string value = tok.Value;

        switch (prefix)
        {
            case "file":
                q.HasFilters = true;
                return FilterNode.FileOnly();

            case "folder":
                q.HasFilters = true;
                return FilterNode.FolderOnly();

            case "root":
                q.HasFilters = true;
                return FilterNode.RootOnly();

            case "ext":
                q.HasFilters = true;
                q.ExtFilter ??= value.TrimStart('.');
                return FilterNode.ParseExtension(value);

            case "parent" or "path":
                q.HasFilters = true;
                q.PathFilter ??= value.Trim('"');
                return FilterNode.ParsePath(value);

            case "nopath":
                q.HasFilters = true;
                return FilterNode.ParsePath(value, negate: true);

            case "size":
                q.HasFilters = true;
                return FilterNode.ParseSize(value);

            case "dm" or "datemodified":
                q.HasFilters = true;
                return FilterNode.ParseDateModified(value);

            case "dc" or "datecreated":
                q.HasFilters = true;
                return FilterNode.ParseDateCreated(value);

            case "da" or "dateaccessed":
                q.HasFilters = true;
                return FilterNode.ParseDateAccessed(value);

            case "len":
                q.HasFilters = true;
                return FilterNode.ParseNameLength(value);

            case "depth" or "parents":
                q.HasFilters = true;
                return FilterNode.ParseDepth(value);

            case "attrib" or "attributes":
                q.HasFilters = true;
                return FilterNode.ParseAttributes(value);

            case "startwith":
                q.HasFilters = true;
                return FilterNode.StartWith(value);

            case "endwith":
                q.HasFilters = true;
                return FilterNode.EndWith(value);

            case "case":
                m.CaseSensitive = true;
                return null;

            case "nocase":
                m.CaseSensitive = false;
                return null;

            case "wholeword" or "ww":
                m.WholeWord = true;
                return null;

            case "count":
                if (int.TryParse(value, out var cnt))
                    q.MaxCount = cnt;
                return null;

            case "regex":
                try
                {
                    var regex = new Regex(value, RegexOptions.IgnoreCase | RegexOptions.Compiled);
                    q.IsRegex = true;
                    q.RegexPattern = regex;
                    q.Mode = MatchMode.Regex;
                    return new RegexNode(regex);
                }
                catch
                {
                    return new TermNode(value);
                }

            case "audio":
                q.HasFilters = true;
                return FilterNode.ParseExtension("mp3;wav;flac;aac;ogg;wma;m4a;opus");

            case "video":
                q.HasFilters = true;
                return FilterNode.ParseExtension("mp4;avi;mkv;mov;wmv;flv;webm;m4v;ts");

            case "doc":
                q.HasFilters = true;
                return FilterNode.ParseExtension("doc;docx;pdf;xls;xlsx;ppt;pptx;txt;rtf;odt;csv;md");

            case "exe":
                q.HasFilters = true;
                return FilterNode.ParseExtension("exe;msi;bat;cmd;com;scr;ps1");

            case "zip":
                q.HasFilters = true;
                return FilterNode.ParseExtension("zip;rar;7z;tar;gz;bz2;xz;zst;cab;iso");

            case "pic":
                q.HasFilters = true;
                return FilterNode.ParseExtension("jpg;jpeg;png;gif;bmp;svg;webp;ico;tiff;psd;raw");

            case "type":
                q.HasFilters = true;
                return ResolveTypeMacro(value);

            case "shell":
                q.HasFilters = true;
                return ResolveShellFolder(value);

            default:
                return new TermNode($"{prefix}:{value}");
        }
    }

    private static readonly Dictionary<string, string> TypeMacroExtensions = new(StringComparer.OrdinalIgnoreCase)
    {
        ["audio"] = "mp3;wav;flac;aac;ogg;wma;m4a;opus",
        ["video"] = "mp4;avi;mkv;mov;wmv;flv;webm;m4v;ts",
        ["doc"] = "doc;docx;pdf;xls;xlsx;ppt;pptx;txt;rtf;odt;csv;md",
        ["exe"] = "exe;msi;bat;cmd;com;scr;ps1",
        ["zip"] = "zip;rar;7z;tar;gz;bz2;xz;zst;cab;iso",
        ["pic"] = "jpg;jpeg;png;gif;bmp;svg;webp;ico;tiff;psd;raw",
        ["image"] = "jpg;jpeg;png;gif;bmp;svg;webp;ico;tiff;psd;raw",
    };

    private static QueryNode ResolveTypeMacro(string value)
    {
        if (TypeMacroExtensions.TryGetValue(value, out var exts))
            return FilterNode.ParseExtension(exts);
        return FilterNode.ParseExtension(value);
    }

    private static readonly Dictionary<string, Environment.SpecialFolder> ShellFolderMap = new(StringComparer.OrdinalIgnoreCase)
    {
        ["desktop"] = Environment.SpecialFolder.DesktopDirectory,
        ["documents"] = Environment.SpecialFolder.MyDocuments,
        ["music"] = Environment.SpecialFolder.MyMusic,
        ["pictures"] = Environment.SpecialFolder.MyPictures,
        ["videos"] = Environment.SpecialFolder.MyVideos,
        ["appdata"] = Environment.SpecialFolder.ApplicationData,
        ["localappdata"] = Environment.SpecialFolder.LocalApplicationData,
        ["startup"] = Environment.SpecialFolder.Startup,
        ["programs"] = Environment.SpecialFolder.Programs,
        ["favorites"] = Environment.SpecialFolder.Favorites,
        ["recent"] = Environment.SpecialFolder.Recent,
        ["templates"] = Environment.SpecialFolder.Templates,
        ["fonts"] = Environment.SpecialFolder.Fonts,
        ["windows"] = Environment.SpecialFolder.Windows,
        ["system"] = Environment.SpecialFolder.System,
        ["programfiles"] = Environment.SpecialFolder.ProgramFiles,
        ["profile"] = Environment.SpecialFolder.UserProfile,
    };

    private static QueryNode ResolveShellFolder(string value)
    {
        if (value.Equals("downloads", StringComparison.OrdinalIgnoreCase))
        {
            var downloads = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), "Downloads");
            return FilterNode.ParsePath(downloads);
        }

        if (ShellFolderMap.TryGetValue(value, out var folder))
        {
            var path = Environment.GetFolderPath(folder);
            if (!string.IsNullOrEmpty(path))
                return FilterNode.ParsePath(path);
        }

        return FilterNode.ParsePath(value);
    }
}
