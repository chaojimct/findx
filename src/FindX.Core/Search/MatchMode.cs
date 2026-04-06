namespace FindX.Core.Search;

[Flags]
public enum MatchMode
{
    Prefix = 1,
    Pinyin = 2,
    Regex = 4,
    Fuzzy = 8,
    Exact = 16,
    All = Prefix | Pinyin | Fuzzy,
}
