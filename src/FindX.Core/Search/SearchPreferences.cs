namespace FindX.Core.Search;

public sealed class SearchPreferences
{
    public bool PreferPinyinForAsciiQueries { get; set; } = true;

    public SearchPreferences Clone() => new()
    {
        PreferPinyinForAsciiQueries = PreferPinyinForAsciiQueries,
    };
}
