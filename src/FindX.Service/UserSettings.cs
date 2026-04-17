using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;
using FindX.Core.Search;

namespace FindX.Service;

public sealed class UserSettings
{
    public bool PreferPinyinForAsciiQueries { get; set; } = true;

    /// <summary>
    /// 搜索结果是否从磁盘补全大小、修改时间。null 表示旧版 settings.json 未写该字段，按「开启」处理。
    /// </summary>
    public bool? HydrateSearchResultMetadata { get; set; }

    public SearchPreferences ToSearchPreferences() => new()
    {
        PreferPinyinForAsciiQueries = PreferPinyinForAsciiQueries,
        HydrateSearchResultMetadata = HydrateSearchResultMetadata != false,
    };
}

internal static class UserSettingsStore
{
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        WriteIndented = true,
    };

    public static UserSettings Load(string path)
    {
        try
        {
            if (!File.Exists(path))
                return new UserSettings();

            var json = File.ReadAllText(path);
            return JsonSerializer.Deserialize<UserSettings>(json, JsonOptions) ?? new UserSettings();
        }
        catch
        {
            return new UserSettings();
        }
    }

    public static void Save(string path, UserSettings settings)
    {
        var dir = Path.GetDirectoryName(path);
        if (!string.IsNullOrEmpty(dir))
            Directory.CreateDirectory(dir);
        File.WriteAllText(path, JsonSerializer.Serialize(settings, JsonOptions));
    }
}
