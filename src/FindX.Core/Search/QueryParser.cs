using System.Text.RegularExpressions;

namespace FindX.Core.Search;

/// <summary>
/// 查询语法解析器。支持：
/// - 简单文本搜索
/// - 路径过滤 (parent:C:\Users)
/// - 扩展名过滤 (ext:cs)
/// - 正则表达式 (regex:pattern)
/// - 布尔组合 (AND/OR 通过空格和 | 分隔)
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
}

public static class QueryParser
{
    public static ParsedQuery Parse(string input)
    {
        var q = new ParsedQuery { RawQuery = input.Trim() };
        if (string.IsNullOrEmpty(q.RawQuery)) return q;

        var parts = q.RawQuery.Split(' ', StringSplitOptions.RemoveEmptyEntries);
        foreach (var part in parts)
        {
            if (part.StartsWith("parent:", StringComparison.OrdinalIgnoreCase))
            {
                q.PathFilter = part[7..].Trim('"');
            }
            else if (part.StartsWith("ext:", StringComparison.OrdinalIgnoreCase))
            {
                q.ExtFilter = part[4..].TrimStart('.');
            }
            else if (part.StartsWith("regex:", StringComparison.OrdinalIgnoreCase))
            {
                try
                {
                    q.RegexPattern = new Regex(part[6..], RegexOptions.IgnoreCase | RegexOptions.Compiled);
                    q.IsRegex = true;
                    q.Mode = MatchMode.Regex;
                }
                catch { q.Keywords.Add(part); }
            }
            else
            {
                q.Keywords.Add(part);
            }
        }

        return q;
    }
}
