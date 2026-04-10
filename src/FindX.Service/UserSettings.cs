using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;
using FindX.Core.Search;

namespace FindX.Service;

public sealed class UserSettings
{
    public bool PreferPinyinForAsciiQueries { get; set; } = true;

    public SearchPreferences ToSearchPreferences() => new()
    {
        PreferPinyinForAsciiQueries = PreferPinyinForAsciiQueries,
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
